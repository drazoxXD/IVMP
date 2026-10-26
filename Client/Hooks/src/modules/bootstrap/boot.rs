//! Direct-to-online boot: skip profile/save selection and the story scripts so the game
//! loads straight into an empty single-player world that we drive ourselves.
//!
//! Research and addresses (GTAIV.exe 1.2.0.59): `docs/boot-flow-research.md`.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::{log, memory};

// Not applied for now: skipping the legal/logo intro wait (GameThread_Main, sig
// `80 3D ?? ?? ?? ?? 00 74 74 80 3D ?? ?? ?? ?? 00 74 6B E8` +7, `74 74` -> `EB 74`). It was the
// first suspect for the crash while loading: the intro also covers engine init.

/// True when the game boots straight into a network game (no single-player load first).
pub fn network_boot() -> bool {
    crate::config().network_test
}

/// Installs the boot hooks and delegates byte patches to the Patches crate.
pub fn install() {
    log!("boot: scanning {} KB of game code", memory::code_section().len() / 1024);
    ivmp_patches::boot::install_direct_boot();
    if network_boot() {
        ivmp_patches::boot::install_network_boot();
        ivmp_networking::net::on_game_settings(ivmp_patches::boot::use_game_lobby);
    }
    install_spawn_hook();
    install_menu_guard();
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
    ivmp_patches::boot::mark_session_loading();
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
    if ivmp_patches::boot::session_loading() || !HOOKS_INSTALLED.load(Ordering::SeqCst) || !network_boot() {
        return;
    }
    if unsafe { *(memory::from_ghidra(GAME_MODE_CURRENT) as *const i32) } != 1 {
        return;
    }
    ivmp_patches::boot::mark_session_loading();
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