//! GTAIV.exe image access: bounds, IDA-style pattern scanning and code patching.

use std::ffi::c_void;

use windows::Win32::System::Diagnostics::Debug::{FlushInstructionCache, ReadProcessMemory};
use windows::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleW};
use windows::Win32::System::Memory::{
    MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_EXECUTE_READWRITE, PAGE_PROTECTION_FLAGS, VirtualProtect, VirtualQuery,
};
use windows::Win32::System::ProcessStatus::{GetModuleInformation, MODULEINFO};
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VS_FIXEDFILEINFO, VerQueryValueW};
use windows::core::{PCWSTR, w};

/// The game executable's mapped image.
pub fn game_image() -> &'static [u8] {
    unsafe {
        let module = GetModuleHandleW(PCWSTR::null()).expect("game module");
        let mut info = MODULEINFO::default();
        GetModuleInformation(GetCurrentProcess(), module, &mut info, size_of::<MODULEINFO>() as u32)
            .expect("GetModuleInformation");
        std::slice::from_raw_parts(info.lpBaseOfDll as *const u8, info.SizeOfImage as usize)
    }
}

/// Image base the Ghidra project uses; the game is relocated at runtime.
pub const GHIDRA_BASE: usize = 0x0040_0000;

/// Runtime address of a Ghidra address (GTAIV.exe 1.2.0.59).
pub fn from_ghidra(address: usize) -> usize {
    address - GHIDRA_BASE + game_image().as_ptr() as usize
}

/// Points the `call rel32` at `site` to `target` and returns the old target.
pub unsafe fn redirect_call(site: usize, target: usize) -> windows::core::Result<usize> {
    unsafe {
        let old_rel = std::ptr::read_unaligned((site + 1) as *const i32);
        let old_target = (site as isize + 5 + old_rel as isize) as usize;
        let rel = (target as isize - (site as isize + 5)) as i32;
        patch(site + 1, &rel.to_le_bytes())?;
        Ok(old_target)
    }
}

/// Parses `"55 8B EC ?? 0F"` into bytes with `None` wildcards.
pub fn parse_pattern(pattern: &str) -> Option<Vec<Option<u8>>> {
    pattern
        .split_whitespace()
        .map(|t| match t {
            "?" | "??" => Some(None),
            hex => u8::from_str_radix(hex, 16).ok().map(Some),
        })
        .collect()
}

/// The game's code section (`.text`). Only this is scanned: the rest of the image holds the
/// copy protection (`.rkstr`, `.tbm`), whose pages must not be touched.
pub fn code_section() -> &'static [u8] {
    let image = game_image();
    let u16_at = |at: usize| u16::from_le_bytes([image[at], image[at + 1]]) as usize;
    let u32_at = |at: usize| u32::from_le_bytes([image[at], image[at + 1], image[at + 2], image[at + 3]]) as usize;
    let pe = u32_at(0x3C);
    let sections = pe + 24 + u16_at(pe + 20);
    for i in 0..u16_at(pe + 6) {
        let header = sections + i * 40;
        if &image[header..header + 5] == b".text" {
            let (size, rva) = (u32_at(header + 8), u32_at(header + 12));
            return &image[rva..rva + size];
        }
    }
    image
}

/// A copy of the memory in `range`, as runs of `(address, bytes)`.
///
/// Read through `ReadProcessMemory` page by page, so pages we may not read (no-access, guard,
/// execute-only: the copy protection flips some code pages while it decrypts them) are skipped
/// instead of faulting. Touching such a page directly once killed the game.
pub fn snapshot(range: &[u8]) -> Vec<(usize, Vec<u8>)> {
    const PAGE: usize = 0x1000;
    let (start, end) = (range.as_ptr() as usize, range.as_ptr() as usize + range.len());
    let mut runs: Vec<(usize, Vec<u8>)> = Vec::new();
    let mut page = [0u8; PAGE];
    let mut at = start;
    while at < end {
        // Only plainly readable regions: never touch guard, no-access or execute-only pages.
        let mut info = MEMORY_BASIC_INFORMATION::default();
        if unsafe { VirtualQuery(Some(at as *const c_void), &mut info, size_of::<MEMORY_BASIC_INFORMATION>()) } == 0 {
            break;
        }
        let region_end = (info.BaseAddress as usize + info.RegionSize).min(end);
        if !is_readable(&info) {
            at = region_end;
            continue;
        }
        while at < region_end {
            let len = (PAGE - at % PAGE).min(region_end - at);
            if read(at, &mut page[..len]) {
                match runs.last_mut() {
                    Some((run_start, bytes)) if *run_start + bytes.len() == at => bytes.extend_from_slice(&page[..len]),
                    _ => runs.push((at, page[..len].to_vec())),
                }
            }
            at += len;
        }
    }
    runs
}

