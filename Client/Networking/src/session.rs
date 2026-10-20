//! Milestone 1 experiment: make the engine host its own network session without Rockstar's
//! online services, and log what happens.
//!
//! Path (GTAIV.exe 1.2.0.59, decrypted dump):
//! native NETWORK_HOST_GAME (hash 0x5BEA05E2, impl 0xBDAF00) -> CNetwork host game (0x47D130)
//! -> session host (0x4DCC30) -> snSession::Host (0x61FBF0) -> snHostSessionTask -> rlSession host.
//! The byte at 0x017F5FC6 (set by native 0x6FDA43A3) makes 0x47D130 skip the Social Club
//! sign-in checks and host in its LAN mode; we set it before hosting.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};

use crate::{log, memory};

/// void NETWORK_HOST_GAME impl(int gameMode, bool ranked, int slots, bool private, int episode, int maxTeams), cdecl.
const HOST_GAME: usize = 0x00BD_AF00;
/// Byte: LAN / no-Rockstar-services mode.
const LAN_MODE: usize = 0x017F_5FC6;
/// Index of the signed-in local gamer (-1 = none) and the gamer table it indexes.
const LOCAL_GAMER_INDEX: usize = 0x0103_6F14;
const GAMER_TABLE: usize = 0x011A_8808;
/// The game's main session object; its state is at +0x50 (2..3 = hosting / hosted).
const MAIN_SESSION_STATE: usize = 0x0198_2070;
/// netStatus of the last host request: 1 pending, 2 failed, 3 succeeded.
const HOST_STATUS: usize = 0x0190_6A80;
/// Bit 2: the network layer is started.
const NETWORK_FLAGS: usize = 0x0198_B568;

/// `network_startup` calls NETWORK_HOST_GAME itself. Hosting from here as well made its call
/// fail, and the failure path (CNetwork leave game, 0x47CDF0) waits for our pending host task on
/// the game thread: a deadlock. So by default we only switch LAN mode on and let the script host.
const HOST_OURSELVES: bool = false;

/// GAME_MODE guess: Free Mode. Not verified yet.
const GAME_MODE: i32 = 13;
const SLOTS: i32 = 16;

static REQUESTED: AtomicBool = AtomicBool::new(false);
static LAST_STATE: AtomicI32 = AtomicI32::new(i32::MIN);
static LAST_STATUS: AtomicI32 = AtomicI32::new(i32::MIN);
static LAST_FLAGS: AtomicU32 = AtomicU32::new(u32::MAX);

unsafe fn read<T: Copy>(ghidra: usize) -> T {
    unsafe { std::ptr::read_unaligned(memory::from_ghidra(ghidra) as *const T) }
}

/// Asks the engine to host a session. Game thread only, once the network game mode is loaded.
pub fn host() {
    if REQUESTED.swap(true, Ordering::SeqCst) {
        return;
    }
    unsafe {
        let gamer_index: i32 = read(LOCAL_GAMER_INDEX);
        log!("session: local gamer index {gamer_index}");
        if gamer_index < 0 {
            // 0x47D130 would dereference a null gamer; we need to fake a local gamer first.
            log!("session: no local gamer, not hosting yet");
            return;
        }
        let gamer = *(memory::from_ghidra(GAMER_TABLE) as *const usize).add(gamer_index as usize);
        if gamer != 0 {
            let state = *((gamer + 0x50) as *const u8);
            let id = std::ptr::read_unaligned((gamer + 0x38) as *const u64);
            log!("session: gamer {gamer:#x}, sign-in state {state}, id {id:#x}");
        }
        *(memory::from_ghidra(LAN_MODE) as *mut u8) = 1;
        trace_host_task();
        trace_join_task();
        skip_rline_join();
        skip_rline_join_when_joining();
        if HOST_OURSELVES {
            let host_game: extern "C" fn(i32, u8, i32, u8, i32, i32) = std::mem::transmute(memory::from_ghidra(HOST_GAME));
            host_game(GAME_MODE, 0, SLOTS, 0, 0, 0);
            log!("session: host requested (mode {GAME_MODE}, {SLOTS} slots, LAN mode)");
        } else {
            log!("session: LAN mode on, leaving the host call to network_startup");
        }
    }
}

/// True once the network session load has happened (LAN mode set, scripts running).
pub fn network_ready() -> bool {
    REQUESTED.load(Ordering::Relaxed)
}

/// Logs changes of the session state, host status and network flags. Call once per frame.
pub fn watch() {
    if !REQUESTED.load(Ordering::Relaxed) {
        return;
    }
    watch_match_config();
    unsafe {
        let state: i32 = read(MAIN_SESSION_STATE);
        let status: i32 = read(HOST_STATUS);
        let flags: u32 = read(NETWORK_FLAGS);
        if LAST_STATE.swap(state, Ordering::Relaxed) != state {
            log!("session: main session state {state}");
        }
        if LAST_STATUS.swap(status, Ordering::Relaxed) != status {
            log!("session: host status {status} (1 pending, 2 failed, 3 done)");
        }
        if LAST_FLAGS.swap(flags, Ordering::Relaxed) != flags {
            log!("session: network flags {flags:#x}");
        }
    }
}

