//! Direct-to-online boot: skip profile/save selection and the story scripts so the game
//! loads straight into an empty single-player world that we drive ourselves.
//!
//! Research and addresses (GTAIV.exe 1.2.0.59): `docs/boot-flow-research.md`.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::{log, memory};

// Not applied for now: skipping the legal/logo intro wait (GameThread_Main, sig
// `80 3D ?? ?? ?? ?? 00 74 74 80 3D ?? ?? ?? ?? 00 74 6B E8` +7, `74 74` -> `EB 74`). It was the
// first suspect for the crash while loading: the intro also covers engine init.

struct Patch {
    name: &'static str,
    /// Signature of the site; `offset` is added to the match.
    pattern: &'static str,
    offset: usize,
    /// Bytes expected at the site before patching (safety check).
    original: &'static [u8],
    bytes: &'static [u8],
}

const PATCHES: &[Patch] = &[
    // Boot_TitleScreenWait: queue no frontend action instead of the title screen (0xE)...
    Patch {
        name: "no title screen action",
        pattern: "33 F6 6A 0E 89 74 24 14 E8",
        offset: 2,
        original: &[0x6A, 0x0E],
        bytes: &[0x6A, 0x00],
    },
    // ...and run its wait loop once instead of until the title is confirmed.
    Patch {
        name: "skip title screen wait",
        pattern: "83 FE 01 0F 85 64 FF FF FF 6A 00 B9",
        offset: 3,
        original: &[0x0F, 0x85, 0x64, 0xFF, 0xFF, 0xFF],
        bytes: &[0x90, 0x90, 0x90, 0x90, 0x90, 0x90],
    },
    // Boot_InitialLoadFlow loop head: `push 1; call Sleep` -> `jmp 0x5C1DE6` (mov esi, 9).
    // Skips profile sign-in, storage device and autoload-save states and goes to the
    // "continue" exit, which loads the world with fromSave = true and no save data.
    Patch {
        name: "skip boot profile/save states",
        pattern: "6A 01 FF D3 E8 ?? ?? ?? ?? 83 FE 06 0F 87",
        offset: 0,
        original: &[0x6A, 0x01, 0xFF, 0xD3, 0xE8],
        bytes: &[0xE9, 0xAE, 0x00, 0x00, 0x00],
    },
    // State 9 episode pick: `cmp byte [ep_override],0; mov ecx,[ep]; cmovnz ecx,[ep_cmdline]`
    // -> `xor ecx, ecx` + NOPs, so we always load episode 0 (GTA IV).
    Patch {
        name: "force episode 0",
        pattern: "80 3D ?? ?? ?? ?? 00 8B 0D ?? ?? ?? ?? 0F 45 0D ?? ?? ?? ?? 6A 00 6A 01 B2 01",
        offset: 0,
        original: &[0x80, 0x3D],
        bytes: &[
            0x33, 0xC9, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, //
            0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90,
        ],
    },
    // CGame_Process: with the pause menu open the world update flag (0x018B6EDF) is cleared
    // unless the game thinks it is in a network session. Keep it set so the world never freezes.
    Patch {
        name: "world keeps running in the pause menu",
        pattern: "C6 05 ?? ?? ?? ?? 00 84 DB 0F 84 85 00 00 00 8B 35",
        offset: 6,
        original: &[0x00],
        bytes: &[0x01],
    },
    // CTheScripts_InitAndLaunchStartup: `jnz` -> `jmp`, never launch `startup` / `network_startup`.
    Patch {
        name: "no startup script",
        pattern: "80 7D 08 00 C6 05 ?? ?? ?? ?? 00 75 64 E8",
        offset: 11,
        original: &[0x75, 0x64],
        bytes: &[0xEB, 0x64],
    },
];

/// With `network_test`: skip only the story `startup` script and let `network_startup` run
/// (it sets up the multiplayer ped, HUD and menus). `jz startup` -> `jz exit`.
const ONLY_SKIP_STORY_SCRIPT: Patch = Patch {
    name: "skip story startup script only",
    pattern: "80 7D 08 00 C6 05 ?? ?? ?? ?? 00 75 64 E8 ?? ?? ?? ?? 84 C0 74 07",
    offset: 20,
    original: &[0x74, 0x07],
    bytes: &[0x74, 0x5B],
};

/// The startup script site once "no startup script" is applied (`jnz` became `jmp`).
const STARTUP_SCRIPTS_SKIPPED: &str = "80 7D 08 00 C6 05 ?? ?? ?? ?? 00 EB 64 E8 ?? ?? ?? ?? 84 C0 74 07";

/// The network session has started loading (the startup scripts launch while it loads).
static SESSION_LOADING: AtomicBool = AtomicBool::new(false);

