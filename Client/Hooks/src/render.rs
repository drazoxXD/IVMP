//! Hooks the game's Direct3D 9 device (GTA IV renders with D3D9): `Present` draws the UI,
//! `Reset` frees the UI's buffers first. The device methods live in a vtable shared by every
//! device of the same d3d9 implementation, so a throwaway device gives us the slots to patch.

use std::ffi::c_void;
use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};

use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct3D9::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{Interface, PCWSTR, w};

use crate::{events, log, memory, ui};

const RESET_SLOT: usize = 16;
const PRESENT_SLOT: usize = 17;
const END_SCENE_SLOT: usize = 42;

static ORIGINAL_PRESENT: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_RESET: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_END_SCENE: AtomicUsize = AtomicUsize::new(0);
/// The game device's own vtable slots (the Steam overlay and friends patch those, and their
/// trampolines skip the start of d3d9's functions, so only this sees every Present).
static VTABLE_PRESENT: AtomicUsize = AtomicUsize::new(0);
static VTABLE_RESET: AtomicUsize = AtomicUsize::new(0);
/// A Present of some kind has run (so EndScene stops drawing).
static PRESENT_SEEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static SWAP_CHAIN_PRESENT: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_WNDPROC: AtomicIsize = AtomicIsize::new(0);
static WINDOW: AtomicIsize = AtomicIsize::new(0);

type Present = unsafe extern "system" fn(*mut c_void, *const RECT, *const RECT, HWND, *const c_void) -> i32;
type EndScene = unsafe extern "system" fn(*mut c_void) -> i32;
type Reset = unsafe extern "system" fn(*mut c_void, *mut D3DPRESENT_PARAMETERS) -> i32;

/// Creates a throwaway device and returns the code addresses of its Reset and Present. The vtable
/// is per device (heap), but the methods are shared code in d3d9.dll, so those get the hook.
fn device_methods() -> Option<[usize; 3]> {
    unsafe {
        let window = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            PCWSTR::null(),
            WS_OVERLAPPED,
            0,
            0,
            8,
            8,
            None,
            None,
            None,
            None,
        )
        .ok()?;
        let d3d = Direct3DCreate9(D3D_SDK_VERSION)?;
        let mut device: Option<IDirect3DDevice9> = None;
        let mut created = Ok(());
        // The null device has no hardware behind it (so it works while the game holds the
        // adapter) and shares the vtable with the real ones.
        for (kind, format) in [(D3DDEVTYPE_HAL, D3DFMT_UNKNOWN), (D3DDEVTYPE_HAL, D3DFMT_X8R8G8B8), (D3DDEVTYPE_NULLREF, D3DFMT_UNKNOWN)] {
            let mut parameters = D3DPRESENT_PARAMETERS {
                Windowed: true.into(),
                SwapEffect: D3DSWAPEFFECT_DISCARD,
                BackBufferFormat: format,
                BackBufferWidth: 2,
                BackBufferHeight: 2,
                hDeviceWindow: window,
                ..Default::default()
            };
            created = d3d.CreateDevice(D3DADAPTER_DEFAULT, kind, window, D3DCREATE_SOFTWARE_VERTEXPROCESSING as u32, &mut parameters, &mut device);
            if created.is_ok() && device.is_some() {
                log!("render: dummy device created as {kind:?}");
                break;
            }
            log!("render: dummy device {kind:?} {format:?} failed: {created:?}");
        }
        let vtable = device.as_ref().and_then(|d| {
            let object = d.as_raw() as usize;
            let mut first = [0u8; 4];
            if !memory::read(object, &mut first) {
                return None;
            }
            let vtable = u32::from_le_bytes(first) as usize;
            let mut targets = [0usize; 3];
            for (target, slot) in targets.iter_mut().zip([RESET_SLOT, PRESENT_SLOT, END_SCENE_SLOT]) {
                let mut entry = [0u8; 4];
                if !memory::read(vtable + slot * 4, &mut entry) {
                    return None;
                }
                *target = u32::from_le_bytes(entry) as usize;
                log!("render: dummy device method {slot} -> {:#x} ({})", *target, module_of(*target));
            }
            Some(targets)
        });
        drop(device);
        let _ = DestroyWindow(window);
        if created.is_err() {
            log!("render: could not create a device to find the vtable: {created:?}");
        }
        vtable
    }
}

