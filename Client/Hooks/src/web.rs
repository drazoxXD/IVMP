//! The web layer: CEF runs in its own process (`IVMP_Web.exe` in the `cef` folder next to
//! IVMP_Client.dll), so no CEF thread or window lives in the game (the Steam overlay's hooks
//! crashed on those). The game talks to it over a named pipe and reads the page's frames from a
//! shared-memory mapping, copies them into a Direct3D 9 texture and draws that under imgui.
//! See docs/web-engine-research.md.
//!
//! F2 shows or hides the page (and gives it the keyboard and mouse); the first F2 starts the
//! process. Without the `cef` folder nothing happens: the web layer is an optional download.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows::Win32::Graphics::Direct3D9::*;
use windows::Win32::System::Memory::{FILE_MAP_READ, MapViewOfFile, OpenFileMappingW};
use windows::core::HSTRING;

use crate::{imgui_dx9, log};

/// The page the layer opens (served from `cef\ui` by the ivmp:// scheme).
const START_URL: &str = "ivmp://ui/index.html";

// Messages to the web process (u32 length, u8 type, payload) and back.
const TO_VISIBLE: u8 = 1;
const TO_FOCUS: u8 = 2;
const TO_RESIZE: u8 = 3;
const TO_MOUSE_MOVE: u8 = 4;
const TO_MOUSE_BUTTON: u8 = 5;
const TO_MOUSE_WHEEL: u8 = 6;
const TO_KEY: u8 = 7;
const TO_EMIT: u8 = 8;
const FROM_LOG: u8 = 0x81;
const FROM_BRIDGE: u8 = 0x82;
const FROM_READY: u8 = 0x83;

const FRAME_HEADER: usize = 16;

/// Shared frame: header (sequence, width, height, reserved) then BGRA.
struct Frame {
    base: *const u8,
}
unsafe impl Send for Frame {}
unsafe impl Sync for Frame {}

struct Host {
    pipe: Mutex<File>,
    frame: Frame,
}

static HOST: OnceLock<Host> = OnceLock::new();
static VISIBLE: AtomicBool = AtomicBool::new(false);
static LAST_X: AtomicU32 = AtomicU32::new(0);
static LAST_Y: AtomicU32 = AtomicU32::new(0);
/// 0 not started, 1 starting, 2 ready, 3 failed.
static STATE: AtomicU8 = AtomicU8::new(0);

pub fn visible() -> bool {
    VISIBLE.load(Ordering::Relaxed) && HOST.get().is_some()
}

fn send(kind: u8, payload: &[u8]) {
    let Some(host) = HOST.get() else { return };
    let mut message = Vec::with_capacity(5 + payload.len());
    message.extend_from_slice(&(payload.len() as u32 + 1).to_le_bytes());
    message.push(kind);
    message.extend_from_slice(payload);
    let _ = host.pipe.lock().unwrap().write_all(&message);
}

