//! Direct-to-online boot: skip profile/save selection and the story scripts so the game
//! loads straight into an empty single-player world that we drive ourselves.
//!
//! Research and addresses (GTAIV.exe 1.2.0.59): `docs/boot-flow-research.md`.

use std::sync::atomic::{AtomicUsize, Ordering};

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
    // GameMode_Request (0x942B20, `mov eax, [esp+4]`): ignore requests, the mode stays network.
    Patch {
        name: "game mode stays network",
        pattern: "8B 44 24 04 A3 ?? ?? ?? ?? C6 05 ?? ?? ?? ?? 01 C3",
        offset: 0,
        original: &[0x8B],
        bytes: &[0xC3],
    },
];

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
    use_mp_player_model();
    unsafe {
        *(memory::from_ghidra(GAME_MODE) as *mut u32) = 1;
        if let Some(site) = memory::find(NETWORK_GAME_CONFIG.0) {
            let config = std::ptr::read_unaligned((site + NETWORK_GAME_CONFIG.1) as *const u32) as *mut u32;
            std::ptr::write_bytes(config, 0, 30);
            *config = 16; // Free Mode
            *config.add(2) = 32; // slots
            log!("boot: network game config at {config:p} set to Free Mode, 32 slots");
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
    // LibertyM route: no network_startup lobby either, our host system (hostsys.rs) hosts.
    // ONLY_SKIP_STORY_SCRIPT is kept for going back to the script-driven lobby.
    let _ = &ONLY_SKIP_STORY_SCRIPT;
    apply(PATCHES);
    if network_boot() {
        install_network_boot();
    }
    install_spawn_hook();
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

fn install_spawn_hook() {
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
    unsafe {
        let original: extern "fastcall" fn(u32) = std::mem::transmute(ORIGINAL_START_NETWORK.load(Ordering::SeqCst));
        original(loading_screen_up);
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

extern "C" fn game_process() {
    crate::hang::heartbeat();
    crate::focus::hook_input();
    ensure_player();
    network_test_tick();
    ivmp_networking::session::watch();
    if crate::config().network_test && ivmp_networking::session::network_ready() {
        ivmp_networking::hostsys::tick();
    }
    unsafe {
        let original: extern "C" fn() = std::mem::transmute(ORIGINAL_GAME_PROCESS.load(Ordering::SeqCst));
        original();
    }
}
