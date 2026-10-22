//! The web layer: CEF (in the separate `cef` folder next to IVMP_Client.dll, behind IVMP_Web.dll's
//! small C API) renders a locked-down page windowless; its frames are copied into a Direct3D 9
//! texture and drawn over the game, under imgui. See docs/web-engine-research.md.
//!
//! F2 shows or hides the page (and gives it the keyboard and mouse). Without the `cef` folder
//! nothing happens: the web layer is an optional download.

use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

use windows::Win32::Graphics::Direct3D9::*;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LOAD_WITH_ALTERED_SEARCH_PATH, LoadLibraryExW};
use windows::core::{HSTRING, PCSTR};

use crate::{imgui_dx9, log};

/// The page the layer opens (served from `cef\ui` by the ivmp:// scheme).
const START_URL: &str = "ivmp://ui/index.html";

struct Api {
    start: unsafe extern "C" fn(*const u16, i32, i32, *const c_char) -> i32,
    resize: unsafe extern "C" fn(i32, i32),
    frame_version: unsafe extern "C" fn() -> u32,
    copy_frame: unsafe extern "C" fn(*mut u8, i32, i32, i32) -> u32,
    visible: unsafe extern "C" fn(i32),
    focus: unsafe extern "C" fn(i32),
    mouse_move: unsafe extern "C" fn(i32, i32),
    mouse_button: unsafe extern "C" fn(i32, i32, i32, i32),
    mouse_wheel: unsafe extern "C" fn(i32, i32, i32),
    key: unsafe extern "C" fn(i32, i32, i32, i32),
    emit: unsafe extern "C" fn(*const c_char, *const c_char),
}

static API: OnceLock<Api> = OnceLock::new();
static VISIBLE: AtomicBool = AtomicBool::new(false);
static LAST_X: AtomicU32 = AtomicU32::new(0);
static LAST_Y: AtomicU32 = AtomicU32::new(0);

pub fn visible() -> bool {
    VISIBLE.load(Ordering::Relaxed) && API.get().is_some()
}

extern "C" fn log_from_web(line: *const c_char) {
    if !line.is_null() {
        log!("{}", unsafe { CStr::from_ptr(line) }.to_string_lossy());
    }
}

/// A message the page sent with `ivmp.send` (the one channel out of the page). The script runner
/// will take these; for now they are logged.
extern "C" fn bridge_from_web(json: *const c_char) {
    if !json.is_null() {
        log!("web: bridge: {}", unsafe { CStr::from_ptr(json) }.to_string_lossy());
    }
}

