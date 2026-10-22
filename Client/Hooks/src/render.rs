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

static ORIGINAL_PRESENT: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_RESET: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_WNDPROC: AtomicIsize = AtomicIsize::new(0);
static WINDOW: AtomicIsize = AtomicIsize::new(0);

type Present = unsafe extern "system" fn(*mut c_void, *const RECT, *const RECT, HWND, *const c_void) -> i32;
type Reset = unsafe extern "system" fn(*mut c_void, *mut D3DPRESENT_PARAMETERS) -> i32;

/// Creates a throwaway device and returns the address of its vtable.
fn device_vtable() -> Option<usize> {
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
        let mut parameters = D3DPRESENT_PARAMETERS {
            Windowed: true.into(),
            SwapEffect: D3DSWAPEFFECT_DISCARD,
            BackBufferFormat: D3DFMT_UNKNOWN,
            hDeviceWindow: window,
            ..Default::default()
        };
        let mut device: Option<IDirect3DDevice9> = None;
        let created = d3d.CreateDevice(
            D3DADAPTER_DEFAULT,
            D3DDEVTYPE_HAL,
            window,
            D3DCREATE_SOFTWARE_VERTEXPROCESSING as u32,
            &mut parameters,
            &mut device,
        );
        let vtable = device.as_ref().map(|d| *(d.as_raw() as *const usize));
        drop(device);
        let _ = DestroyWindow(window);
        if created.is_err() {
            log!("render: could not create a device to find the vtable: {created:?}");
        }
        vtable
    }
}

/// Hooks Present and Reset. Call once, after the game has unpacked.
pub fn install() {
    let Some(vtable) = device_vtable() else { return };
    for (slot, hook, original) in [
        (PRESENT_SLOT, present as usize, &ORIGINAL_PRESENT),
        (RESET_SLOT, reset as usize, &ORIGINAL_RESET),
    ] {
        let address = vtable + slot * 4;
        original.store(unsafe { *(address as *const usize) }, Ordering::SeqCst);
        if let Err(e) = unsafe { memory::patch(address, &(hook as u32).to_le_bytes()) } {
            log!("render: could not hook vtable slot {slot}: {e}");
            return;
        }
    }
    log!("render: hooked the D3D9 device's Present and Reset (vtable {vtable:#x})");
}

unsafe extern "system" fn present(device: *mut c_void, source: *const RECT, dest: *const RECT, window: HWND, dirty: *const c_void) -> i32 {
    if let Some(d3d) = unsafe { IDirect3DDevice9::from_raw_borrowed(&device) } {
        let mut parameters = D3DDEVICE_CREATION_PARAMETERS::default();
        if unsafe { d3d.GetCreationParameters(&mut parameters) }.is_ok() {
            subclass(parameters.hFocusWindow);
            events::emit(events::GameEvent::Frame);
            ui::frame(d3d, parameters.hFocusWindow);
        }
    }
    let original: Present = unsafe { std::mem::transmute(ORIGINAL_PRESENT.load(Ordering::SeqCst)) };
    unsafe { original(device, source, dest, window, dirty) }
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
