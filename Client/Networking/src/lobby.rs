//! Auto-start Free Mode: skip the multiplayer lobby UI.
//!
//! What the game does (GTAIV.exe 1.2.0.59):
//! * `network_startup` hosts, then `network_main` starts the lobby script for the game type:
//!   `deathmatch_cr` by default, `freemode_cr` for Free Mode (Tab in the lobby re-hosts).
//! * The lobby script writes the match config at 0x0110E2B0; +0x24 is the lobby state
//!   (1 = lobby, 2 = in game).
//! * The network state machine (0x479C70) leaves the lobby (state 8) for the game (state 10) once
//!   the session is started; NETWORK_START_SESSION (hash 0x58802CE5, impl 0x47BBD0) starts it.
//!
//! So: start the Free Mode lobby script instead of the deathmatch one, write the recorded
//! settings into the match config once the lobby is up, then start the session.
//! This is an experiment; every step is logged.

use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};


use crate::{log, memory};

const MATCH_CONFIG: usize = 0x0110_E2B0;
const LOBBY_STATE: usize = 0x24;
/// impl of NETWORK_START_SESSION: `bool ()`.
const START_SESSION: usize = 0x0047_BBD0;
/// Network game state (0x017F5EA4): 8 = lobby, 10 = in game.
const NETWORK_STATE: usize = 0x017F_5EA4;

/// The settings from the user's recorded Free Mode game (auto-aim off, police off, ...).
/// Field labels are not all verified; the values are copied from that game as-is.
const SETTINGS: [(usize, i32); 16] = [
    (0x2C, 0),
    (0x30, 1),
    (0x34, 7), // game type: Free Mode (1 = Deathmatch)
    (0x38, 0),
    (0x48, 1),
    (0x4C, 1),
    (0x50, 0), // auto-aim off
    (0x54, 0),
    (0x58, 2),
    (0x5C, 4),
    (0x60, 1),
    (0x64, 1),
    (0x68, 0),
    (0x6C, 0),
    (0x70, 1),
    (0x74, 1),
];

/// Frames to wait in the lobby before writing settings and starting.
const LOBBY_SETTLE_FRAMES: u32 = 120;

static STEP: AtomicU8 = AtomicU8::new(0);
static FRAMES_IN_LOBBY: AtomicU32 = AtomicU32::new(0);


// --- Host as Free Mode ---------------------------------------------------------------------------
// network_startup hosts with NETWORK_HOST_GAME(mode, ranked, slots, private, episode, maxTeams)
// (impl 0xBDAF00); those arguments become the first match config fields, and the mode decides the
// game: the recorded Free Mode game had +0x00 = 16 and +0x08 = 32, the default deathmatch 0 and 0.
// Swapping only the lobby script started a deathmatch game, so override the host arguments.

const HOST_GAME: usize = 0x00BD_AF00;
const FREE_MODE: i32 = 16;
const FREE_MODE_SLOTS: i32 = 32;
static ORIGINAL_HOST_GAME: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub fn install_host_override() {
    let target = memory::from_ghidra(HOST_GAME);
    ORIGINAL_HOST_GAME.store(target, Ordering::SeqCst);
    let calls = memory::find_calls_to(target);
    for call in &calls {
        if let Err(e) = unsafe { memory::redirect_call(*call, host_game as usize) } {
            log!("lobby: host override at {call:#x} failed: {e}");
        }
    }
    log!("lobby: host override at {} call sites", calls.len());
}

extern "C" fn host_game(mode: i32, ranked: u32, slots: i32, private: u32, episode: i32, max_teams: i32) -> u32 {
    log!("lobby: script hosts mode {mode}, ranked {ranked}, slots {slots}, private {private}, episode {episode}, teams {max_teams}");
    let (mode, slots) = (FREE_MODE, FREE_MODE_SLOTS);
    log!("lobby: hosting as mode {mode} with {slots} slots instead");
    let original: extern "C" fn(i32, u32, i32, u32, i32, i32) -> u32 =
        unsafe { std::mem::transmute(ORIGINAL_HOST_GAME.load(Ordering::SeqCst)) };
    original(mode, ranked, slots, private, episode, max_teams)
}

unsafe fn config_field(offset: usize) -> *mut i32 {
    (memory::from_ghidra(MATCH_CONFIG) + offset) as *mut i32
}

/// Call once per frame on the game thread while in network mode.
pub fn tick() {
    unsafe {
        let lobby = *config_field(LOBBY_STATE);
        let network_state = *(memory::from_ghidra(NETWORK_STATE) as *const i32);
        match STEP.load(Ordering::Relaxed) {
            0 => {
                if lobby != 1 || network_state != 8 {
                    FRAMES_IN_LOBBY.store(0, Ordering::Relaxed);
                    return;
                }
                if FRAMES_IN_LOBBY.fetch_add(1, Ordering::Relaxed) < LOBBY_SETTLE_FRAMES {
                    return;
                }
                for (offset, value) in SETTINGS {
                    *config_field(offset) = value;
                }
                log!("lobby: wrote Free Mode settings into the match config");
                let start: extern "C" fn() -> bool = std::mem::transmute(memory::from_ghidra(START_SESSION));
                let started = start();
                log!("lobby: start session -> {started}");
                STEP.store(1, Ordering::Relaxed);
            }
            1 => {
                if network_state == 10 || lobby == 2 {
                    log!("lobby: in game (network state {network_state}, lobby state {lobby})");
                    STEP.store(2, Ordering::Relaxed);
                }
            }
            _ => {}
        }
    }
}