fn is_readable(info: &MEMORY_BASIC_INFORMATION) -> bool {
    const READABLE: u32 = 0x02 | 0x04 | 0x08 | 0x20 | 0x40 | 0x80;
    info.State == MEM_COMMIT && info.Protect.0 & READABLE != 0 && info.Protect.0 & (0x100 | 0x01) == 0
}

/// Copies memory at `address` into `out` without faulting; false if any of it isn't readable.
pub fn read(address: usize, out: &mut [u8]) -> bool {
    let mut info = MEMORY_BASIC_INFORMATION::default();
    if unsafe { VirtualQuery(Some(address as *const c_void), &mut info, size_of::<MEMORY_BASIC_INFORMATION>()) } == 0
        || !is_readable(&info)
    {
        return false;
    }
    let mut read = 0usize;
    unsafe {
        ReadProcessMemory(GetCurrentProcess(), address as *const c_void, out.as_mut_ptr() as *mut c_void, out.len(), Some(&mut read))
    }
    .is_ok()
        && read == out.len()
}

/// Address of the first match of `pattern` in the game's code section.
pub fn find(pattern: &str) -> Option<usize> {
    let pattern = parse_pattern(pattern)?;
    if pattern.is_empty() {
        return None;
    }
    snapshot(code_section()).into_iter().find_map(|(start, run)| {
        run.windows(pattern.len())
            .position(|window| window.iter().zip(&pattern).all(|(byte, p)| p.is_none_or(|p| *byte == p)))
            .map(|offset| start + offset)
    })
}

/// Addresses of every `call rel32` (E8) in the game's code that targets `target`.
pub fn find_calls_to(target: usize) -> Vec<usize> {
    let mut calls = Vec::new();
    for (start, run) in snapshot(code_section()) {
        for i in 0..run.len().saturating_sub(4) {
            if run[i] == 0xE8 {
                let rel = i32::from_le_bytes([run[i + 1], run[i + 2], run[i + 3], run[i + 4]]);
                if (start + i + 5).wrapping_add(rel as isize as usize) == target {
                    calls.push(start + i);
                }
            }
        }
    }
    calls
}

/// Overwrites code at `address`, keeping the page protection as it was.
pub unsafe fn patch(address: usize, bytes: &[u8]) -> windows::core::Result<()> {
    unsafe {
        let mut old = PAGE_PROTECTION_FLAGS::default();
        VirtualProtect(address as *const c_void, bytes.len(), PAGE_EXECUTE_READWRITE, &mut old)?;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), address as *mut u8, bytes.len());
        VirtualProtect(address as *const c_void, bytes.len(), old, &mut old)?;
        FlushInstructionCache(GetCurrentProcess(), Some(address as *const c_void), bytes.len())
    }
}

/// File version of GTAIV.exe, e.g. "1.2.0.59". Signatures are tied to a version.
pub fn game_version() -> Option<String> {
    unsafe {
        let mut path = [0u16; 1024];
        if GetModuleFileNameW(None, &mut path) == 0 {
            return None;
        }
        let path = PCWSTR(path.as_ptr());
        let size = GetFileVersionInfoSizeW(path, None);
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(path, None, size, data.as_mut_ptr() as *mut c_void).ok()?;
        let mut info: *mut c_void = std::ptr::null_mut();
        let mut info_len = 0u32;
        VerQueryValueW(data.as_ptr() as *const c_void, w!("\\"), &mut info, &mut info_len).ok().ok()?;
        let info = &*(info as *const VS_FIXEDFILEINFO);
        Some(format!(
            "{}.{}.{}.{}",
            info.dwFileVersionMS >> 16,
            info.dwFileVersionMS & 0xFFFF,
            info.dwFileVersionLS >> 16,
            info.dwFileVersionLS & 0xFFFF
        ))
    }
}