/// `gamemode = -1` (told by the server, before the session loads): give the host the game's own
/// multiplayer lobby back. Undo "no startup script" and skip only the story `startup` script
/// ([`ONLY_SKIP_STORY_SCRIPT`]), so `network_startup` runs: it hosts and opens the lobby, where
/// the host picks the game. Only on the host: on a joiner the script's session calls ran into
/// IVMP's join and the game thread hung in the join code (0x47CF9F); joiners join as in peer
/// to peer and get the game when the host starts it. Called from the network thread.
fn use_game_lobby(mode: i32) {
    use ivmp_networking::net;
    if mode != net::LOBBY_MODE {
        return;
    }
    if net::our_id().is_none() || net::our_id() != net::host_id() {
        log!("boot: gamemode -1: we join the host's lobby, no lobby script here");
        return;
    }
    if SESSION_LOADING.load(Ordering::SeqCst) {
        log!("boot: gamemode -1 came after the session started loading; no lobby this time");
        return;
    }
    let Some(site) = memory::find(STARTUP_SCRIPTS_SKIPPED) else {
        log!("boot: gamemode -1: startup script site not found, no lobby");
        return;
    };
    // `jmp` -> `jnz` again (offset 11), then `jz startup` -> `jz exit` (offset 20).
    let restored = unsafe { memory::patch(site + 11, &[0x75]) };
    let story_only = unsafe { memory::patch(site + 20, ONLY_SKIP_STORY_SCRIPT.bytes) };
    match (restored, story_only) {
        (Ok(()), Ok(())) => log!("boot: gamemode -1: network_startup runs, players get the game's lobby"),
        (r, s) => log!("boot: gamemode -1: lobby patch failed ({r:?}, {s:?})"),
    }
}

/// With `network_test`: the first world load is already a network game (the LibertyM way, see
/// docs/libertym-notes.md), so the player and world are created as network objects and the
/// engine syncs them. Loading single player first and switching modes left nothing to sync.
const NETWORK_BOOT_PATCHES: &[Patch] = &[
    // Boot_InitialLoadFlow: our jump lands on `mov esi, 9` (continue -> single-player load);
    // state 8 instead is the network game start, which calls Game_StartNetworkSession.
    Patch {
        name: "boot into the network game state",
        pattern: "BE 09 00 00 00 6A 00 B9 ?? ?? ?? ?? C6 05",
        offset: 1,
        original: &[0x09],
        bytes: &[0x08],
    },
    // State 8 only starts the network game when 0x018B6148 == 2; otherwise it falls through to
    // the single-player paths. `jnz` -> nops.
    Patch {
        name: "network game start without the mode check",
        pattern: "83 3D ?? ?? ?? ?? 02 75 55 E8 ?? ?? ?? ?? E8 ?? ?? ?? ?? 8B 35",
        offset: 7,
        original: &[0x75, 0x55],
        bytes: &[0x90, 0x90],
    },
    // State 8 takes the network episode from a pending invite (0x482150), which is -1 without one,
    // and stores it in 0x011D6FE0 for Game_StartNetworkSession. -1 sometimes ended on "The
    // downloadable content required for this autoload is not available". `call` -> `xor eax, eax`
    // so the episode is always 0 (GTA IV).
    Patch {
        name: "network game in episode 0",
        pattern: "E8 ?? ?? ?? ?? 8B 35 ?? ?? ?? ?? 8B F8 8B CE 89 3D",
        offset: 0,
        original: &[0xE8],
        bytes: &[0x31, 0xC0, 0x90, 0x90, 0x90],
    },
    // LibertyM HookRemoveSpecNetChecks: the function keeping the MP_WARNING_5 / MP_WARNING_6
    // counters ("running too slowly", "out of streaming memory"), which kick a player out of the
    // session. Two copies on one PC are slow enough to trip them. `push ecx` -> `ret`.
    Patch {
        name: "no too-slow / streaming memory kicks",
        pattern: "51 A1 ?? ?? ?? ?? 53 55 8B EA 56 8A F1 57",
        offset: 0,
        original: &[0x51],
        bytes: &[0xC3],
    },
];

/// GameMode_Request(mode) (0x942B20): `[0x11D6FC8] = mode; [0x11D6FCC] = 1` (mode change
/// pending, picked up by GameMode_ProcessPendingChange).
const GAME_MODE_REQUEST_PATTERN: &str = "8B 44 24 04 A3 ?? ?? ?? ?? C6 05 ?? ?? ?? ?? 01 C3";
static GAME_MODE_VAR: AtomicUsize = AtomicUsize::new(0);
static GAME_MODE_PENDING: AtomicUsize = AtomicUsize::new(0);

