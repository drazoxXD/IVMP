use std::sync::atomic::{AtomicUsize, Ordering};

use ivmp_core::{log, memory};

const GAME_MODE_REQUEST_PATTERN: &str =
    "8B 44 24 04 A3 ?? ?? ?? ?? C6 05 ?? ?? ?? ?? 01 C3";
static GAME_MODE_VAR: AtomicUsize = AtomicUsize::new(0);
static GAME_MODE_PENDING: AtomicUsize = AtomicUsize::new(0);

extern "C" fn game_mode_request(mode: i32) {
    static LAST_RELOAD: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mode = if mode == 0 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        if now.saturating_sub(LAST_RELOAD.load(Ordering::Relaxed)) >= 10_000 {
            LAST_RELOAD.store(now, Ordering::Relaxed);
            log!("boot: the game left its session; loading the network game again");
            1
        } else {
            0
        }
    } else {
        mode
    };
    if mode == 0 {
        log!("boot: ignored a second request for single player within 10 s");
        return;
    }
    if ivmp_networking::net::lobby_mode() {
        ivmp_networking::net::tell_session_restart();
    }
    unsafe {
        *(GAME_MODE_VAR.load(Ordering::SeqCst) as *mut i32) = mode;
        *(GAME_MODE_PENDING.load(Ordering::SeqCst) as *mut u8) = 1;
    }
}

pub fn install() {
    let Some(site) = memory::find(GAME_MODE_REQUEST_PATTERN) else {
        log!("boot: GameMode_Request not found, mode requests not filtered");
        return;
    };
    unsafe {
        GAME_MODE_VAR.store(
            std::ptr::read_unaligned((site + 5) as *const u32) as usize,
            Ordering::SeqCst,
        );
        GAME_MODE_PENDING.store(
            std::ptr::read_unaligned((site + 11) as *const u32) as usize,
            Ordering::SeqCst,
        );
        let rel = (game_mode_request as usize as isize - (site as isize + 5)) as i32;
        let mut jump = [0xE9u8, 0, 0, 0, 0];
        jump[1..].copy_from_slice(&rel.to_le_bytes());
        if let Err(e) = memory::patch(site, &jump) {
            log!("boot: game mode filter at {site:#x} failed: {e}");
        }
    }
}
