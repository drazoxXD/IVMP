//! Injecting the 32-bit hooks DLL into 32-bit GTAIV.exe from the 64-bit launcher.
//!
//! A 64-bit process can create threads in a WoW64 process, but `LoadLibraryW` in
//! our own address space is the 64-bit one. So we locate the target's 32-bit
//! `kernel32.dll`, walk its export table through `ReadProcessMemory` to find the
//! 32-bit `LoadLibraryW`, and start the remote thread there. No 32-bit helper exe needed.

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Diagnostics::Debug::{ReadProcessMemory, WriteProcessMemory};
use windows::Win32::System::Memory::{MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAllocEx, VirtualFreeEx};
use windows::Win32::System::Threading::{
    CreateRemoteThread, GetExitCodeThread, IsWow64Process, LPTHREAD_START_ROUTINE, OpenProcess, PROCESS_CREATE_THREAD,
    PROCESS_QUERY_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_VM_OPERATION, PROCESS_VM_READ, PROCESS_VM_WRITE,
    WaitForSingleObject,
};

use crate::process::{self, ProcessHandle};

pub type Result<T> = std::result::Result<T, String>;

pub fn open_game(pid: u32) -> Result<ProcessHandle> {
    let access = PROCESS_CREATE_THREAD
        | PROCESS_QUERY_INFORMATION
        | PROCESS_VM_OPERATION
        | PROCESS_VM_READ
        | PROCESS_VM_WRITE
        | PROCESS_SYNCHRONIZE;
    let handle = unsafe { OpenProcess(access, false, pid) }.map_err(|e| {
        format!("Could not open the game process ({e}). If the game runs as administrator, run the launcher as administrator too.")
    })?;
    let handle = ProcessHandle(handle);

    let mut wow64 = Default::default();
    unsafe { IsWow64Process(handle.0, &mut wow64) }.map_err(|e| format!("IsWow64Process failed: {e}"))?;
    if !wow64.as_bool() {
        return Err("The game process is not 32-bit, this is not the GTA IV build IVMP expects.".into());
    }
    Ok(handle)
}