/// GameMode_Request, filtered: the game stays in network mode, network requests go through.
/// Changing the game type in the lobby stores the new match settings (0x47B990) and requests the
/// network game again (native 0x1BC5050E -> 0xBDC960 -> GameMode_Request(1)) so the session
/// reloads with them; ignoring every request left the host on the loading screen for good.
/// Leaving the session (after a race, "Leave" in the lobby, or the game bailing out of a session
/// that failed or lost its connection) asks for single player (0): ignoring that left the host on
/// a loading screen that never ended. There is no single player to go back to, so it becomes a
/// network reload, a fresh session (a fresh lobby in a lobby game, which the others follow). Not
/// more often than every 10 s, so a session that keeps failing doesn't reload in a loop.
extern "C" fn game_mode_request(mode: i32) {
    static LAST_RELOAD: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mode = if mode == 0 {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
        if now.saturating_sub(LAST_RELOAD.load(Ordering::Relaxed)) >= 10_000 {
            LAST_RELOAD.store(now, Ordering::Relaxed);
            log!("boot: the game left its session (asked for single player): loading the network game again");
            1
        } else {
            0
        }
    } else {
        mode
    };
    if mode == 0 {
        log!("boot: ignored a second request for single player within 10 s, staying in network mode");
        return;
    }
    log!("boot: game mode {mode} requested (network game reload)");
    // In a lobby game the host's reload means a new session: the joiners follow it.
    if ivmp_networking::net::lobby_mode() {
        ivmp_networking::net::tell_session_restart();
    }
    unsafe {
        *(GAME_MODE_VAR.load(Ordering::SeqCst) as *mut i32) = mode;
        *(GAME_MODE_PENDING.load(Ordering::SeqCst) as *mut u8) = 1;
    }
}

fn install_game_mode_filter() {
    let Some(site) = memory::find(GAME_MODE_REQUEST_PATTERN) else {
        log!("boot: GameMode_Request not found, mode requests not filtered");
        return;
    };
    unsafe {
        GAME_MODE_VAR.store(std::ptr::read_unaligned((site + 5) as *const u32) as usize, Ordering::SeqCst);
        GAME_MODE_PENDING.store(std::ptr::read_unaligned((site + 11) as *const u32) as usize, Ordering::SeqCst);
        let rel = (game_mode_request as usize as isize - (site as isize + 5)) as i32;
        let mut jump = [0xE9u8, 0, 0, 0, 0];
        jump[1..].copy_from_slice(&rel.to_le_bytes());
        match memory::patch(site, &jump) {
            Ok(()) => log!("boot: game mode stays network (single player requests ignored) at {site:#x}"),
            Err(e) => log!("boot: game mode filter at {site:#x} failed: {e}"),
        }
    }
}

/// Game mode (0x011D6FC8): 1 = network game.
const GAME_MODE: usize = 0x011D_6FC8;
/// Stored network game config (30 dwords) the network game start uses; Free Mode, 32 slots.
const NETWORK_GAME_CONFIG: (&str, usize) = ("B9 1E 00 00 00 BF ?? ?? ?? ?? F3 A5 5F 5E C3", 6);
/// A call that zeroes that config during network init (inside the function called at
/// pattern + 20, at +0x76): nopped so our config survives.
const CONFIG_ZEROING_CALL: (&str, usize, usize) = ("6A 08 6A 3B FF 15 ?? ?? ?? ?? E8", 20, 0x76);

fn apply(patches: &[Patch]) {
    for patch in patches {
        let Some(site) = memory::find(patch.pattern).map(|a| a + patch.offset) else {
            log!("boot: {}: signature not found, skipped", patch.name);
            continue;
        };
        let current = unsafe { std::slice::from_raw_parts(site as *const u8, patch.original.len()) };
        if current != patch.original {
            log!("boot: {} at {site:#x}: unexpected bytes {current:02X?}, skipped", patch.name);
            continue;
        }
        match unsafe { memory::patch(site, patch.bytes) } {
            Ok(()) => log!("boot: {} at {site:#x}", patch.name),
            Err(e) => log!("boot: {} at {site:#x} failed: {e}", patch.name),
        }
    }
}

/// Player_Create (0xBB53B0) starts with `push "player"` (the Niko model) at +4. The host refuses
/// to clone a remote player with that model: the clone create for the second client's player
/// (CNetObjPlayer, 0x4B9340) failed every time. Multiplayer players use the MP ped model.
const PLAYER_MODEL_PUSH: usize = 0x00BB_53B4;
static MP_PLAYER_MODEL: &[u8] = b"M_Y_MULTIPLAYER\0";

fn use_mp_player_model() {
    let site = memory::from_ghidra(PLAYER_MODEL_PUSH);
    unsafe {
        if *(site as *const u8) != 0x68 {
            log!("boot: player model push not found at {site:#x}, keeping Niko");
            return;
        }
        let mut bytes = [0x68u8, 0, 0, 0, 0];
        bytes[1..].copy_from_slice(&(MP_PLAYER_MODEL.as_ptr() as u32).to_le_bytes());
        match memory::patch(site, &bytes) {
            Ok(()) => log!("boot: the player uses the multiplayer model M_Y_MULTIPLAYER"),
            Err(e) => log!("boot: player model patch failed: {e}"),
        }
    }
}

