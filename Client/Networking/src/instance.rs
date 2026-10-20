//! Running two copies of the game on one PC, like FiveM's `-cl2`.
//!
//! * WinMain (0x401DA0) creates the mutex `GTANY-088FA840-...` and stops with fatal error
//!   "AIIR" when it already exists. The new copy checks before we can be injected into it, so
//!   every copy we are in closes its own handle to that mutex instead.
//! * The engine binds its game socket to port 6672. The second client (`-cl2` on its command
//!   line) binds 6673, through an import-table hook on ws2_32 `bind`, which also remembers the
//!   game socket for the tunnel (net.rs).

use std::ffi::c_void;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use windows::core::s;

use crate::{log, memory};

const SINGLE_INSTANCE_MUTEX: &str = "GTANY-088FA840-B10D-11D3-BC36-006067709674";
/// Import slot of ws2_32 `bind` (ordinal 2).
const BIND_IMPORT: usize = 0x00E7_34B0;
const GAME_PORT: u16 = 6672;
const SECOND_CLIENT_PORT: u16 = 6673;
/// Handle values to look at; the game has a few thousand handles at most.
const MAX_HANDLE: usize = 0x2_0000;

const OBJECT_NAME_INFORMATION: u32 = 1;
const OBJECT_TYPE_INFORMATION: u32 = 2;

type NtQueryObject = unsafe extern "system" fn(HANDLE, u32, *mut c_void, u32, *mut u32) -> i32;
type Bind = unsafe extern "system" fn(usize, *const SockAddrIn, i32) -> i32;

static ORIGINAL_BIND: AtomicUsize = AtomicUsize::new(0);
/// The engine's game socket (the one bound to the game port), 0 until bound.
static GAME_SOCKET: AtomicUsize = AtomicUsize::new(0);

pub fn game_socket() -> usize {
    GAME_SOCKET.load(Ordering::SeqCst)
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SockAddrIn {
    pub family: u16,
    /// Network byte order.
    pub port: u16,
    /// Network byte order.
    pub addr: u32,
    pub zero: [u8; 8],
}

/// The UNICODE_STRING at the start of both object information classes.
#[repr(C)]
struct UnicodeString {
    length: u16,
    max_length: u16,
    buffer: *const u16,
}

pub fn second_client() -> bool {
    static SECOND: OnceLock<bool> = OnceLock::new();
    *SECOND.get_or_init(|| {
        let line = unsafe { windows::Win32::System::Environment::GetCommandLineW().to_string() }.unwrap_or_default();
        line.split_whitespace().any(|arg| arg.eq_ignore_ascii_case(ivmp_shared::SECOND_CLIENT_ARG))
    })
}

/// Name or type of a handle through NtQueryObject, `None` if it can't be read.
unsafe fn query(nt_query_object: NtQueryObject, handle: HANDLE, class: u32) -> Option<String> {
    // u64 keeps the buffer aligned for the UNICODE_STRING header.
    let mut buf = [0u64; 0x200];
    let size = size_of_val(&buf) as u32;
    let status = unsafe { nt_query_object(handle, class, buf.as_mut_ptr() as *mut c_void, size, std::ptr::null_mut()) };
    if status < 0 {
        return None;
    }
    let text = unsafe { &*(buf.as_ptr() as *const UnicodeString) };
    if text.buffer.is_null() {
        return Some(String::new());
    }
    let chars = unsafe { std::slice::from_raw_parts(text.buffer, text.length as usize / 2) };
    Some(String::from_utf16_lossy(chars))
}

/// Closes this process's handle to the single-instance mutex, so another copy can start.
pub fn release_single_instance_mutex() {
    let nt_query_object: NtQueryObject = unsafe {
        let Ok(ntdll) = GetModuleHandleA(s!("ntdll.dll")) else { return };
        let Some(function) = GetProcAddress(ntdll, s!("NtQueryObject")) else { return };
        std::mem::transmute(function)
    };
    for value in (4..MAX_HANDLE).step_by(4) {
        let handle = HANDLE(value as *mut c_void);
        unsafe {
            // Only ask mutexes for their name: a name query can block on some pipes and files.
            if query(nt_query_object, handle, OBJECT_TYPE_INFORMATION).as_deref() != Some("Mutant") {
                continue;
            }
            let Some(name) = query(nt_query_object, handle, OBJECT_NAME_INFORMATION) else { continue };
            if name.ends_with(SINGLE_INSTANCE_MUTEX) {
                let _ = CloseHandle(handle);
                log!("instance: released the single-instance mutex (handle {value:#x}), another copy can start");
                return;
            }
        }
    }
    log!("instance: single-instance mutex handle not found");
}

unsafe extern "system" fn bind(socket: usize, address: *const SockAddrIn, len: i32) -> i32 {
    let original: Bind = unsafe { std::mem::transmute(ORIGINAL_BIND.load(Ordering::SeqCst)) };
    unsafe {
        if !address.is_null() && len as usize >= size_of::<SockAddrIn>() && u16::from_be((*address).port) == GAME_PORT {
            GAME_SOCKET.store(socket, Ordering::SeqCst);
            log!("instance: game socket is {socket:#x}");
            if !second_client() {
                return original(socket, address, len);
            }
            let mut moved = *address;
            moved.port = SECOND_CLIENT_PORT.to_be();
            log!("instance: game socket bound to {SECOND_CLIENT_PORT} instead of {GAME_PORT}");
            return original(socket, &moved, len);
        }
        original(socket, address, len)
    }
}

/// Remembers the game socket; on the second client also moves it to its own port.
pub fn install_bind_hook() {
    let slot = memory::from_ghidra(BIND_IMPORT);
    let original = unsafe { std::ptr::read_unaligned(slot as *const usize) };
    ORIGINAL_BIND.store(original, Ordering::SeqCst);
    match unsafe { memory::patch(slot, &(bind as usize as u32).to_le_bytes()) } {
        Ok(()) => log!("instance: bind hooked (import slot {slot:#x})"),
        Err(e) => log!("instance: bind hook failed: {e}"),
    }
}
