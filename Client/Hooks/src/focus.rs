//! Keep the game running and rendering when its window loses focus (needed to run two clients
//! side by side).
//!
//! The window procedure (0x420A40, encrypted on disk) handles WM_ACTIVATEAPP(FALSE) by setting
//! the global "app inactive" flag (0x017ED8D1, read in ~50 places to pause the game), calling a
//! focus-lost callback and minimising the window when fullscreen. We turn its `jz` into a `jmp`
//! so deactivation only reaches DefWindowProc. The procedure is decrypted at runtime, so we wait
//! for its bytes to appear.
//!
//! Background slowdown: Windows throttles background processes (EcoQoS, and it ignores the
//! game's 1 ms timer resolution for windows that aren't in front), and the game skips work
//! while `IsIconic` says the window is minimised. A client that slows down while alt-tabbed
//! falls out of sync, so both are switched off: power throttling is disabled for the process
//! and `IsIconic` always answers "not minimised".
//!
//! Input still arrives while unfocused (the DirectInput devices read in the background), so the
//! keyboard and mouse devices' GetDeviceState / GetDeviceData report nothing while another
//! window has the focus.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

use crate::{log, memory};

/// `cmp esi, [mainThreadId]; jz skip` in the WM_ACTIVATEAPP(FALSE) branch; `jz` at +6.
const PATTERN: &str = "3B 35 ?? ?? ?? ?? 74 24 A1 ?? ?? ?? ?? C6 05 ?? ?? ?? ?? 01 85 C0";

/// Where the pattern sits in GTAIV.exe 1.2.0.59 (Ghidra address; encrypted on disk).
const WNDPROC_SITE: usize = 0x0042_0AC1;
const UNPACK_TIMEOUT: Duration = Duration::from_secs(60);

fn pattern_at(address: usize) -> bool {
    let Some(pattern) = memory::parse_pattern(PATTERN) else { return false };
    let mut bytes = vec![0u8; pattern.len()];
    memory::read(address, &mut bytes)
        && bytes.iter().zip(&pattern).all(|(b, p)| p.is_none_or(|p| *b == p))
}