// --- snHostSessionTask trace -------------------------------------------------------------------
// The session reaches "hosted" and then fails within a frame. To see which step of the host task
// fails, its vtable (0xFE32F0) is patched: Update (slot 3) logs the task state before and after,
// Finish (slot 7) logs the result.

const HOST_TASK_VTABLE: usize = 0x00FE_32F0;
const SLOT_UPDATE: usize = 3;
const SLOT_FINISH: usize = 7;
/// snHostSessionTask: step at +0x90, status of the current step (netStatus) at +0x94.
const TASK_STEP: usize = 0x90;
const TASK_STATUS: usize = 0x94;

static ORIGINAL_UPDATE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static ORIGINAL_FINISH: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// snHostSessionTask step 2 adds the local gamers to the Rockstar online ("rline") session, and
/// only does so if the local gamer's presence is online (0x019F32AC bit 1). Without Rockstar's
/// services it isn't, so the task goes to its failure steps 4/5 and the freshly hosted session is
/// torn down. We replace step 2 with a jump to step 3's success path at 0x66258D
/// (`push 0; push 1; jmp Finish`), i.e. Finish(1, 0).
const SKIP_RLINE_PATTERN: &str = "8B 86 9C 00 00 00 85 C0 75 66 C1 E0 07 F6 80";
const SKIP_RLINE_ORIGINAL: [u8; 6] = [0x8B, 0x86, 0x9C, 0x00, 0x00, 0x00];
const SKIP_RLINE_BYTES: [u8; 6] = [0xEB, 0x63, 0x90, 0x90, 0x90, 0x90];

pub fn skip_rline_join() {
    let Some(site) = memory::find(SKIP_RLINE_PATTERN) else {
        log!("session: rline join step not found, skipped");
        return;
    };
    let current = unsafe { std::slice::from_raw_parts(site as *const u8, 6) };
    if current != SKIP_RLINE_ORIGINAL {
        log!("session: rline join step at {site:#x} has unexpected bytes {current:02X?}");
        return;
    }
    match unsafe { memory::patch(site, &SKIP_RLINE_BYTES) } {
        Ok(()) => log!("session: host task finishes without the rline join ({site:#x})"),
        Err(e) => log!("session: rline join patch failed: {e}"),
    }
}

// snJoinSessionTask (vtable 0xFE3278, update 0x662BF0) has the same Rockstar step: after the host
// accepted us (state 3 done), state 4 calls the rline join (0x6E3840), which fails without
// Rockstar's services and sends the task to "leave" and failure. Replace state 4 with "finished,
// succeeded" (push 0; push 1; jmp to the task's finish call at 0x662ED5).
const SKIP_JOIN_RLINE_PATTERN: &str = "8B 4E 60 57 6A 01 8D 86 58 01 00 00 50 8D 86 E0 00 00 00 50 83 C1 48 E8";
/// From the state 4 code to the `mov eax, [esi]; mov ecx, esi; call [eax+0x1C]` finish call.
const SKIP_JOIN_RLINE_FINISH: usize = 0x60;

pub fn skip_rline_join_when_joining() {
    let Some(site) = memory::find(SKIP_JOIN_RLINE_PATTERN) else {
        log!("session: join task rline step not found, skipped");
        return;
    };
    let finish = site + SKIP_JOIN_RLINE_FINISH;
    let expected = [0x8B, 0x06, 0x8B, 0xCE, 0xFF, 0x50, 0x1C];
    if unsafe { std::slice::from_raw_parts(finish as *const u8, expected.len()) } != expected {
        log!("session: join task finish call not where expected, skipped");
        return;
    }
    let rel = (finish as isize - (site as isize + 6)) as i8;
    let bytes = [0x6A, 0x00, 0x6A, 0x01, 0xEB, rel as u8];
    match unsafe { memory::patch(site, &bytes) } {
        Ok(()) => log!("session: join task finishes without the rline join ({site:#x})"),
        Err(e) => log!("session: join task rline patch failed: {e}"),
    }
}

pub fn trace_host_task() {
    unsafe {
        let vtable = memory::from_ghidra(HOST_TASK_VTABLE) as *mut usize;
        for (slot, hook, original) in [
            (SLOT_UPDATE, host_task_update as usize, &ORIGINAL_UPDATE),
            (SLOT_FINISH, host_task_finish as usize, &ORIGINAL_FINISH),
        ] {
            let entry = vtable.add(slot);
            original.store(*entry, Ordering::SeqCst);
            match memory::patch(entry as usize, &hook.to_le_bytes()) {
                Ok(()) => {}
                Err(e) => log!("session: host task trace slot {slot} failed: {e}"),
            }
        }
    }
    log!("session: tracing snHostSessionTask");
}