/// Finds the cef folder and starts CEF on its own thread once the game runs frames.
pub fn install() {
    let folder = crate::dll_dir().join("cef");
    if !folder.join("IVMP_Web.dll").exists() {
        log!("web: no cef folder next to the client ({}), the web layer is off", folder.display());
        return;
    }
    std::thread::spawn(move || {
        // Not during the game's own loading.
        while !crate::hang::game_running() {
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        std::thread::sleep(std::time::Duration::from_secs(3));
        let dll = folder.join("IVMP_Web.dll");
        // The altered search path makes libcef.dll load from the same folder.
        let module = match unsafe { LoadLibraryExW(&HSTRING::from(dll.as_os_str()), None, LOAD_WITH_ALTERED_SEARCH_PATH) } {
            Ok(module) => module,
            Err(e) => {
                log!("web: could not load {}: {e}", dll.display());
                return;
            }
        };
        macro_rules! symbol {
            ($name:literal) => {
                match unsafe { GetProcAddress(module, PCSTR(concat!($name, "\0").as_ptr())) } {
                    Some(address) => unsafe { std::mem::transmute::<_, _>(address) },
                    None => {
                        log!("web: IVMP_Web.dll has no {}", $name);
                        return;
                    }
                }
            };
        }
        let set_log: unsafe extern "C" fn(extern "C" fn(*const c_char)) = symbol!("ivmp_web_set_log");
        let set_bridge: unsafe extern "C" fn(extern "C" fn(*const c_char)) = symbol!("ivmp_web_set_bridge");
        let api = Api {
            start: symbol!("ivmp_web_start"),
            resize: symbol!("ivmp_web_resize"),
            frame_version: symbol!("ivmp_web_frame_version"),
            copy_frame: symbol!("ivmp_web_copy_frame"),
            visible: symbol!("ivmp_web_visible"),
            focus: symbol!("ivmp_web_focus"),
            mouse_move: symbol!("ivmp_web_mouse_move"),
            mouse_button: symbol!("ivmp_web_mouse_button"),
            mouse_wheel: symbol!("ivmp_web_mouse_wheel"),
            key: symbol!("ivmp_web_key"),
            emit: symbol!("ivmp_web_emit"),
        };
        unsafe {
            set_log(log_from_web);
            set_bridge(bridge_from_web);
        }
        let wide: Vec<u16> = folder.as_os_str().to_string_lossy().encode_utf16().chain(Some(0)).collect();
        let url = CString::new(START_URL).unwrap();
        log!("web: starting CEF from {}", folder.display());
        if unsafe { (api.start)(wide.as_ptr(), 1280, 720, url.as_ptr()) } != 1 {
            log!("web: CEF did not start");
            return;
        }
        unsafe { (api.visible)(0) };
        let _ = API.set(api);
        log!("web: ready, F2 shows the page");
    });
}

/// F2: show or hide the page (and give it, or take back, the keyboard and mouse).
pub fn toggle() {
    let Some(api) = API.get() else { return };
    let show = !VISIBLE.load(Ordering::Relaxed);
    VISIBLE.store(show, Ordering::Relaxed);
    unsafe {
        (api.visible)(show as i32);
        (api.focus)(show as i32);
    }
    if show {
        crate::ui::reset_raw_cursor();
        // Tell the page the game is here (demonstrates the other direction of the bridge).
        let name = CString::new("game.hello").unwrap();
        let data = CString::new(r#"{"from":"IVMP client"}"#).unwrap();
        unsafe { (api.emit)(name.as_ptr(), data.as_ptr()) };
    }
}

pub fn close() {
    if VISIBLE.swap(false, Ordering::Relaxed) {
        if let Some(api) = API.get() {
            unsafe {
                (api.visible)(0);
                (api.focus)(0);
            }
        }
    }
}

pub fn mouse_move(x: f32, y: f32) {
    if let Some(api) = API.get() {
        LAST_X.store(x as u32, Ordering::Relaxed);
        LAST_Y.store(y as u32, Ordering::Relaxed);
        unsafe { (api.mouse_move)(x as i32, y as i32) };
    }
}

pub fn mouse_button(button: usize, down: bool) {
    if let Some(api) = API.get() {
        let (x, y) = (LAST_X.load(Ordering::Relaxed) as i32, LAST_Y.load(Ordering::Relaxed) as i32);
        unsafe { (api.mouse_button)(button as i32, down as i32, x, y) };
    }
}

pub fn mouse_wheel(delta: i32) {
    if let Some(api) = API.get() {
        let (x, y) = (LAST_X.load(Ordering::Relaxed) as i32, LAST_Y.load(Ordering::Relaxed) as i32);
        unsafe { (api.mouse_wheel)(x, y, delta) };
    }
}

/// type: 0 key down, 1 key up, 2 character.
pub fn key(kind: i32, vk: i32, lparam: isize, modifiers: i32) {
    if let Some(api) = API.get() {
        unsafe { (api.key)(kind, vk, lparam as i32, modifiers) };
    }
}

/// The texture the frames are copied into, and what was last uploaded.
struct Layer {
    texture: Option<IDirect3DTexture9>,
    size: (u32, u32),
    version: u32,
}

struct Shared(Layer);
unsafe impl Send for Shared {}

static LAYER: Mutex<Shared> = Mutex::new(Shared(Layer { texture: None, size: (0, 0), version: 0 }));

/// Copies a new frame into the texture and draws it over the screen. Call before the UI, at Present.
pub fn draw(device: &IDirect3DDevice9) {
    let Some(api) = API.get() else { return };
    if !VISIBLE.load(Ordering::Relaxed) {
        return;
    }
    let mut viewport = D3DVIEWPORT9::default();
    if unsafe { device.GetViewport(&mut viewport) }.is_err() || viewport.Width == 0 {
        return;
    }
    let (width, height) = (viewport.Width, viewport.Height);
    let mut layer = LAYER.lock().unwrap();
    let layer = &mut layer.0;
    if layer.size != (width, height) || layer.texture.is_none() {
        let mut texture: Option<IDirect3DTexture9> = None;
        let created = unsafe {
            device.CreateTexture(width, height, 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_MANAGED, &mut texture, std::ptr::null_mut::<windows::Win32::Foundation::HANDLE>())
        };
        if created.is_err() || texture.is_none() {
            log!("web: could not create the {width}x{height} texture: {created:?}");
            return;
        }
        layer.texture = texture;
        layer.size = (width, height);
        layer.version = 0;
        unsafe { (api.resize)(width as i32, height as i32) };
    }
    let Some(texture) = layer.texture.clone() else { return };
    let version = unsafe { (api.frame_version)() };
    if version != layer.version {
        let mut locked = D3DLOCKED_RECT::default();
        if unsafe { texture.LockRect(0, &mut locked, std::ptr::null(), 0) }.is_ok() {
            let copied = unsafe { (api.copy_frame)(locked.pBits as *mut u8, locked.Pitch, width as i32, height as i32) };
            let _ = unsafe { texture.UnlockRect(0) };
            if copied != 0 {
                layer.version = copied;
            }
        }
    }
    if layer.version != 0 {
        imgui_dx9::draw_texture(device, &texture, width as f32, height as f32);
    }
}

#[allow(dead_code)]
fn unused(_: *const c_void) {}
