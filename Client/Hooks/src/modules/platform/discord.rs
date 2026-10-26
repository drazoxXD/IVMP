//! Discord Rich Presence via Discord IPC named pipes.
//!
//! Connects to `\\.\pipe\discord-ipc-0..9`, handles handshakes, and dynamically updates
//! presence details, state, and player counts.

use std::fs::File;
use std::io::{Read, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};

unsafe extern "system" {
    fn PeekNamedPipe(
        hNamedPipe: HANDLE,
        lpBuffer: *mut std::ffi::c_void,
        nBufferSize: u32,
        lpBytesRead: *mut u32,
        lpTotalBytesAvail: *mut u32,
        lpBytesLeftThisMessage: *mut u32,
    ) -> i32;
}

const APP_ID: &str = "1564012472975761448";

#[derive(Debug, PartialEq, Eq, Clone)]
struct ActivityState {
    details: String,
    state: String,
}

fn try_connect_pipe() -> Option<File> {
    for i in 0..10 {
        let pipe_path = format!("\\\\.\\pipe\\discord-ipc-{i}\0");
        let wide: Vec<u16> = pipe_path.encode_utf16().collect();
        let handle = unsafe {
            CreateFileW(
                PCWSTR::from_raw(wide.as_ptr()),
                (GENERIC_READ | GENERIC_WRITE).0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        };
        if let Ok(h) = handle {
            if h != INVALID_HANDLE_VALUE {
                let file = unsafe { File::from_raw_handle(h.0 as _) };
                return Some(file);
            }
        }
    }
    None
}

fn send_packet(file: &mut File, opcode: u32, payload: &str) -> std::io::Result<()> {
    let len = payload.len() as u32;
    file.write_all(&opcode.to_le_bytes())?;
    file.write_all(&len.to_le_bytes())?;
    file.write_all(payload.as_bytes())?;
    file.flush()?;
    Ok(())
}

fn perform_handshake(file: &mut File) -> std::io::Result<()> {
    let handshake_json = serde_json::json!({
        "v": 1,
        "client_id": APP_ID
    })
    .to_string();
    send_packet(file, 0, &handshake_json)?;

    let mut header = [0u8; 8];
    file.read_exact(&mut header)?;
    let length = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
    let mut body = vec![0u8; length];
    file.read_exact(&mut body)?;
    Ok(())
}

fn drain_pipe(file: &mut File) {
    let raw = file.as_raw_handle();
    loop {
        let mut avail: u32 = 0;
        let ok = unsafe {
            PeekNamedPipe(
                HANDLE(raw as _),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut avail,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 || avail == 0 {
            break;
        }
        let to_read = (avail as usize).min(4096);
        let mut buf = vec![0u8; to_read];
        if file.read(&mut buf).is_err() {
            break;
        }
    }
}

fn send_activity(file: &mut File, activity: &ActivityState, start_time: u64, nonce: u64) -> std::io::Result<()> {
    let payload = serde_json::json!({
        "cmd": "SET_ACTIVITY",
        "args": {
            "pid": std::process::id(),
            "activity": {
                "details": activity.details,
                "state": activity.state,
                "assets": {
                    "large_image": "logo",
                    "large_text": "IVMP"
                },
                "timestamps": {
                    "start": start_time
                }
            }
        },
        "nonce": nonce.to_string()
    })
    .to_string();
    send_packet(file, 1, &payload)
}

/// Spawns a background thread that manages Discord Rich Presence connection and updates.
pub fn install() {
    std::thread::spawn(move || {
        crate::log!("discord: starting Discord RPC worker (App ID: {APP_ID})");
        let start_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let mut pipe: Option<File> = None;
        let mut last_activity: Option<ActivityState> = None;
        let mut last_send = Instant::now() - Duration::from_secs(60);
        let mut nonce = 0u64;

        loop {
            std::thread::sleep(Duration::from_millis(1500));

            // Drain any pending responses to prevent pipe buffer from filling up
            if let Some(ref mut f) = pipe {
                drain_pipe(f);
            }

            // Ensure we are connected to Discord IPC pipe
            if pipe.is_none() {
                if let Some(mut new_pipe) = try_connect_pipe() {
                    match perform_handshake(&mut new_pipe) {
                        Ok(()) => {
                            crate::log!("discord: connected to Discord IPC pipe");
                            pipe = Some(new_pipe);
                            last_activity = None;
                        }
                        Err(e) => {
                            crate::log!("discord: handshake failed: {e}");
                        }
                    }
                }
            }

            let Some(ref mut f) = pipe else {
                continue;
            };

            // Format activity details and state based on network connection
            let current_activity = if ivmp_networking::net::is_connected() {
                let s_name = ivmp_networking::net::server_name();
                let (current, max) = ivmp_networking::net::player_count();
                ActivityState {
                    details: format!("Connected to {s_name}"),
                    state: format!("Players {current}/{max}"),
                }
            } else if ivmp_networking::net::status().starts_with("Connecting") {
                let s_name = ivmp_networking::net::server_name();
                ActivityState {
                    details: "Connecting to server...".to_string(),
                    state: format!("Connecting to {s_name}"),
                }
            } else {
                ActivityState {
                    details: "In Main Menu".to_string(),
                    state: "Grand Theft Auto IV".to_string(),
                }
            };

            // Send activity if state changed or heartbeat due (every 15s)
            let changed = last_activity.as_ref() != Some(&current_activity);
            let heartbeat_due = last_send.elapsed() >= Duration::from_secs(15);

            if changed || heartbeat_due {
                nonce += 1;
                if let Err(e) = send_activity(f, &current_activity, start_time, nonce) {
                    crate::log!("discord: error updating activity: {e}, resetting connection");
                    pipe = None;
                    last_activity = None;
                } else {
                    last_activity = Some(current_activity);
                    last_send = Instant::now();
                }
            }
        }
    });
}