fn ints(values: &[i32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// Notes whether the cef folder is there. CEF itself starts on the first F2, not with the game.
pub fn install() {
    let folder = crate::dll_dir().join("cef");
    if folder.join("IVMP_Web.exe").exists() {
        log!("web: cef folder found, F2 starts the web layer");
    } else {
        log!("web: no cef folder next to the client ({}), the web layer is off", folder.display());
    }
}

fn fail(why: &str) {
    log!("web: {why}");
    STATE.store(3, Ordering::SeqCst);
}

/// Starts IVMP_Web.exe, connects to its pipe and maps its frame. Runs on its own thread.
fn start() {
    let folder = crate::dll_dir().join("cef");
    let exe = folder.join("IVMP_Web.exe");
    if !exe.exists() {
        return fail(&format!("no cef folder ({}), nothing to start", folder.display()));
    }
    let pid = std::process::id();
    let pipe_name = format!("ivmp_web_{pid}");
    let frame_name = format!("Local\\ivmp_web_frame_{pid}");
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    log!("web: starting {}", exe.display());
    let child = std::process::Command::new(&exe)
        .current_dir(&folder)
        .arg(format!("--ivmp-pipe={pipe_name}"))
        .arg(format!("--ivmp-frame={frame_name}"))
        .arg(format!("--ivmp-game-pid={pid}"))
        .arg(format!("--ivmp-url={START_URL}"))
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(e) => return fail(&format!("could not start IVMP_Web.exe: {e}")),
    };

    // The process creates the pipe and the mapping; wait for them.
    let started = Instant::now();
    // Two pipes, one per direction (a synchronous handle with a pending read blocks writes).
    let mut open = |suffix: &str, write: bool| -> Result<File, String> {
        let name = format!(r"\\.\pipe\{pipe_name}{suffix}");
        let started = Instant::now();
        loop {
            match OpenOptions::new().read(!write).write(write).open(&name) {
                Ok(file) => return Ok(file),
                Err(_) if started.elapsed() < Duration::from_secs(20) => {
                    if let Ok(Some(status)) = child.try_wait() {
                        return Err(format!("IVMP_Web.exe exited early ({status})"));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => return Err(format!("no pipe {suffix} from IVMP_Web.exe: {e}")),
            }
        }
    };
    let reader = match open("_out", false) {
        Ok(file) => file,
        Err(why) => return fail(&why),
    };
    let pipe = match open("_in", true) {
        Ok(file) => file,
        Err(why) => return fail(&why),
    };
    let mapping = match unsafe { OpenFileMappingW(FILE_MAP_READ.0, false, &HSTRING::from(frame_name.as_str())) } {
        Ok(handle) => handle,
        Err(e) => return fail(&format!("no frame mapping from IVMP_Web.exe: {e}")),
    };
    let view = unsafe { MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, 0) };
    if view.Value.is_null() {
        return fail("could not map the frame");
    }
    let _ = HOST.set(Host { pipe: Mutex::new(pipe), frame: Frame { base: view.Value as *const u8 } });
    std::thread::spawn(move || read_messages(reader));
    // Hidden until shown (the first F2 asked for it, so show it now).
    send(TO_VISIBLE, &ints(&[0]));
    STATE.store(2, Ordering::SeqCst);
    log!("web: connected to IVMP_Web.exe");
    toggle();
}

/// Messages from the web process: its log, the page's ivmp.send messages (the one channel out of
/// the page; the script runner will take these, for now they are logged), ready.
fn read_messages(mut pipe: File) {
    loop {
        let mut length = [0u8; 4];
        if pipe.read_exact(&mut length).is_err() {
            log!("web: the web process closed the pipe");
            STATE.store(3, Ordering::SeqCst);
            VISIBLE.store(false, Ordering::Relaxed);
            return;
        }
        let length = u32::from_le_bytes(length) as usize;
        if length == 0 || length > (1 << 20) {
            return;
        }
        let mut body = vec![0u8; length];
        if pipe.read_exact(&mut body).is_err() {
            return;
        }
        let text = String::from_utf8_lossy(&body[1..]).to_string();
        match body[0] {
            FROM_LOG => log!("{text}"),
            FROM_BRIDGE => log!("web: bridge: {text}"),
            FROM_READY => log!("web: CEF is ready"),
            _ => {}
        }
    }
}

/// F2: show or hide the page (and give it, or take back, the keyboard and mouse).
pub fn toggle() {
    if HOST.get().is_none() {
        // First use: start the process (on its own thread); the page opens when it is connected.
        if STATE.compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
            std::thread::spawn(start);
        }
        return;
    }
    let show = !VISIBLE.load(Ordering::Relaxed);
    VISIBLE.store(show, Ordering::Relaxed);
    send(TO_VISIBLE, &ints(&[show as i32]));
    send(TO_FOCUS, &ints(&[show as i32]));
    if show {
        crate::ui::reset_raw_cursor();
        // Tell the page the game is here (the other direction of the bridge).
        let mut payload = b"game.hello\0".to_vec();
        payload.extend_from_slice(br#"{"from":"IVMP client"}"#);
        send(TO_EMIT, &payload);
    }
}

pub fn close() {
    if VISIBLE.swap(false, Ordering::Relaxed) {
        send(TO_VISIBLE, &ints(&[0]));
        send(TO_FOCUS, &ints(&[0]));
    }
}

pub fn mouse_move(x: f32, y: f32) {
    LAST_X.store(x as u32, Ordering::Relaxed);
    LAST_Y.store(y as u32, Ordering::Relaxed);
    send(TO_MOUSE_MOVE, &ints(&[x as i32, y as i32]));
}

pub fn mouse_button(button: usize, down: bool) {
    let (x, y) = (LAST_X.load(Ordering::Relaxed) as i32, LAST_Y.load(Ordering::Relaxed) as i32);
    send(TO_MOUSE_BUTTON, &ints(&[button as i32, down as i32, x, y]));
}

pub fn mouse_wheel(delta: i32) {
    let (x, y) = (LAST_X.load(Ordering::Relaxed) as i32, LAST_Y.load(Ordering::Relaxed) as i32);
    send(TO_MOUSE_WHEEL, &ints(&[x, y, delta]));
}

/// type: 0 key down, 1 key up, 2 character.
pub fn key(kind: i32, vk: i32, lparam: isize, modifiers: i32) {
    send(TO_KEY, &ints(&[kind, vk, lparam as i32, modifiers]));
}

/// Copies the latest frame (width x height) into `destination` (`pitch` bytes per row). Returns
/// the frame's sequence number, or 0 when there is no frame of that size or it was being written.
fn copy_frame(host: &Host, destination: *mut u8, pitch: i32, width: u32, height: u32) -> u32 {
    let base = host.frame.base;
    for _ in 0..3 {
        let header = base as *const u32;
        let before = unsafe { std::ptr::read_volatile(header) };
        let (w, h) = unsafe { (std::ptr::read_volatile(header.add(1)), std::ptr::read_volatile(header.add(2))) };
        if before == 0 || before & 1 == 1 || w != width || h != height {
            return 0;
        }
        for row in 0..height as usize {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    base.add(FRAME_HEADER + row * width as usize * 4),
                    destination.add(row * pitch as usize),
                    width as usize * 4,
                );
            }
        }
        std::sync::atomic::fence(Ordering::SeqCst);
        if unsafe { std::ptr::read_volatile(header) } == before {
            return before;
        }
    }
    0
}

fn frame_sequence(host: &Host) -> u32 {
    unsafe { std::ptr::read_volatile(host.frame.base as *const u32) }
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

/// The game resets the device: let go of the texture (it is made again on the next frame).
pub fn device_lost() {
    let mut layer = LAYER.lock().unwrap();
    layer.0.texture = None;
    layer.0.size = (0, 0);
}

/// Copies a new frame into the texture and draws it over the screen. Call before the UI, at Present.
pub fn draw(device: &IDirect3DDevice9) {
    let Some(host) = HOST.get() else { return };
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
        send(TO_RESIZE, &ints(&[width as i32, height as i32]));
    }
    let Some(texture) = layer.texture.clone() else { return };
    let sequence = frame_sequence(host);
    if sequence != layer.version {
        let mut locked = D3DLOCKED_RECT::default();
        if unsafe { texture.LockRect(0, &mut locked, std::ptr::null(), 0) }.is_ok() {
            let copied = copy_frame(host, locked.pBits as *mut u8, locked.Pitch, width, height);
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
