//! Memory use for the Memory overlay: the game, the web layer (CEF), the IVMP client and the mods.
//!
//! The client's heap is counted by [`Counting`], the DLL's global allocator (Rust allocates from
//! the process heap the game uses too, so it can't be told apart otherwise). Everything else is
//! private memory (commit) from the process counters, sampled once a second on its own thread.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, HANDLE, HMODULE};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::ProcessStatus::{EnumProcessModules, GetModuleInformation, K32GetProcessMemoryInfo, MODULEINFO, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

/// Bytes the client has allocated and not freed.
static HEAP: AtomicUsize = AtomicUsize::new(0);

/// The system allocator, counting what is in use.
pub struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            HEAP.fetch_add(layout.size(), Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            HEAP.fetch_add(layout.size(), Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        HEAP.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new = unsafe { System.realloc(ptr, layout, new_size) };
        if !new.is_null() {
            HEAP.fetch_add(new_size, Ordering::Relaxed);
            HEAP.fetch_sub(layout.size(), Ordering::Relaxed);
        }
        new
    }
}

/// One sample, in bytes.
#[derive(Clone, Copy, Default)]
pub struct Usage {
    /// The game: the process's private memory minus the client's heap and the page's texture.
    pub game: usize,
    /// The web process and CEF's helpers, plus the page's texture in the game process.
    pub cef: usize,
    pub cef_processes: usize,
    /// IVMP_Client.dll's image and heap.
    pub client: usize,
    /// Loaded .asi mods (their images; what they allocate counts as the game's).
    pub mods: usize,
    pub mod_count: usize,
    /// Private memory of the game process and the web processes.
    pub total: usize,
}

static LAST: Mutex<Option<Usage>> = Mutex::new(None);
static SAMPLING: AtomicBool = AtomicBool::new(false);

/// The latest sample (None until the first one). Starts sampling on the first call.
pub fn usage() -> Option<Usage> {
    if !SAMPLING.swap(true, Ordering::Relaxed) {
        std::thread::spawn(|| loop {
            let usage = sample();
            *LAST.lock().unwrap() = Some(usage);
            std::thread::sleep(Duration::from_secs(1));
        });
    }
    *LAST.lock().unwrap()
}

/// Private memory (commit) of a process.
fn private_bytes(process: HANDLE) -> usize {
    let mut counters = PROCESS_MEMORY_COUNTERS_EX::default();
    let size = size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
    counters.cb = size;
    let ok = unsafe { K32GetProcessMemoryInfo(process, &mut counters as *mut _ as *mut PROCESS_MEMORY_COUNTERS, size) };
    if ok.as_bool() { counters.PrivateUsage } else { 0 }
}

/// Image sizes of our DLL and of the loaded .asi modules (count, bytes).
fn images() -> (usize, usize, usize) {
    let process = unsafe { GetCurrentProcess() };
    let mut modules = [HMODULE::default(); 1024];
    let mut needed = 0u32;
    if unsafe { EnumProcessModules(process, modules.as_mut_ptr(), size_of_val(&modules) as u32, &mut needed) }.is_err() {
        return (0, 0, 0);
    }
    let ours = images as *const () as usize;
    let (mut client, mut mods, mut mod_count) = (0, 0, 0);
    for &module in &modules[..(needed as usize / size_of::<HMODULE>()).min(modules.len())] {
        let mut info = MODULEINFO::default();
        if unsafe { GetModuleInformation(process, module, &mut info, size_of::<MODULEINFO>() as u32) }.is_err() {
            continue;
        }
        let (start, size) = (info.lpBaseOfDll as usize, info.SizeOfImage as usize);
        if (start..start + size).contains(&ours) {
            client = size;
            continue;
        }
        let mut name = [0u16; 1024];
        let len = unsafe { GetModuleFileNameW(Some(module), &mut name) } as usize;
        if String::from_utf16_lossy(&name[..len]).to_lowercase().ends_with(".asi") {
            mods += size;
            mod_count += 1;
        }
    }
    (client, mods, mod_count)
}

fn sample() -> Usage {
    let process_total = private_bytes(unsafe { GetCurrentProcess() });
    let (image, mods, mod_count) = images();
    let heap = HEAP.load(Ordering::Relaxed);
    let texture = crate::web::texture_bytes();
    let ids = crate::web::process_ids();
    let mut web = 0;
    for id in &ids {
        if let Ok(handle) = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, *id) } {
            web += private_bytes(handle);
            let _ = unsafe { CloseHandle(handle) };
        }
    }
    Usage {
        // Images are mapped files, not private memory: only the heap and texture come off.
        game: process_total.saturating_sub(heap + texture),
        cef: web + texture,
        cef_processes: ids.len(),
        client: image + heap,
        mods,
        mod_count,
        total: process_total + web,
    }
}

/// "2.2 GB", "512 MB", "12 MB".
pub fn format(bytes: usize) -> String {
    let mb = bytes as f64 / (1024.0 * 1024.0);
    if mb >= 1024.0 { format!("{:.1} GB", mb / 1024.0) } else { format!("{mb:.0} MB") }
}