fn install_network_boot() {
    apply(NETWORK_BOOT_PATCHES);
    install_game_mode_filter();
    use_mp_player_model();
    unsafe {
        *(memory::from_ghidra(GAME_MODE) as *mut u32) = 1;
        if let Some(site) = memory::find(NETWORK_GAME_CONFIG.0) {
            let config = std::ptr::read_unaligned((site + NETWORK_GAME_CONFIG.1) as *const u32) as *mut u32;
            std::ptr::write_bytes(config, 0, 30);
            *config = 16; // Free Mode
            *config.add(2) = 32; // slots
            log!("boot: network game config at {config:p} set to Free Mode, 32 slots");
            // The server's game mode and slots replace these once it sends them.
            ivmp_networking::net::set_network_config(config as usize);
        }
        if let Some(site) = memory::find(CONFIG_ZEROING_CALL.0) {
            let call = site + CONFIG_ZEROING_CALL.1;
            let rel = std::ptr::read_unaligned((call + 1) as *const i32);
            let zeroing = (call as isize + 5 + rel as isize) as usize + CONFIG_ZEROING_CALL.2;
            if *(zeroing as *const u8) == 0xE8 {
                match memory::patch(zeroing, &[0x90; 5]) {
                    Ok(()) => log!("boot: network game config is no longer zeroed ({zeroing:#x})"),
                    Err(e) => log!("boot: config zeroing patch failed: {e}"),
                }
            } else {
                log!("boot: no call at the config zeroing site {zeroing:#x}, skipped");
            }
        }
    }
    log!("boot: the first load is a network game");
}

/// True when the game boots straight into a network game (no single-player load first).
pub fn network_boot() -> bool {
    crate::config().network_test
}

/// Applies the boot patches. Must run before the game reaches its boot flow.
pub fn install() {
    log!("boot: scanning {} KB of game code", memory::code_section().len() / 1024);
    // LibertyM route: no network_startup lobby either, our host system (hostsys.rs) hosts;
    // with `gamemode = -1` the lobby comes back ([`use_game_lobby`]).
    apply(PATCHES);
    if network_boot() {
        install_network_boot();
        ivmp_networking::net::on_game_settings(use_game_lobby);
    }
    install_spawn_hook();
    install_menu_guard();
    if crate::config().dlc >= 2 {
        crate::dlc::install();
    }
}

// Without the startup script nothing creates the player ped, and the first world update
// dereferences FindPlayerPed() == null. Boot_InitialLoadFlow calls Game_SpawnPlayerAfterLoad(1)
// right after the world has loaded; we redirect that call, create the player and give the
// spawn code a valid spawn point before running it.

/// Where the player appears. Hove Beach, next to the first safehouse.
const SPAWN: [f32; 3] = [894.0, -500.0, 15.0];
const SPAWN_HEADING: f32 = 0.0;

/// `push 1; call Game_SpawnPlayerAfterLoad` in Boot_InitialLoadFlow (the call is at +2).
const SPAWN_CALL_PATTERN: &str = "6A 01 E8 ?? ?? ?? ?? 8B 0D ?? ?? ?? ?? 83 C4 04 8D 44 24 10";
const FIND_PLAYER_PED: usize = 0x0093_F050;
/// int CreatePlayer(float* xyz): what the CREATE_PLAYER native calls; returns the player index.
const CREATE_PLAYER: usize = 0x00BB_53B0;
const SPAWN_POINT_INDEX: usize = 0x0103_2110;
/// Spawn point table, 0x40 bytes per entry: x, y, z, w, heading (deg), name (0x24 bytes), used flag.
const SPAWN_POINTS: usize = 0x012D_DB00;

/// `call CGame_Process` in GameThread_Main's main loop (the call is at +8).
const GAME_PROCESS_CALL_PATTERN: &str = "0F 45 C1 A2 ?? ?? ?? ?? E8 ?? ?? ?? ?? E8 ?? ?? ?? ?? B9";

/// `call Game_CommitEpisodeSwitch` just before the spawn call in Boot_InitialLoadFlow (at +0).
/// The commit builds the get-up pose table (PoseMatcher_Init) only if a player ped exists, so the
/// player has to be created before it; otherwise the first ragdoll get-up crashes at 0xC71517.
const COMMIT_CALL_PATTERN: &str = "E8 ?? ?? ?? ?? 6A 01 E8 ?? ?? ?? ?? 8B 0D ?? ?? ?? ?? 83 C4 04 8D 44 24 10";