/// Name of the module an address is in, for the log.
fn module_of(address: usize) -> String {
    use windows::Win32::System::LibraryLoader::{GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, GetModuleFileNameW, GetModuleHandleExW};
    let mut module = windows::Win32::Foundation::HMODULE::default();
    let found = unsafe {
        GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, PCWSTR(address as *const u16), &mut module)
    };
    if found.is_err() {
        return "no module".into();
    }
    let mut name = [0u16; 260];
    let len = unsafe { GetModuleFileNameW(Some(module), &mut name) } as usize;
    String::from_utf16_lossy(&name[..len])
}

/// Overwrites the start of the function at `target` with a jump to `hook` and returns the address
/// of a stub that runs the original. Only for the standard `mov edi, edi; push ebp; mov ebp, esp`
/// prologue (exactly 5 bytes, so the copy needs no instruction decoding).
fn inline_hook(target: usize, hook: usize) -> Option<usize> {
    use windows::Win32::System::Memory::{MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READWRITE, VirtualAlloc};
    let mut start = [0u8; 8];
    if !memory::read(target, &mut start) {
        return None;
    }
    let stub = unsafe { VirtualAlloc(None, 32, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE) } as usize;
    if stub == 0 {
        return None;
    }
    let mut code = [0u8; 16];
    // How many bytes the jump to the hook overwrites (whole instructions only).
    let length;
    if start[0] == 0xE9 {
        // Somebody else (the Steam overlay, FusionFix, ...) already jumps from here: chain to
        // wherever that jump goes.
        length = 5;
        let destination = (target + 5).wrapping_add(u32::from_le_bytes([start[1], start[2], start[3], start[4]]) as usize);
        log!("render: {target:#x} already jumps to {destination:#x} ({}), chaining to it", module_of(destination));
        code[0] = 0xE9;
        code[1..5].copy_from_slice(&destination.wrapping_sub(stub + 5).to_le_bytes());
    } else if start[..5] == [0x8B, 0xFF, 0x55, 0x8B, 0xEC] {
        // mov edi, edi; push ebp; mov ebp, esp
        length = 5;
        code[..5].copy_from_slice(&start[..5]);
    } else if start[..3] == [0x6A, 0x14, 0xB8] {
        // push 14h; mov eax, imm32 (an SEH prologue, no relative operands)
        length = 7;
        code[..7].copy_from_slice(&start[..7]);
    } else {
        log!("render: unexpected prologue at {target:#x}: {start:02x?}");
        return None;
    }
    if start[0] != 0xE9 {
        code[length] = 0xE9;
        code[length + 1..length + 5].copy_from_slice(&(target + length).wrapping_sub(stub + length + 5).to_le_bytes());
    }
    unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), stub as *mut u8, code.len()) };
    let mut patch = [0x90u8; 7];
    patch[0] = 0xE9;
    patch[1..5].copy_from_slice(&hook.wrapping_sub(target + 5).to_le_bytes());
    unsafe { memory::patch(target, &patch[..length]) }.ok()?;
    Some(stub)
}

/// Hooks Present and Reset. Call once, after the game has unpacked.
pub fn install() {
    let Some([reset_target, present_target, end_scene_target]) = device_methods() else { return };
    for (target, hook, original, name) in [
        (present_target, present as usize, &ORIGINAL_PRESENT, "Present"),
        (reset_target, reset as usize, &ORIGINAL_RESET, "Reset"),
        (end_scene_target, end_scene as usize, &ORIGINAL_END_SCENE, "EndScene"),
    ] {
        match inline_hook(target, hook) {
            Some(stub) => original.store(stub, Ordering::SeqCst),
            None => {
                log!("render: could not hook {name} at {target:#x}");
                return;
            }
        }
    }
    log!("render: hooked the D3D9 device's Present ({present_target:#x}) and Reset ({reset_target:#x})");
}