/// snJoinSessionTask: same slots; step at +0x90, status of the current step at +0x78C, and +0x364
/// set to 1 when connecting to the host failed.
const JOIN_TASK_VTABLE: usize = 0x00FE_3278;
const JOIN_TASK_STATUS: usize = 0x78C;
const JOIN_TASK_CONNECT_FAILED: usize = 0x364;

static ORIGINAL_JOIN_UPDATE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static ORIGINAL_JOIN_FINISH: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub fn trace_join_task() {
    unsafe {
        let vtable = memory::from_ghidra(JOIN_TASK_VTABLE) as *mut usize;
        for (slot, hook, original) in [
            (SLOT_UPDATE, join_task_update as usize, &ORIGINAL_JOIN_UPDATE),
            (SLOT_FINISH, join_task_finish as usize, &ORIGINAL_JOIN_FINISH),
        ] {
            let entry = vtable.add(slot);
            original.store(*entry, Ordering::SeqCst);
            if let Err(e) = memory::patch(entry as usize, &hook.to_le_bytes()) {
                log!("session: join task trace slot {slot} failed: {e}");
            }
        }
    }
    log!("session: tracing snJoinSessionTask");
}

extern "thiscall" fn join_task_update(task: *mut u8, elapsed: i32) {
    unsafe {
        let state = |task| (task_field(task, TASK_STEP), task_field(task, JOIN_TASK_STATUS), task_field(task, JOIN_TASK_CONNECT_FAILED));
        let before = state(task);
        let original: extern "thiscall" fn(*mut u8, i32) = std::mem::transmute(ORIGINAL_JOIN_UPDATE.load(Ordering::SeqCst));
        original(task, elapsed);
        let after = state(task);
        if before != after {
            log!("session: join task step {} (status {}, connect failed {}) -> step {} (status {}, connect failed {})",
                before.0, before.1, before.2, after.0, after.1, after.2);
        }
    }
}

extern "thiscall" fn join_task_finish(task: *mut u8, success: i32, code: i32) {
    unsafe {
        log!(
            "session: join task finished, success {success}, code {code}, at step {} (status {})",
            task_field(task, TASK_STEP),
            task_field(task, JOIN_TASK_STATUS)
        );
        let original: extern "thiscall" fn(*mut u8, i32, i32) = std::mem::transmute(ORIGINAL_JOIN_FINISH.load(Ordering::SeqCst));
        original(task, success, code);
    }
}

unsafe fn task_field(task: *mut u8, offset: usize) -> i32 {
    unsafe { std::ptr::read_unaligned(task.add(offset) as *const i32) }
}

extern "thiscall" fn host_task_update(task: *mut u8, elapsed: i32) {
    unsafe {
        let before = (task_field(task, TASK_STEP), task_field(task, TASK_STATUS));
        let original: extern "thiscall" fn(*mut u8, i32) = std::mem::transmute(ORIGINAL_UPDATE.load(Ordering::SeqCst));
        original(task, elapsed);
        let after = (task_field(task, TASK_STEP), task_field(task, TASK_STATUS));
        if before != after {
            log!("session: host task step {} (status {}) -> step {} (status {})", before.0, before.1, after.0, after.1);
        }
    }
}

extern "thiscall" fn host_task_finish(task: *mut u8, success: i32, code: i32) {
    unsafe {
        log!(
            "session: host task finished, success {success}, code {code}, at step {} (status {})",
            task_field(task, TASK_STEP),
            task_field(task, TASK_STATUS)
        );
        let original: extern "thiscall" fn(*mut u8, i32, i32) = std::mem::transmute(ORIGINAL_FINISH.load(Ordering::SeqCst));
        original(task, success, code);
    }
}

// --- Match config recorder ----------------------------------------------------------------------
// The host's game settings (mode, auto-aim, police, weapons, time, ...) end up in the match config
// at 0x0110E2B0 (30 dwords; the lobby script writes most of it through native 0x47B4B0, which
// copies +0x2C..+0x74). Field meanings aren't mapped yet, so log every change: pick the settings
// once in the lobby UI and the log shows which field holds what.

const MATCH_CONFIG: usize = 0x0110_E2B0;
const MATCH_CONFIG_DWORDS: usize = 30;

static LAST_CONFIG: std::sync::Mutex<Option<[i32; MATCH_CONFIG_DWORDS]>> = std::sync::Mutex::new(None);

pub fn watch_match_config() {
    let current: [i32; MATCH_CONFIG_DWORDS] =
        unsafe { std::ptr::read_unaligned(memory::from_ghidra(MATCH_CONFIG) as *const [i32; MATCH_CONFIG_DWORDS]) };
    let mut last = LAST_CONFIG.lock().unwrap();
    match *last {
        None => log!("config: initial {current:?}"),
        Some(previous) => {
            for (i, (old, new)) in previous.iter().zip(&current).enumerate() {
                if old != new {
                    log!("config: +{:#04x} {old} -> {new}", i * 4);
                }
            }
        }
    }
    *last = Some(current);
}