static ORIGINAL_COMMIT: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_SPAWN: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_GAME_PROCESS: AtomicUsize = AtomicUsize::new(0);

/// The CREATE_PLAYER native's handler (0xBB3370) calls CreatePlayer here.
const CREATE_PLAYER_NATIVE_CALL: usize = 0x00BB_339D;
/// The local player's index.
const LOCAL_PLAYER_INDEX: usize = 0x0103_6F14;

/// CREATE_PLAYER from a script when we already made the player: with `gamemode = -1` a joiner
/// starts network_startup after joining, its CREATE_PLAYER made a second player and the game
/// crashed in CreatePlayer (write to null + 0xA60 at 0x9F19C8). Hand back the player we have.
extern "C" fn create_player_from_script(position: *mut [f32; 3]) -> i32 {
    unsafe {
        let find_player_ped: extern "C" fn() -> usize = std::mem::transmute(memory::from_ghidra(FIND_PLAYER_PED));
        if find_player_ped() != 0 {
            let index = *(memory::from_ghidra(LOCAL_PLAYER_INDEX) as *const i32);
            log!("boot: a script asked for the player again, keeping player {index}");
            return index;
        }
        let create_player: extern "C" fn(*mut [f32; 3]) -> i32 = std::mem::transmute(memory::from_ghidra(CREATE_PLAYER));
        create_player(position)
    }
}

fn install_spawn_hook() {
    let site = memory::from_ghidra(CREATE_PLAYER_NATIVE_CALL);
    if unsafe { *(site as *const u8) } == 0xE8 {
        match unsafe { memory::redirect_call(site, create_player_from_script as usize) } {
            Ok(_) => log!("boot: scripts get the existing player from CREATE_PLAYER"),
            Err(e) => log!("boot: CREATE_PLAYER hook failed: {e}"),
        }
    } else {
        log!("boot: no call at {CREATE_PLAYER_NATIVE_CALL:#x}, CREATE_PLAYER not hooked");
    }

    // Safety net for paths that skip Game_SpawnPlayerAfterLoad (e.g. a new-game reload):
    // make sure a player ped exists before every frame's world update.
    match memory::find(GAME_PROCESS_CALL_PATTERN).map(|a| a + 8) {
        Some(site) => match unsafe { memory::redirect_call(site, game_process as usize) } {
            Ok(original) => {
                ORIGINAL_GAME_PROCESS.store(original, Ordering::SeqCst);
                log!("boot: game process hook at {site:#x}");
            }
            Err(e) => log!("boot: game process hook at {site:#x} failed: {e}"),
        },
        None => log!("boot: game process hook: signature not found, skipped"),
    }

    // Every session load (boot, network switch, return to single player, reload) ends with
    // Game_CommitEpisodeSwitch; hook all of its call sites so a player always exists by then.
    match memory::find(COMMIT_CALL_PATTERN) {
        Some(site) => {
            let rel = unsafe { std::ptr::read_unaligned((site + 1) as *const i32) };
            let commit = (site as isize + 5 + rel as isize) as usize;
            ORIGINAL_COMMIT.store(commit, Ordering::SeqCst);
            for call in memory::find_calls_to(commit) {
                match unsafe { memory::redirect_call(call, commit_episode_switch as usize) } {
                    Ok(_) => log!("boot: commit hook at {call:#x}"),
                    Err(e) => log!("boot: commit hook at {call:#x} failed: {e}"),
                }
            }
        }
        None => log!("boot: commit hook: signature not found, skipped"),
    }

    install_network_session_hook();

    let Some(site) = memory::find(SPAWN_CALL_PATTERN).map(|a| a + 2) else {
        log!("boot: spawn hook: signature not found, skipped");
        return;
    };
    match unsafe { memory::redirect_call(site, spawn_player_after_load as usize) } {
        Ok(original) => {
            ORIGINAL_SPAWN.store(original, Ordering::SeqCst);
            log!("boot: spawn hook at {site:#x}");
        }
        Err(e) => log!("boot: spawn hook at {site:#x} failed: {e}"),
    }
}

/// Points the game's spawn point 0 at [`SPAWN`] and selects it.
fn set_spawn_point() {
    unsafe {
        let entry = memory::from_ghidra(SPAWN_POINTS) as *mut u8;
        std::ptr::write_bytes(entry, 0, 0x40);
        let floats = entry as *mut f32;
        floats.add(0).write(SPAWN[0]);
        floats.add(1).write(SPAWN[1]);
        floats.add(2).write(SPAWN[2]);
        floats.add(4).write(SPAWN_HEADING);
        entry.add(0x38).write(1);
        (memory::from_ghidra(SPAWN_POINT_INDEX) as *mut i32).write(0);
    }
}