/// Draws the UI on `device` (once per frame, whichever of EndScene / Present gets here first).
/// GTA IV doesn't go through Present (the log never shows it), so EndScene on the back buffer is
/// the usual place.
fn draw(device: *mut c_void, only_back_buffer: bool) {
    static LAST: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
    // Never on a loading screen (another thread draws it while the game thread loads: touching
    // the device then froze the game), and only ever from the one thread that draws the frames.
    static DRAW_THREAD: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    if !crate::hang::game_running() {
        ui::close_console();
        return;
    }
    let thread = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
    if DRAW_THREAD.compare_exchange(0, thread, Ordering::SeqCst, Ordering::SeqCst).is_err_and(|owner| owner != thread) {
        return;
    }
    let Some(d3d) = (unsafe { IDirect3DDevice9::from_raw_borrowed(&device) }) else { return };
    if only_back_buffer {
        let target = unsafe { d3d.GetRenderTarget(0) }.ok().map(|t| t.as_raw() as usize);
        let back = unsafe { d3d.GetBackBuffer(0, 0, D3DBACKBUFFER_TYPE_MONO) }.ok().map(|t| t.as_raw() as usize);
        if target.is_none() || target != back {
            return;
        }
    }
    {
        let mut last = LAST.lock().unwrap();
        let now = std::time::Instant::now();
        if last.is_some_and(|l| now.duration_since(l) < std::time::Duration::from_millis(4)) {
            return;
        }
        *last = Some(now);
    }
    let mut parameters = D3DDEVICE_CREATION_PARAMETERS::default();
    if unsafe { d3d.GetCreationParameters(&mut parameters) }.is_ok() {
        subclass(parameters.hFocusWindow);
        events::emit(events::GameEvent::Frame);
        ui::frame(d3d, parameters.hFocusWindow);
    }
}

unsafe extern "system" fn present(device: *mut c_void, source: *const RECT, dest: *const RECT, window: HWND, dirty: *const c_void) -> i32 {
    static FIRST: std::sync::Once = std::sync::Once::new();
    FIRST.call_once(|| log!("render: first Present, device {device:p}"));
    draw(device, false);
    let original: Present = unsafe { std::mem::transmute(ORIGINAL_PRESENT.load(Ordering::SeqCst)) };
    unsafe { original(device, source, dest, window, dirty) }
}

/// Patches the device's own vtable (Present and Reset), once, from the first EndScene.
fn hook_device_vtable(device: *mut c_void) {
    let vtable = unsafe { *(device as *const usize) };
    for (slot, hook, original) in [
        (PRESENT_SLOT, vtable_present as usize, &VTABLE_PRESENT),
        (RESET_SLOT, vtable_reset as usize, &VTABLE_RESET),
    ] {
        let address = vtable + slot * 4;
        original.store(unsafe { *(address as *const usize) }, Ordering::SeqCst);
        if let Err(e) = unsafe { memory::patch(address, &(hook as u32).to_le_bytes()) } {
            log!("render: could not hook the device vtable slot {slot}: {e}");
            return;
        }
    }
    log!("render: hooked the game device's own vtable {vtable:#x} (Present, Reset)");
    // The game may present through its swap chain instead of the device.
    if let Some(d3d) = unsafe { IDirect3DDevice9::from_raw_borrowed(&device) } {
        if let Ok(chain) = unsafe { d3d.GetSwapChain(0) } {
            let chain_vtable = unsafe { *(chain.as_raw() as *const usize) };
            let address = chain_vtable + 3 * 4;
            SWAP_CHAIN_PRESENT.store(unsafe { *(address as *const usize) }, Ordering::SeqCst);
            match unsafe { memory::patch(address, &(swap_chain_present as u32).to_le_bytes()) } {
                Ok(()) => log!("render: hooked the swap chain's Present too (vtable {chain_vtable:#x})"),
                Err(e) => log!("render: could not hook the swap chain's Present: {e}"),
            }
        }
    }
}

type SwapChainPresent = unsafe extern "system" fn(*mut c_void, *const RECT, *const RECT, HWND, *const c_void, u32) -> i32;

