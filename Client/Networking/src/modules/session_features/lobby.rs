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
// Swapping only the lobby script started a deathmatch game, so override the host arguments with
// the server's game mode and slots (server.cfg, `net::game_settings`).

const HOST_GAME: usize = 0x00BD_AF00;
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
    let original: extern "C" fn(i32, u32, i32, u32, i32, i32) -> u32 =
        unsafe { std::mem::transmute(ORIGINAL_HOST_GAME.load(Ordering::SeqCst)) };
    // `gamemode = -1`: the first player (the server's host) hosts what was picked in the lobby;
    // everyone else joins that session (hostsys.rs), so their script must not host its own.
    if crate::net::lobby_mode() {
        if crate::net::our_id().is_some() && crate::net::our_id() != crate::net::host_id() {
            log!("lobby: gamemode -1 and we aren't the host: not hosting, joining the host instead");
            return 0;
        }
        // The script passed garbage for `ranked` (44037632) in the first lobby test; joiners
        // look for unranked games, as IVMP always hosts them.
        if ranked != 0 {
            log!("lobby: hosting unranked (the script asked for ranked {ranked})");
        }
        return original(mode, 0, slots, private, episode, max_teams);
    }
    let (mode, slots) = crate::net::game_settings();
    let (mode, slots) = (mode, slots as i32);
    log!("lobby: hosting as mode {mode} with {slots} slots instead");
    original(mode, ranked, slots, private, episode, max_teams)
}

// --- Starting network_startup by hand ------------------------------------------------------------
// CTheScripts_InitAndLaunchStartup (0x94C63B, launch at 0x94CA43..0x94CAA4) starts
// "network_startup" in a network game: index by name (0x94B5A0), request (0x94EC40 (index, 0),
// 0x8D8870 (index, module 0x10475B8)), load what was requested (0x8D8570 (0)), start a thread
// on the program (0xBDE840 (index)) with 0x400 bytes of stack (0x86EE60 (program, 0, 0, 0x400)),
// release the request (0x8D8830 (index, module)). All cdecl. With `gamemode = -1` a joiner
// boots without it (its session calls collided with IVMP's join) and starts it here once it
// is in the host's session, so it gets the lobby too.

const SCRIPT_INDEX_BY_NAME: usize = 0x0094_B5A0;
const SCRIPT_REQUEST: usize = 0x0094_EC40;
const SCRIPT_ADD_REF: usize = 0x008D_8870;
const LOAD_REQUESTED: usize = 0x008D_8570;
const SCRIPT_PROGRAM: usize = 0x00BD_E840;
const START_SCRIPT_THREAD: usize = 0x0086_EE60;
const SCRIPT_RELEASE: usize = 0x008D_8830;
const SCRIPT_MODULE: usize = 0x0104_75B8;
const NETWORK_STARTUP: &[u8] = b"network_startup\0";

/// Starts `network_startup` the way the game's startup code does. Game thread only.
pub fn start_network_startup() {
    unsafe {
        let index_by_name: extern "C" fn(*const u8) -> i32 = std::mem::transmute(memory::from_ghidra(SCRIPT_INDEX_BY_NAME));
        let request: extern "C" fn(i32, i32) = std::mem::transmute(memory::from_ghidra(SCRIPT_REQUEST));
        let add_ref: extern "C" fn(i32, u32) = std::mem::transmute(memory::from_ghidra(SCRIPT_ADD_REF));
        let load_requested: extern "C" fn(i32) = std::mem::transmute(memory::from_ghidra(LOAD_REQUESTED));
        let program: extern "C" fn(i32) -> usize = std::mem::transmute(memory::from_ghidra(SCRIPT_PROGRAM));
        let start: extern "C" fn(usize, usize, i32, i32) -> i32 = std::mem::transmute(memory::from_ghidra(START_SCRIPT_THREAD));
        let release: extern "C" fn(i32, u32) = std::mem::transmute(memory::from_ghidra(SCRIPT_RELEASE));
        let module = *(memory::from_ghidra(SCRIPT_MODULE) as *const u32);
        let index = index_by_name(NETWORK_STARTUP.as_ptr());
        if index < 0 {
            log!("lobby: network_startup not found ({index})");
            return;
        }
        request(index, 0);
        add_ref(index, module);
        load_requested(0);
        let thread = start(program(index), 0, 0, 0x400);
        release(index, module);
        log!("lobby: started network_startup (script {index}, thread {thread})");
    }
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