extern "C" fn commit_episode_switch() {
    set_spawn_point();
    ensure_player();
    unsafe {
        let original: extern "C" fn() = std::mem::transmute(ORIGINAL_COMMIT.load(Ordering::SeqCst));
        original();
    }
    log!("boot: episode committed with player");
}

const SPAWN_PLAYER_AFTER_LOAD: usize = 0x008C_7FC0;
/// Game_StartNetworkSession(CL = loading screen already up): loads the network session.
const START_NETWORK_SESSION: usize = 0x005C_22F0;

static ORIGINAL_START_NETWORK: AtomicUsize = AtomicUsize::new(0);

fn install_network_session_hook() {
    let target = memory::from_ghidra(START_NETWORK_SESSION);
    ORIGINAL_START_NETWORK.store(target, Ordering::SeqCst);
    for call in memory::find_calls_to(target) {
        match unsafe { memory::redirect_call(call, start_network_session as usize) } {
            Ok(_) => log!("boot: network session hook at {call:#x}"),
            Err(e) => log!("boot: network session hook at {call:#x} failed: {e}"),
        }
    }
}

/// The boot path spawns and fades in through Game_SpawnPlayerAfterLoad; a network session load
/// has no such step (network_startup would do it), so the screen stayed on "LOADING...".
extern "fastcall" fn start_network_session(loading_screen_up: u32) {
    SESSION_LOADING.store(true, Ordering::SeqCst);
    unsafe {
        let original: extern "fastcall" fn(u32) = std::mem::transmute(ORIGINAL_START_NETWORK.load(Ordering::SeqCst));
        original(loading_screen_up);
    }
    after_network_load();
}

/// Every hook and the server connection are in (end of the DLL's init thread).
static HOOKS_INSTALLED: AtomicBool = AtomicBool::new(false);

pub fn hooks_installed() {
    HOOKS_INSTALLED.store(true, Ordering::SeqCst);
}

/// g_GameModeCurrent (0x011D6FC4): 1 once a network session is loaded.
const GAME_MODE_CURRENT: usize = 0x011D_6FC4;

/// The first network load can start before [`start_network_session`] is hooked: with FusionFix
/// the intros are skipped and the game boots while we are still scanning, so its load ran
/// unhooked, the screen stayed on "LOADING..." and the host never hosted (its joiner found no
/// session). CGame_Process only runs once the boot load has returned (GameThread_Main), so on
/// the first frames, once our hooks are in, finish what the hook would have done.
fn catch_up_missed_network_load() {
    if SESSION_LOADING.load(Ordering::SeqCst) || !HOOKS_INSTALLED.load(Ordering::SeqCst) || !network_boot() {
        return;
    }
    if unsafe { *(memory::from_ghidra(GAME_MODE_CURRENT) as *const i32) } != 1 {
        return;
    }
    SESSION_LOADING.store(true, Ordering::SeqCst);
    log!("boot: the network session loaded before our hook was in (intros skipped?), finishing the load now");
    after_network_load();
    // The load ran its startup scripts before `gamemode -1` could bring `network_startup` back
    // ([`use_game_lobby`] was too late), so the host starts the lobby script by hand.
    use ivmp_networking::net;
    if net::lobby_mode() && net::our_id().is_some() && net::our_id() == net::host_id() {
        log!("boot: gamemode -1: the session loaded without the lobby script, starting it now");
        ivmp_networking::lobby::start_network_startup();
    }
}

fn after_network_load() {
    unsafe {
        set_spawn_point();
        let spawn: extern "C" fn(u32) = std::mem::transmute(memory::from_ghidra(SPAWN_PLAYER_AFTER_LOAD));
        spawn(0);
    }
    log!("net test: network session loaded, player placed and faded in");
    ivmp_natives::nativelog::enable();
    ivmp_networking::hostsys::on_network_loaded();
    ivmp_networking::session::host();
}

extern "C" fn spawn_player_after_load(after_save_load: u32) {
    set_spawn_point();
    unsafe {
        ensure_player();

        let original: extern "C" fn(u32) = std::mem::transmute(ORIGINAL_SPAWN.load(Ordering::SeqCst));
        original(after_save_load);
        log!("boot: player spawned");
        // Everything is decrypted by now: write the image out for Ghidra (first run only).
        crate::dump::dump_once(crate::dll_dir());
    }
}

/// Creates the player ped at [`SPAWN`] if there is none. Game thread only.
/// Player_Create (0xBB53B0) makes player 0: it calls FindPlayerPed (0x93F050, returns 0 when
/// there is none) and uses that 0 as the index for CPlayerInfo creation (0x93E960). In a network
/// session every machine needs its own physical player index, or the joining copy can't add the
/// host's player (both would be 0). This replaces `call FindPlayerPed; test eax, eax; jnz skip`
/// with `mov eax, slot` + nops while we create the player.
const CREATE_PLAYER_INDEX_SITE: usize = 0x00BB_53FE;
const CREATE_PLAYER_INDEX_ORIGINAL: [u8; 9] = [0xE8, 0x4D, 0x9C, 0xD8, 0xFF, 0x85, 0xC0, 0x75, 0x1A];