/// Waits until the protection has decrypted the game code, using the window procedure (encrypted
/// on disk) as the marker. Only reads those few bytes while waiting.
pub fn wait_for_unpack() -> bool {
    let site = memory::from_ghidra(WNDPROC_SITE);
    let start = Instant::now();
    while start.elapsed() < UNPACK_TIMEOUT {
        if pattern_at(site) {
            log!("game code unpacked after {} ms", start.elapsed().as_millis());
            // Let the protection finish restoring page protections.
            std::thread::sleep(Duration::from_millis(500));
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// `IsIconic` import slot (Ghidra address).
const IS_ICONIC_IMPORT: usize = 0x00E7_33DC;

unsafe extern "system" fn never_iconic(_window: windows::Win32::Foundation::HWND) -> i32 {
    0
}

/// Keeps the game at full speed in the background.
fn no_background_slowdown() {
    use windows::Win32::System::Threading::{
        GetCurrentProcess, PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION, PROCESS_POWER_THROTTLING_STATE, ProcessPowerThrottling,
        SetProcessInformation,
    };
    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        // Take control of both, with neither enabled: no EcoQoS, keep our timer resolution.
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED | PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
        StateMask: 0,
    };
    let result = unsafe {
        SetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            &state as *const _ as *const c_void,
            size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    };
    match result {
        Ok(()) => log!("focus: background power throttling off"),
        Err(e) => log!("focus: could not turn off power throttling: {e}"),
    }
    let slot = memory::from_ghidra(IS_ICONIC_IMPORT);
    match unsafe { memory::patch(slot, &(never_iconic as usize as u32).to_le_bytes()) } {
        Ok(()) => log!("focus: the game never sees itself as minimised"),
        Err(e) => log!("focus: IsIconic hook failed: {e}"),
    }
}

/// Patches the window procedure. Call after [`wait_for_unpack`].
pub fn install() {
    no_background_slowdown();
    let site = memory::from_ghidra(WNDPROC_SITE) + 6;
    let mut current = [0u8; 1];
    if !memory::read(site, &mut current) || current[0] != 0x74 {
        log!("focus: unexpected byte {:#04x} at {site:#x}, skipped", current[0]);
        return;
    }
    match unsafe { memory::patch(site, &[0xEB]) } {
        Ok(()) => log!("focus: game keeps running in the background ({site:#x})"),
        Err(e) => log!("focus: patch at {site:#x} failed: {e}"),
    }
}

/// IDirectInputDevice8* globals, set when the game creates its devices (0x69E9C0, 0x69DA50).
const KEYBOARD_DEVICE: usize = 0x018B_7DAC;
const MOUSE_DEVICE: usize = 0x01BB_393C;
/// IDirectInputDevice8 vtable slots: GetDeviceState, GetDeviceData.
const GET_DEVICE_STATE: usize = 9;
const GET_DEVICE_DATA: usize = 10;
const DI_OK: i32 = 0;

type GetDeviceState = unsafe extern "system" fn(*mut c_void, u32, *mut u8) -> i32;
type GetDeviceData = unsafe extern "system" fn(*mut c_void, u32, *mut u8, *mut u32, u32) -> i32;

static INPUT_HOOKED: AtomicBool = AtomicBool::new(false);
/// Originals per vtable (keyboard and mouse can share one).
static STATE_ORIGINALS: [AtomicUsize; 2] = [AtomicUsize::new(0), AtomicUsize::new(0)];
static DATA_ORIGINALS: [AtomicUsize; 2] = [AtomicUsize::new(0), AtomicUsize::new(0)];
static VTABLES: [AtomicUsize; 2] = [AtomicUsize::new(0), AtomicUsize::new(0)];

/// Whether one of our windows has the focus.
fn focused() -> bool {
    unsafe {
        let window = GetForegroundWindow();
        let mut pid = 0u32;
        GetWindowThreadProcessId(window, Some(&mut pid));
        pid == GetCurrentProcessId()
    }
}

/// Finds the originals for the device's vtable.
fn originals_of(device: *mut c_void) -> Option<usize> {
    let vtable = unsafe { *(device as *const usize) };
    VTABLES.iter().position(|v| v.load(Ordering::SeqCst) == vtable)
}

unsafe extern "system" fn get_device_state(device: *mut c_void, size: u32, data: *mut u8) -> i32 {
    let Some(index) = originals_of(device) else { return DI_OK };
    let original: GetDeviceState = unsafe { std::mem::transmute(STATE_ORIGINALS[index].load(Ordering::SeqCst)) };
    let result = unsafe { original(device, size, data) };
    if !focused() && !data.is_null() {
        unsafe { std::ptr::write_bytes(data, 0, size as usize) };
        return DI_OK;
    }
    result
}

unsafe extern "system" fn get_device_data(device: *mut c_void, size: u32, data: *mut u8, count: *mut u32, flags: u32) -> i32 {
    let Some(index) = originals_of(device) else { return DI_OK };
    let original: GetDeviceData = unsafe { std::mem::transmute(DATA_ORIGINALS[index].load(Ordering::SeqCst)) };
    let result = unsafe { original(device, size, data, count, flags) };
    if !focused() && !count.is_null() {
        // Read (and so drop) the buffered events, but hand none to the game.
        unsafe { *count = 0 };
        return DI_OK;
    }
    result
}

/// Once the game has created its keyboard and mouse, hooks their vtables. Call every frame.
pub fn hook_input() {
    if INPUT_HOOKED.load(Ordering::Relaxed) {
        return;
    }
    let devices = [KEYBOARD_DEVICE, MOUSE_DEVICE].map(|d| unsafe { *(memory::from_ghidra(d) as *const usize) });
    if devices.contains(&0) {
        return;
    }
    INPUT_HOOKED.store(true, Ordering::Relaxed);
    let mut next = 0;
    for device in devices {
        let vtable = unsafe { *(device as *const usize) };
        if VTABLES.iter().any(|v| v.load(Ordering::SeqCst) == vtable) {
            continue;
        }
        let state_slot = vtable + GET_DEVICE_STATE * 4;
        let data_slot = vtable + GET_DEVICE_DATA * 4;
        unsafe {
            STATE_ORIGINALS[next].store(*(state_slot as *const usize), Ordering::SeqCst);
            DATA_ORIGINALS[next].store(*(data_slot as *const usize), Ordering::SeqCst);
            VTABLES[next].store(vtable, Ordering::SeqCst);
            let state = memory::patch(state_slot, &(get_device_state as usize as u32).to_le_bytes());
            let data = memory::patch(data_slot, &(get_device_data as usize as u32).to_le_bytes());
            if let Err(e) = state.and(data) {
                log!("focus: input hook on vtable {vtable:#x} failed: {e}");
            }
        }
        next += 1;
    }
    log!("focus: keyboard and mouse ignored while another window has the focus");
}