/// Waits until the WoW64 loader has mapped the 32-bit kernel32 and returns its base.
pub fn wait_for_kernel32(process: HANDLE, timeout: Duration) -> Result<u32> {
    let start = Instant::now();
    loop {
        if let Ok(modules) = process::modules_32(process) {
            if let Some(base) = process::find_module(&modules, "kernel32.dll") {
                return Ok(base);
            }
        }
        if start.elapsed() > timeout {
            return Err("Timed out waiting for the game to finish starting up.".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Where the copy protection's unpacking can be seen: the game's window procedure, which is
/// encrypted on disk (GTAIV.exe 1.2.0.59, RVA of 0x420AC1). Injecting while the protection is still
/// unpacking made the game crash at random, so we wait until these bytes are decrypted.
const UNPACK_MARKER_RVA: u32 = 0x0002_0AC1;
const UNPACK_MARKER: [Option<u8>; 8] = [Some(0x3B), Some(0x35), None, None, None, None, Some(0x74), Some(0x24)];

/// Waits until the game code is unpacked. Returns how long that took.
pub fn wait_for_unpack(process: HANDLE, game_exe: &str, timeout: Duration) -> Result<Duration> {
    let start = Instant::now();
    loop {
        let base = process::modules_32(process).ok().and_then(|m| process::find_module(&m, game_exe));
        if let Some(base) = base {
            let mut bytes = [0u8; UNPACK_MARKER.len()];
            let ok = unsafe {
                ReadProcessMemory(
                    process,
                    (base + UNPACK_MARKER_RVA) as usize as *const c_void,
                    bytes.as_mut_ptr() as *mut c_void,
                    bytes.len(),
                    None,
                )
            }
            .is_ok();
            if ok && bytes.iter().zip(&UNPACK_MARKER).all(|(b, m)| m.is_none_or(|m| *b == m)) {
                return Ok(start.elapsed());
            }
        }
        if start.elapsed() > timeout {
            return Err("Timed out waiting for the game code to unpack.".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub fn is_loaded(process: HANDLE, dll_name: &str) -> bool {
    process::modules_32(process)
        .map(|m| process::find_module(&m, dll_name).is_some())
        .unwrap_or(false)
}

/// Loads `dll_path` into the target with `LoadLibraryW` and returns the module base.
pub fn inject(process: HANDLE, kernel32_base: u32, dll_path: &Path) -> Result<u32> {
    let load_library = remote_export_32(process, kernel32_base, "LoadLibraryW")?;

    let wide: Vec<u16> = dll_path.as_os_str().encode_wide().chain(Some(0)).collect();
    let bytes = wide.len() * 2;

    unsafe {
        let remote = VirtualAllocEx(process, None, bytes, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
        if remote.is_null() {
            return Err(format!("VirtualAllocEx failed: {}", windows::core::Error::from_thread()));
        }
        let free = || {
            let _ = VirtualFreeEx(process, remote, 0, MEM_RELEASE);
        };

        if let Err(e) = WriteProcessMemory(process, remote, wide.as_ptr() as *const c_void, bytes, None) {
            free();
            return Err(format!("WriteProcessMemory failed: {e}"));
        }

        // The thread starts at a 32-bit address in the target; the OS runs it in WoW64 mode.
        let start: LPTHREAD_START_ROUTINE = std::mem::transmute(load_library as usize);
        let thread = match CreateRemoteThread(process, None, 0, start, Some(remote), 0, None) {
            Ok(t) => ProcessHandle(t),
            Err(e) => {
                free();
                return Err(format!("CreateRemoteThread failed: {e}"));
            }
        };

        if WaitForSingleObject(thread.0, 15_000) != WAIT_OBJECT_0 {
            // Leave the string allocated: the thread may still read it.
            return Err("LoadLibraryW did not return in time.".into());
        }
        let mut exit_code = 0u32;
        let _ = GetExitCodeThread(thread.0, &mut exit_code);
        free();

        // On a 32-bit target the thread exit code is the full HMODULE.
        if exit_code == 0 {
            return Err(format!(
                "LoadLibraryW failed inside the game. Check that {} exists and is a 32-bit build.",
                dll_path.display()
            ));
        }
        Ok(exit_code)
    }
}

fn read<T: Copy + Default>(process: HANDLE, address: u32) -> Result<T> {
    let mut value = T::default();
    unsafe {
        ReadProcessMemory(
            process,
            address as usize as *const c_void,
            &mut value as *mut T as *mut c_void,
            size_of::<T>(),
            None,
        )
    }
    .map_err(|e| format!("ReadProcessMemory at {address:#010x} failed: {e}"))?;
    Ok(value)
}

fn read_cstr(process: HANDLE, address: u32) -> Result<String> {
    let mut buf = [0u8; 64];
    unsafe {
        ReadProcessMemory(process, address as usize as *const c_void, buf.as_mut_ptr() as *mut c_void, buf.len(), None)
    }
    .map_err(|e| format!("ReadProcessMemory at {address:#010x} failed: {e}"))?;
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    Ok(String::from_utf8_lossy(&buf[..len]).into_owned())
}

/// Resolves an export of a PE32 module mapped in the target process.
fn remote_export_32(process: HANDLE, base: u32, name: &str) -> Result<u32> {
    if read::<u16>(process, base)? != 0x5A4D {
        return Err("kernel32 has no MZ header".into());
    }
    let nt = base + read::<u32>(process, base + 0x3C)?;
    if read::<u32>(process, nt)? != 0x0000_4550 {
        return Err("kernel32 has no PE header".into());
    }
    let optional = nt + 24;
    if read::<u16>(process, optional)? != 0x10B {
        return Err("kernel32 in the game is not PE32".into());
    }
    // PE32 optional header: data directories start at +96, export directory is entry 0.
    let export_rva = read::<u32>(process, optional + 96)?;
    let export_size = read::<u32>(process, optional + 100)?;
    let export = base + export_rva;

    let number_of_names = read::<u32>(process, export + 0x18)?;
    let functions = base + read::<u32>(process, export + 0x1C)?;
    let names = base + read::<u32>(process, export + 0x20)?;
    let ordinals = base + read::<u32>(process, export + 0x24)?;

    for i in 0..number_of_names {
        let name_rva = read::<u32>(process, names + i * 4)?;
        if read_cstr(process, base + name_rva)? != name {
            continue;
        }
        let ordinal = read::<u16>(process, ordinals + i * 2)? as u32;
        let rva = read::<u32>(process, functions + ordinal * 4)?;
        if rva >= export_rva && rva < export_rva + export_size {
            return Err(format!("{name} is a forwarded export, cannot call it directly"));
        }
        return Ok(base + rva);
    }
    Err(format!("{name} not found in the game's kernel32"))
}