/// The client id the IVMP server uses when it hosts the session itself (`Server --host`).
const SERVER_HOST_ID: u8 = 254;

/// Our physical player index. The host (first client) is 0. The host hands joiners free slots
/// from the top (the first joiner became 31 on the host), and a joiner's own player must use
/// the slot the host gave it, so client 2 -> 31, client 3 -> 30, ...
/// When the IVMP server hosts (host id 254), it is slot 0 and gives client `id` slot `32 - id`.
fn player_slot() -> u8 {
    if ivmp_networking::net::host_id() == Some(SERVER_HOST_ID) {
        return ivmp_networking::net::our_id().map_or(0, |id| 32u8.saturating_sub(id).max(1));
    }
    match ivmp_networking::net::our_id() {
        None | Some(0) | Some(1) => 0,
        Some(id) => 33u8.saturating_sub(id).max(1),
    }
}

unsafe fn with_player_slot(slot: u8, create: impl FnOnce() -> i32) -> i32 {
    let site = memory::from_ghidra(CREATE_PLAYER_INDEX_SITE);
    let current = unsafe { std::slice::from_raw_parts(site as *const u8, CREATE_PLAYER_INDEX_ORIGINAL.len()) };
    if slot == 0 || current != CREATE_PLAYER_INDEX_ORIGINAL {
        if slot != 0 {
            log!("boot: player index patch site has unexpected bytes {current:02X?}, using index 0");
        }
        return create();
    }
    let patch = [0xB8, slot, 0, 0, 0, 0x90, 0x90, 0x90, 0x90];
    unsafe {
        if let Err(e) = memory::patch(site, &patch) {
            log!("boot: player index patch failed: {e}");
            return create();
        }
        let index = create();
        let _ = memory::patch(site, &CREATE_PLAYER_INDEX_ORIGINAL);
        index
    }
}

fn ensure_player() {
    unsafe {
        let find_player_ped: extern "C" fn() -> usize = std::mem::transmute(memory::from_ghidra(FIND_PLAYER_PED));
        if find_player_ped() != 0 {
            return;
        }
        let create_player: extern "C" fn(*mut [f32; 3]) -> i32 = std::mem::transmute(memory::from_ghidra(CREATE_PLAYER));
        let mut position = SPAWN;
        let slot = player_slot();
        let index = with_player_slot(slot, || create_player(&mut position));
        log!("boot: created player {index} at {SPAWN:?}, ped {:#x}", find_player_ped());
    }
}

/// GameMode_Request(int mode): 0 = single player, 1 = network. Picked up by
/// GameMode_ProcessPendingChange, which fades out and calls Game_StartNetworkSession.
const GAME_MODE_REQUEST: usize = 0x0094_2B20;
/// Episode Game_StartNetworkSession loads (normally set from encrypted code by the frontend).
const EPISODE_NETWORK: usize = 0x011D_6FE0;
/// Frames to wait in the world before the network test switches modes.
const NETWORK_TEST_DELAY_FRAMES: u32 = 300;

static FRAMES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn network_test_tick() {
    // Booting straight into the network game needs no mode switch.
    if !crate::config().network_test || network_boot() {
        return;
    }
    if FRAMES.fetch_add(1, Ordering::Relaxed) + 1 != NETWORK_TEST_DELAY_FRAMES {
        return;
    }
    unsafe {
        (memory::from_ghidra(EPISODE_NETWORK) as *mut i32).write(0);
        let request: extern "C" fn(i32) = std::mem::transmute(memory::from_ghidra(GAME_MODE_REQUEST));
        request(1);
    }
    log!("net test: requested network game mode");
}


// The M menu of the lobby (the MP frontend, `CViewportFrontend3DScene`): its close routine
// (0x5BD8B0) runs on a null UI object when the setup (0x5B9AA0) never finished, which crashes the
// game thread. Guard it, and log the setup's state once a second while the menu is up.
// The model-info pools (docs/dlc.md): every kind of model definition (peds, vehicles, weapons,
// instances, interiors, the 2dfx attributes, ...) lives in a fixed-size pool `{capacity, used, base}`,
// 12 bytes each in a table at 0x0103AD64, built once at the game's start from the capacity in the
// data section. The base game nearly fills several of them, and a game that also loads the
// episodes' content (TLAD, TBoGT: `dlc` 2 and 3) takes more slots than exist, so the next
// allocation is a garbage slot (a crash in the IDE parser). The capacities are raised before the
// game builds the pools. The game's own capacities, in table order (the first pool, the 28000 base
// model infos, is left alone: the model table has 31000 slots):
const MODEL_POOL_TABLE: usize = 0x0103_AD64;
const MODEL_POOL_CAPACITIES: [u32; 23] = [
    550, 1800, 100, 205, 585, 220, 17000, 300, 11000, 13250, 10900, 2700, 50, 100, 122, 10900, 600, 35, 20, 100, 280, 250, 30,
];