unsafe extern "system" fn swap_chain_present(chain: *mut c_void, source: *const RECT, dest: *const RECT, window: HWND, dirty: *const c_void, flags: u32) -> i32 {
    static FIRST: std::sync::Once = std::sync::Once::new();
    FIRST.call_once(|| log!("render: first Present through the swap chain"));
    // The swap chain knows its device.
    if let Some(chain_ref) = unsafe { IDirect3DSwapChain9::from_raw_borrowed(&chain) } {
        if let Ok(device) = unsafe { chain_ref.GetDevice() } {
            PRESENT_SEEN.store(true, Ordering::SeqCst);
            draw(device.as_raw(), false);
        }
    }
    let original: SwapChainPresent = unsafe { std::mem::transmute(SWAP_CHAIN_PRESENT.load(Ordering::SeqCst)) };
    unsafe { original(chain, source, dest, window, dirty, flags) }
}

unsafe extern "system" fn vtable_present(device: *mut c_void, source: *const RECT, dest: *const RECT, window: HWND, dirty: *const c_void) -> i32 {
    static FIRST: std::sync::Once = std::sync::Once::new();
    FIRST.call_once(|| log!("render: first Present through the device vtable"));
    PRESENT_SEEN.store(true, Ordering::SeqCst);
    draw(device, false);
    let original: Present = unsafe { std::mem::transmute(VTABLE_PRESENT.load(Ordering::SeqCst)) };
    unsafe { original(device, source, dest, window, dirty) }
}

unsafe extern "system" fn vtable_reset(device: *mut c_void, parameters: *mut D3DPRESENT_PARAMETERS) -> i32 {
    ui::device_lost();
    events::emit(events::GameEvent::DeviceReset);
    let original: Reset = unsafe { std::mem::transmute(VTABLE_RESET.load(Ordering::SeqCst)) };
    unsafe { original(device, parameters) }
}

unsafe extern "system" fn end_scene(device: *mut c_void) -> i32 {
    static FIRST: std::sync::Once = std::sync::Once::new();
    FIRST.call_once(|| {
        log!("render: first EndScene, device {device:p}");
        hook_device_vtable(device);
    });
    // The frame is drawn once, at Present, as the last thing; EndScene only until that hook is in.
    if !PRESENT_SEEN.load(Ordering::SeqCst) {
        draw(device, true);
    }
    let original: EndScene = unsafe { std::mem::transmute(ORIGINAL_END_SCENE.load(Ordering::SeqCst)) };
    unsafe { original(device) }
}

unsafe extern "system" fn reset(device: *mut c_void, parameters: *mut D3DPRESENT_PARAMETERS) -> i32 {
    ui::device_lost();
    events::emit(events::GameEvent::DeviceReset);
    let original: Reset = unsafe { std::mem::transmute(ORIGINAL_RESET.load(Ordering::SeqCst)) };
    unsafe { original(device, parameters) }
}

/// Puts our window procedure in front of the game's, once, so F1 and the console's keys and mouse
/// reach the UI.
fn subclass(window: HWND) {
    if window.0.is_null() || WINDOW.load(Ordering::Relaxed) != 0 {
        return;
    }
    WINDOW.store(window.0 as isize, Ordering::Relaxed);
    let old = unsafe {
        if IsWindowUnicode(window).as_bool() {
            SetWindowLongW(window, GWL_WNDPROC, window_proc as usize as i32)
        } else {
            SetWindowLongA(window, GWL_WNDPROC, window_proc as usize as i32)
        }
    };
    ORIGINAL_WNDPROC.store(old as isize, Ordering::SeqCst);
    log!("render: window {:#x} subclassed (F1 opens the console)", window.0 as usize);
}

unsafe extern "system" fn window_proc(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if ui::window_message(message, wparam.0, lparam.0) {
        return LRESULT(0);
    }
    let original: WNDPROC = unsafe { std::mem::transmute(ORIGINAL_WNDPROC.load(Ordering::SeqCst)) };
    unsafe {
        if IsWindowUnicode(window).as_bool() {
            CallWindowProcW(original, window, message, wparam, lparam)
        } else {
            CallWindowProcA(original, window, message, wparam, lparam)
        }
    }
}

#[allow(dead_code)]
fn unused(_: HANDLE) {}
