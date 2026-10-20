//! Crash logger: a vectored exception handler that writes the faulting address, registers and
//! stack to `IVMP_Client.log` before the game's own crash handler runs. Addresses inside
//! GTAIV.exe are also printed as Ghidra addresses (image base 0x400000).

use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};

use windows::Win32::Foundation::{EXCEPTION_ACCESS_VIOLATION, EXCEPTION_ILLEGAL_INSTRUCTION, EXCEPTION_INT_DIVIDE_BY_ZERO, EXCEPTION_PRIV_INSTRUCTION, EXCEPTION_STACK_OVERFLOW, NTSTATUS};
use windows::Win32::System::Diagnostics::Debug::{AddVectoredExceptionHandler, EXCEPTION_POINTERS};
use windows::Win32::System::Memory::{MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_GUARD, PAGE_NOACCESS, VirtualQuery};

use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, GetModuleFileNameW, GetModuleHandleExW,
};
use windows::core::PCWSTR;

use crate::{log, memory};
use crate::memory::GHIDRA_BASE;

/// First-chance exceptions can be handled by the game, so only log the first few.
const MAX_REPORTS: u32 = 8;
const EXCEPTION_CONTINUE_SEARCH: i32 = 0;

static REPORTS: AtomicU32 = AtomicU32::new(0);

pub fn install() {
    unsafe {
        if AddVectoredExceptionHandler(1, Some(handler)).is_null() {
            log!("crash: AddVectoredExceptionHandler failed");
        }
    }
}

fn is_crash(code: NTSTATUS) -> bool {
    [
        EXCEPTION_ACCESS_VIOLATION,
        EXCEPTION_ILLEGAL_INSTRUCTION,
        EXCEPTION_PRIV_INSTRUCTION,
        EXCEPTION_INT_DIVIDE_BY_ZERO,
        EXCEPTION_STACK_OVERFLOW,
    ]
    .contains(&code)
}

/// `addr (ghidra 0x...)` for addresses inside the game image, plain hex otherwise.
pub fn describe(addr: usize) -> String {
    let image = memory::game_image();
    let base = image.as_ptr() as usize;
    if (base..base + image.len()).contains(&addr) {
        return format!("{addr:#010x} (ghidra {:#010x})", addr - base + GHIDRA_BASE);
    }
    // Some other module: name + offset, so DLL code (ours included) can be identified.
    let mut module = HMODULE::default();
    let flags = GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT;
    if unsafe { GetModuleHandleExW(flags, PCWSTR(addr as *const u16), &mut module) }.is_ok() {
        let mut name = [0u16; 260];
        let len = unsafe { GetModuleFileNameW(Some(module), &mut name) } as usize;
        let name = String::from_utf16_lossy(&name[..len]);
        let short = name.rsplit('\\').next().unwrap_or(&name).to_string();
        return format!("{addr:#010x} ({short}+{:#x})", addr - module.0 as usize);
    }
    format!("{addr:#010x} (no module)")
}

/// Protection of the page at `addr`, for execute faults.
fn page_protection(addr: usize) -> String {
    let mut info = MEMORY_BASIC_INFORMATION::default();
    if unsafe { VirtualQuery(Some(addr as *const c_void), &mut info, size_of::<MEMORY_BASIC_INFORMATION>()) } == 0 {
        return "unknown".into();
    }
    format!("protect {:#x}, state {:#x}, alloc base {:#x}", info.Protect.0, info.State.0, info.AllocationBase as usize)
}

fn readable(addr: usize, len: usize) -> bool {
    let mut info = MEMORY_BASIC_INFORMATION::default();
    let got = unsafe { VirtualQuery(Some(addr as *const c_void), &mut info, size_of::<MEMORY_BASIC_INFORMATION>()) };
    got != 0
        && info.State == MEM_COMMIT
        && (info.Protect & (PAGE_NOACCESS | PAGE_GUARD)).0 == 0
        && addr + len <= info.BaseAddress as usize + info.RegionSize
}

unsafe extern "system" fn handler(info: *mut EXCEPTION_POINTERS) -> i32 {
    let (record, context) = unsafe { (&*(*info).ExceptionRecord, &*(*info).ContextRecord) };
    if !is_crash(record.ExceptionCode) || REPORTS.fetch_add(1, Ordering::Relaxed) >= MAX_REPORTS {
        return EXCEPTION_CONTINUE_SEARCH;
    }
    let code = record.ExceptionCode.0 as u32;
    log!("crash: exception {code:#010x} at {}", describe(record.ExceptionAddress as usize));
    if record.ExceptionCode == EXCEPTION_ACCESS_VIOLATION && record.NumberParameters >= 2 {
        let kind = match record.ExceptionInformation[0] {
            0 => "read",
            1 => "write",
            8 => "execute",
            _ => "access",
        };
        log!("crash:   {kind} of {}", describe(record.ExceptionInformation[1]));
        if kind == "execute" {
            log!("crash:   page {}", page_protection(record.ExceptionInformation[1]));
        }
    }
    log!(
        "crash:   eax={:08x} ebx={:08x} ecx={:08x} edx={:08x} esi={:08x} edi={:08x} ebp={:08x} esp={:08x}",
        context.Eax, context.Ebx, context.Ecx, context.Edx, context.Esi, context.Edi, context.Ebp, context.Esp
    );
    // Return addresses on the stack that point into the game: a rough call stack.
    let esp = context.Esp as usize;
    let mut found = 0;
    for slot in 0..256 {
        let at = esp + slot * 4;
        if !readable(at, 4) {
            break;
        }
        let value = unsafe { *(at as *const u32) } as usize;
        let image = memory::game_image();
        let base = image.as_ptr() as usize;
        if (base + 0x1000..base + 0xA73000).contains(&value) {
            log!("crash:   [esp+{:#05x}] {}", slot * 4, describe(value));
            found += 1;
            if found == 24 {
                break;
            }
        }
    }
    EXCEPTION_CONTINUE_SEARCH
}