/// Three times the room for the small pools, twice for the big ones (indices into them are 16 bit).
fn raised(capacity: u32) -> u32 {
    if capacity >= 10_000 { capacity * 2 } else { capacity * 3 }
}

pub fn raise_model_pools() {
    if crate::config().dlc < 2 {
        return;
    }
    let mut raised_count = 0;
    for (index, original) in MODEL_POOL_CAPACITIES.iter().enumerate() {
        let address = MODEL_POOL_TABLE + 12 * (index + 1);
        let at = memory::from_ghidra(address);
        let mut current = [0u8; 4];
        if !memory::read(at, &mut current) || u32::from_le_bytes(current) != *original {
            log!("pools: {address:#x} holds {}, not {original}: left alone", u32::from_le_bytes(current));
            continue;
        }
        match unsafe { memory::patch(at, &raised(*original).to_le_bytes()) } {
            Ok(()) => raised_count += 1,
            Err(e) => log!("pools: {address:#x} could not be raised: {e}"),
        }
    }
    log!("pools: {raised_count} of {} model pools raised for the episodes", MODEL_POOL_CAPACITIES.len());
}

static MENU_CLOSE_ORIGINAL: AtomicUsize = AtomicUsize::new(0);
const MENU_CLOSE: usize = 0x005B_D8B0;
const MENU_UI_OBJECT: usize = 0x018B_6EAC;

extern "C" fn menu_close() {
    unsafe {
        if *(memory::from_ghidra(MENU_UI_OBJECT) as *const usize) == 0 {
            log!("menu: close skipped, its UI object was never created");
            return;
        }
        let original: extern "C" fn() = std::mem::transmute(MENU_CLOSE_ORIGINAL.load(Ordering::SeqCst));
        original();
    }
}

fn install_menu_guard() {
    match unsafe { ivmp_core::hook::install(memory::from_ghidra(MENU_CLOSE), menu_close as usize) } {
        Ok(hook) => {
            MENU_CLOSE_ORIGINAL.store(hook.trampoline, Ordering::SeqCst);
            log!("menu: close guard installed");
        }
        Err(e) => log!("menu: close guard failed: {e}"),
    }
}

fn menu_diag() {
    static LAST: AtomicUsize = AtomicUsize::new(0);
    unsafe {
        let byte = |a: usize| *(memory::from_ghidra(a) as *const u8);
        let (active, open) = (byte(0x0116_0C39), byte(0x0116_09F6));
        if active == 0 && open == 0 {
            return;
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as usize);
        if LAST.swap(now, Ordering::Relaxed) == now {
            return;
        }
        let flags = *(memory::from_ghidra(0x018B_6EA4) as *const u32);
        let object = *(memory::from_ghidra(MENU_UI_OBJECT) as *const usize);
        log!(
            "menu: active {active} open {open} resources {} {} flags {flags:#x} ui object {object:#x}",
            byte(0x019D_3022) & 1,
            byte(0x019D_302E) & 1
        );
        // The two assets it waits for are model infos 30 and 31 (descriptor +0x18/+0x1c): their
        // stream slot and status (is_loaded = FUN_008D8400(slot, pool), status & 3 == 1).
        let table = *(memory::from_ghidra(0x016D_D640) as *const usize);
        let pool = *(memory::from_ghidra(0x0104_96E8) as *const i32);
        let is_loaded: extern "C" fn(i32, i32) -> u8 = std::mem::transmute(memory::from_ghidra(0x008D_8400));
        for model in [30usize, 31] {
            let slot = *((table + model * 0x58 + 0x48) as *const i32);
            let loaded = if slot == -1 { 0 } else { is_loaded(slot, pool) };
            log!("menu: model info {model}: stream slot {slot:#x}, loaded {loaded}");
        }
    }
}

extern "C" fn game_process() {
    menu_diag();
    crate::scripts::tick();
    crate::hang::heartbeat();
    crate::focus::hook_input();
    ensure_player();
    catch_up_missed_network_load();
    network_test_tick();
    ivmp_networking::session::watch();
    if crate::config().network_test && ivmp_networking::session::network_ready() {
        ivmp_networking::hostsys::tick();
    }
    unsafe {
        let original: extern "C" fn() = std::mem::transmute(ORIGINAL_GAME_PROCESS.load(Ordering::SeqCst));
        let start = std::time::Instant::now();
        original();
        crate::ui::GAME_TICK_US.store(start.elapsed().as_micros() as u32, Ordering::Relaxed);
    }
}
