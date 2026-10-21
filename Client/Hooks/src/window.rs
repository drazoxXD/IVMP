//! The game window shows IVMP: title `IVMP - Build: <date> <commit>` (stamped by build.rs) and
//! the IVMP icon (`Icons/logo.ico`, the launcher's, embedded here). A background thread waits
//! for the game's main window, then keeps the title and icon set (the game may set its own).

use std::time::Duration;

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconFromResourceEx, DefWindowProcW, EnumWindows, GW_OWNER, GetWindow, GetWindowThreadProcessId, HICON, ICON_BIG,
    ICON_SMALL, InternalGetWindowText, IsWindowVisible, LR_DEFAULTCOLOR, SendMessageW, WM_SETICON, WM_SETTEXT,
};
use windows::core::BOOL;

use crate::log;

const ICON: &[u8] = include_bytes!("../../../Icons/logo.ico");

/// How often the title and icon are checked once set.
const CHECK_EVERY: Duration = Duration::from_secs(2);

pub fn title() -> String {
    format!("IVMP - Build: {}", env!("IVMP_BUILD"))
}

/// The image in an .ico file closest to `size` pixels (0 in the directory = 256), as
/// (offset, length) of its data.
fn icon_image(size: u32) -> Option<(usize, usize)> {
    let count = u16::from_le_bytes(ICON.get(4..6)?.try_into().ok()?) as usize;
    (0..count)
        .filter_map(|i| {
            let entry = ICON.get(6 + i * 16..6 + i * 16 + 16)?;
            let width = if entry[0] == 0 { 256 } else { entry[0] as u32 };
            let length = u32::from_le_bytes(entry[8..12].try_into().ok()?) as usize;
            let offset = u32::from_le_bytes(entry[12..16].try_into().ok()?) as usize;
            (offset + length <= ICON.len()).then_some((width.abs_diff(size), offset, length))
        })
        .min_by_key(|(distance, _, _)| *distance)
        .map(|(_, offset, length)| (offset, length))
}

fn load_icon(size: u32) -> Option<HICON> {
    let (offset, length) = icon_image(size)?;
    unsafe { CreateIconFromResourceEx(&ICON[offset..offset + length], true, 0x0003_0000, size as i32, size as i32, LR_DEFAULTCOLOR).ok() }
}

unsafe extern "system" fn find_main_window(window: HWND, found: LPARAM) -> BOOL {
    unsafe {
        let mut pid = 0;
        GetWindowThreadProcessId(window, Some(&mut pid));
        let owned = GetWindow(window, GW_OWNER).is_ok_and(|owner| !owner.0.is_null());
        if pid == GetCurrentProcessId() && IsWindowVisible(window).as_bool() && !owned {
            *(found.0 as *mut HWND) = window;
            return BOOL(0);
        }
    }
    BOOL(1)
}

fn main_window() -> Option<HWND> {
    let mut found = HWND::default();
    unsafe {
        let _ = EnumWindows(Some(find_main_window), LPARAM(&mut found as *mut HWND as isize));
    }
    (!found.0.is_null()).then_some(found)
}

/// The title as Windows stores it (InternalGetWindowText sends no message to the window).
fn window_title(window: HWND) -> String {
    let mut buffer = [0u16; 256];
    let len = unsafe { InternalGetWindowText(window, &mut buffer) }.max(0) as usize;
    String::from_utf16_lossy(&buffer[..len])
}

/// Sets the title without going through the game's window procedure: the window is ANSI and
/// its WM_SETTEXT reached a Unicode default handler with ANSI text, so the title showed our
/// bytes read as UTF-16 ("IV" -> U+5649). DefWindowProcW stores the text itself.
fn set_window_title(window: HWND, title: &str) {
    let wide: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
    unsafe {
        DefWindowProcW(window, WM_SETTEXT, WPARAM(0), LPARAM(wide.as_ptr() as isize));
    }
}

/// Starts the thread that brands the game window.
pub fn install() {
    std::thread::spawn(|| {
        let title = title();
        let (big, small) = (load_icon(256).or_else(|| load_icon(48)), load_icon(16));
        if big.is_none() {
            log!("window: the IVMP icon couldn't be loaded");
        }
        let mut window = None;
        loop {
            match window.filter(|w| unsafe { IsWindowVisible(*w).as_bool() }).or_else(main_window) {
                Some(w) => {
                    if window != Some(w) {
                        window = Some(w);
                        log!("window: game window found, titled {title:?}");
                        unsafe {
                            if let Some(icon) = big {
                                SendMessageW(w, WM_SETICON, Some(WPARAM(ICON_BIG as usize)), Some(LPARAM(icon.0 as isize)));
                            }
                            if let Some(icon) = small.or(big) {
                                SendMessageW(w, WM_SETICON, Some(WPARAM(ICON_SMALL as usize)), Some(LPARAM(icon.0 as isize)));
                            }
                        }
                    }
                    if window_title(w) != title {
                        set_window_title(w, &title);
                    }
                    std::thread::sleep(CHECK_EVERY);
                }
                None => std::thread::sleep(Duration::from_millis(250)),
            }
        }
    });
}
