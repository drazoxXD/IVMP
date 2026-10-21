//! Host system, the LibertyM way (see docs/libertym-notes.md): no `network_startup` lobby; we
//! drive hosting and joining ourselves with the game's network natives, every frame on the game
//! thread.
//!
//! Step 1 (this file): hosting a Free Mode session and letting players into the game.
//! Joining (finding the host the server picked) comes next.

use std::sync::Mutex;

use crate::{log, natives, net};

// Natives (hashes as in GTA IV; handlers found through the registration table).
const NETWORK_HOST_GAME: u32 = 0x5BEA05E2;
const NETWORK_IS_OPERATION_PENDING: u32 = 0x391E4575;
const NETWORK_HOST_SUCCEEDED: u32 = 0x1CA77E94;
const NETWORK_FIND_GAME: u32 = 0x5D4D0C86;
const NETWORK_IS_FIND_PENDING: u32 = 0x23D60810;
const NETWORK_FOUND_GAME_COUNT: u32 = 0x10DF4CED;
const NETWORK_JOIN_GAME: u32 = 0x60806A0C;
const NETWORK_IS_JOIN_PENDING: u32 = 0x76C53927;
const NETWORK_JOIN_SUCCEEDED: u32 = 0x59F24327;
const LOCAL_PLAYER_READY_TO_START: u32 = 0x5C03585C;
const LAUNCH_LOCAL_PLAYER_IN_NETWORK_GAME: u32 = 0x70FE415C;
const PLAYER_WANTS_TO_JOIN: u32 = 0x7D99343C;
const TELL_NET_PLAYER_TO_START_PLAYING: u32 = 0x465D424D;
/// NETWORK_START_SESSION (handler 0xBD8E20 -> 0x47BBD0): the host marks the session started
/// (0x6203C0, session +0xC38); everyone's network state then goes lobby (8) -> in game (10).
/// Normally the game mode script calls it; we run no scripts.
const NETWORK_START_SESSION: u32 = 0x5880_2CE5;
/// Network state machine value (0x17F5EA4) for "in the lobby".
const NETWORK_STATE_LOBBY: u32 = 8;
const NETWORK_STATE_IN_GAME: u32 = 10;

/// Free Mode, any ranked, any episode.
const FIND_ARGS: [u32; 4] = [16, 0, 0, 0];
/// Frames between find attempts while the host isn't visible yet.
const FIND_RETRY_FRAMES: u32 = 180;
/// Free Mode, not ranked, 32 slots, public, episode 0 (IV), no teams.
const HOST_ARGS: [u32; 6] = [16, 0, 32, 0, 0, 0];
const MAX_ATTEMPTS: u32 = 3;
const MAX_PLAYERS: u32 = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    Loaded,
    StartFinding,
    Finding,
    StartJoining,
    Joining,
    JoiningNetGame,
    Joined,
    FindRetryWait,
    StartHosting,
    Hosting,
    HostingNetGame,
    Hosted,
    /// `gamemode = -1`: the game's own lobby (network_startup) hosts, finds and starts games.
    Lobby,
    Fatal,
    GaveUp,
}

struct HostSystem {
    state: State,
    attempts: u32,
    wait_frames: u32,
    /// Players already logged as told to start playing (bit per player).
    told: u32,
    /// Frames spent hosted / joined (for periodic diagnostics).
    frames: u32,
    /// NETWORK_START_SESSION already called.
    started: bool,
    /// Network state when the server's object ids were last put in the engine's ring (0 = not
    /// yet).
    ids_given: u32,
}

static SYSTEM: Mutex<HostSystem> = Mutex::new(HostSystem { state: State::Idle, attempts: 0, wait_frames: 0, told: 0, frames: 0, started: false, ids_given: 0 });

impl HostSystem {
    /// Network object ids come from the server, host and joiner alike: the host's grant never
    /// reaches joiners, and the host's own ids would collide with theirs. Taken straight away
    /// (waiting let the game spawn people and cars it couldn't network), and again whenever the
    /// network state moved on and that emptied them: the joiner's session start does, and in a
    /// lobby game the joiner stays in the lobby (state 8), where an empty ring left its player
    /// ped queued for networking for good, so no lobby listed it.
    /// Refilling a ring that still has ids would hand out ids already in use.
    fn give_object_ids(&mut self, network_state: u32) {
        if self.ids_given != 0 && network_state > self.ids_given && object_id_count() != 0 {
            self.ids_given = network_state;
        }
        if self.ids_given == 0 || network_state > self.ids_given {
            if let Some((first, count)) = net::object_ids() {
                use_object_ids(first, count);
                self.ids_given = network_state.max(1);
            } else if self.frames % 300 == 1 {
                log!("host: no object ids from the server yet");
            }
        }
    }

    fn set(&mut self, state: State) {
        if self.state != state {
            log!("host: {:?} -> {:?}", self.state, state);
            self.state = state;
        }
    }

    fn process(&mut self) {
        match self.state {
            State::Idle | State::GaveUp => {}
            // `gamemode = -1`: the first player (the server's host) gets the game's lobby, which
            // hosts and picks the game; everyone else joins that player's session as in peer to
            // peer (no lobby script there, see boot.rs use_game_lobby).
            State::Loaded if net::lobby_mode() => match (net::our_id(), net::host_id()) {
                (Some(us), Some(host)) if us != host => {
                    log!("host: gamemode -1, joining the lobby of {}", NetProtocol::virtual_ip(host));
                    self.set(State::StartFinding);
                }
                (Some(_), Some(_)) => {
                    log!("host: gamemode -1, we host: the game's lobby picks the game");
                    self.set(State::Lobby);
                }
                _ => {}
            },
            // The IVMP server decides who hosts; everyone else joins that host.
            State::Loaded => match (net::our_id(), net::host_id()) {
                (Some(us), Some(host)) if us != host => {
                    log!("host: joining the server's host {}", NetProtocol::virtual_ip(host));
                    self.set(State::StartFinding);
                }
                (Some(_), Some(_)) => self.set(State::StartHosting),
                // Not connected to the IVMP server yet.
                _ => {}
            },
            State::StartFinding => {
                let (mode, _) = net::game_settings();
                natives::invoke(NETWORK_FIND_GAME, &[mode as u32, FIND_ARGS[1], FIND_ARGS[2], FIND_ARGS[3]]);
                self.set(State::Finding);
            }
            State::Finding => {
                if !natives::invoke_bool(NETWORK_IS_FIND_PENDING, &[]) {
                    let found = natives::invoke(NETWORK_FOUND_GAME_COUNT, &[]).unwrap_or(0);
                    log!("host: find finished, {found} game(s)");
                    if found == 0 {
                        self.wait_frames = FIND_RETRY_FRAMES;
                        self.set(State::FindRetryWait);
                    } else {
                        self.set(State::StartJoining);
                    }
                }
            }
            State::FindRetryWait => {
                // Don't fall back to hosting: that would split the players into two sessions.
                self.wait_frames = self.wait_frames.saturating_sub(1);
                if self.wait_frames == 0 {
                    self.set(State::StartFinding);
                }
            }
            State::StartJoining => {
                ensure_gamer_id();
                log_join_state("before join");
                natives::invoke(NETWORK_JOIN_GAME, &[0]);
                self.set(State::Joining);
            }
            State::Joining => {
                if !natives::invoke_bool(NETWORK_IS_JOIN_PENDING, &[]) {
                    self.set(State::JoiningNetGame);
                }
            }
            State::JoiningNetGame => {
                if natives::invoke_bool(NETWORK_JOIN_SUCCEEDED, &[]) {
                    self.set(State::Joined);
                } else {
                    log!("host: join failed, finding again");
                    log_join_state("after failed join");
                    self.wait_frames = FIND_RETRY_FRAMES;
                    self.set(State::FindRetryWait);
                }
            }
            // The host call's return value isn't reliable here (the impl at 0xBDAF00 returns
            // whatever is in AL), and hosting twice deadlocks (0x47CDF0), so don't retry on it:
            // judge by the pending / succeeded natives instead.
            State::StartHosting => match natives::invoke(NETWORK_HOST_GAME, &{
                let (mode, slots) = net::game_settings();
                let mut args = HOST_ARGS;
                args[0] = mode as u32;
                args[2] = slots;
                args
            }) {
                Some(ret) => {
                    log!("host: NETWORK_HOST_GAME returned {:#x}", ret & 0xFF);
                    self.set(State::Hosting);
                }
                None => self.set(State::GaveUp),
            },
            State::Hosting => {
                if !natives::invoke_bool(NETWORK_IS_OPERATION_PENDING, &[]) {
                    self.set(State::HostingNetGame);
                }
            }
            State::HostingNetGame => {
                if natives::invoke_bool(NETWORK_HOST_SUCCEEDED, &[]) {
                    self.set(State::Hosted);
                    log_join_state("hosted");
                } else {
                    self.set(State::Fatal);
                }
            }
            // A lobby game (`gamemode = -1`): in the host's session, start our own lobby
            // script (network_startup) so we show up in the host's lobby; it and the host's
            // lobby start the game and launch us. We keep the server's object ids in the engine.
            State::Joined if net::lobby_mode() => {
                let network_state = unsafe { *(crate::memory::from_ghidra(0x017F_5EA4) as *const u32) };
                if !self.started {
                    self.started = true;
                    log!("host: in the host's session (network state {network_state}), starting the lobby script");
                    crate::lobby::start_network_startup();
                }
                self.frames += 1;
                if self.frames % 600 == 1 {
                    log!("host: in the host's lobby, network state {network_state}");
                }
                if self.frames % 300 == 1 {
                    log_players();
                }
                self.give_object_ids(network_state);
                if network_state == NETWORK_STATE_IN_GAME {
                    crate::spawner::tick();
                }
            }
            State::Hosted | State::Joined => {
                let network_state = unsafe { *(crate::memory::from_ghidra(0x017F_5EA4) as *const u32) };
                if !self.started && network_state == NETWORK_STATE_LOBBY {
                    self.started = true;
                    let ok = natives::invoke_bool(NETWORK_START_SESSION, &[]);
                    log!("host: started the session (NETWORK_START_SESSION -> {ok})");
                }
                self.frames += 1;
                if self.frames % 300 == 1 {
                    log_players();
                }
                self.give_object_ids(network_state);
                if network_state == NETWORK_STATE_IN_GAME {
                    crate::spawner::tick();
                }
                if natives::invoke_bool(LOCAL_PLAYER_READY_TO_START, &[]) {
                    natives::invoke(LAUNCH_LOCAL_PLAYER_IN_NETWORK_GAME, &[]);
                    log!("host: launched local player into the network game");
                }
                for player in 0..MAX_PLAYERS {
                    if natives::invoke_bool(PLAYER_WANTS_TO_JOIN, &[player]) {
                        natives::invoke(TELL_NET_PLAYER_TO_START_PLAYING, &[player, 0]);
                        // Logged once per player: it repeats every frame until the player starts.
                        if self.told & (1 << player) == 0 {
                            self.told |= 1 << player;
                            log!("host: player {player} told to start playing");
                        }
                    }
                }
            }
            // The lobby's scripts host or join, start the game and launch the players; we only
            // keep the server's object ids in the engine (only once in a session: before that
            // there is nothing to network) and run the spawner.
            State::Lobby => {
                let network_state = unsafe { *(crate::memory::from_ghidra(0x017F_5EA4) as *const u32) };
                self.frames += 1;
                if self.frames % 600 == 1 {
                    log!("host: lobby, network state {network_state}");
                }
                if self.frames % 300 == 1 {
                    log_players();
                }
                if network_state >= NETWORK_STATE_LOBBY {
                    self.give_object_ids(network_state);
                }
                if network_state == NETWORK_STATE_IN_GAME {
                    crate::spawner::tick();
                }
            }
            State::Fatal => {
                if self.attempts < MAX_ATTEMPTS {
                    self.attempts += 1;
                    self.set(State::Loaded);
                } else {
                    log!("host: could not host a session after {MAX_ATTEMPTS} attempts");
                    self.set(State::GaveUp);
                }
            }
        }
    }
}

/// Diagnostics for joining: the state JOIN_GAME (0x47D470) checks before it does anything.
fn log_join_state(when: &str) {
    let read = |address: usize| unsafe { *(crate::memory::from_ghidra(address) as *const u32) };
    // Session manager 0x1982018: sessions at +8 and behind the pointers at +0x6888 / +0x688C,
    // each with its state at +0x50 (2..3 = active). JOIN_GAME needs one active, or the
    // manager not yet initialised (flag 2 of 0x198B568 clear).
    let manager = 0x0198_2018usize;
    let state_of = |session: usize| if session == 0 { u32::MAX } else { unsafe { *((session + 0x50) as *const u32) } };
    let base = crate::memory::from_ghidra(manager);
    let session_a = read(manager + 0x6888) as usize;
    let session_b = read(manager + 0x688C) as usize;
    let gamer_index = read(0x0103_6F14);
    let gamer = if gamer_index == u32::MAX { 0 } else { read(0x011A_8808 + 4 * gamer_index as usize) as usize };
    let (g38, g3c) = if gamer == 0 { (0, 0) } else { unsafe { (*((gamer + 0x38) as *const u32), *((gamer + 0x3C) as *const u32)) } };
    if gamer != 0 {
        let bytes = unsafe { std::slice::from_raw_parts(gamer as *const u8, 0x70) };
        let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
        // Skip the Social Club name (+0x5C) so it doesn't end up in logs.
        log!("host: gamer +0x00: {} / +0x38: {}", hex[..0x38].join(" "), hex[0x38..0x5C].join(" "));
    }
    log!(
        "host: join state ({when}): flags {:#x}, manager {:#x}, sessions +8 {} / a {} / b {}, gamer {gamer:#x} id {g38:#x}:{g3c:#x}",
        read(0x0198_B568),
        read(manager),
        state_of(base + 8),
        state_of(session_a),
        state_of(session_b),
    );
}

/// The second copy isn't signed in to Social Club (only the Rockstar launcher signs a game in),
/// so its local gamer has no id (+0x38, -1:-1) and JOIN_GAME refuses at once. The host's id
/// looks random, so give ours one derived from the client id.
fn ensure_gamer_id() {
    let Some(id) = net::our_id() else { return };
    unsafe {
        let gamer_index = *(crate::memory::from_ghidra(0x0103_6F14) as *const u32);
        if gamer_index == u32::MAX {
            return;
        }
        let gamer = *(crate::memory::from_ghidra(0x011A_8808 + 4 * gamer_index as usize) as *const usize);
        if gamer == 0 {
            return;
        }
        let ids = (gamer + 0x38) as *mut u32;
        if *ids & *ids.add(1) == u32::MAX {
            *ids = crate::peer::base_of(id);
            *ids.add(1) = id as u32;
            log!("host: gave the local gamer id {:#x}:{:#x} (not signed in)", *ids, *ids.add(1));
        }
        // The online identity the session join (0x61FD20) and "gamer online" (0x6BD810) check:
        // +0x40 handle (not -1), +0x48 online id (non-zero), +0x50 kind 3 (online).
        let online = (gamer + 0x40) as *mut u32;
        if *((gamer + 0x50) as *const u8) != 3 {
            *online = *ids;
            *online.add(1) = *ids.add(1);
            *((gamer + 0x48) as *mut u64) = id as u64;
            *((gamer + 0x50) as *mut u8) = 3;
            log!("host: marked the local gamer as online (online id {id})");
        }
        // +0x58 is the signed-in local user index (0 on a signed-in copy, -1 otherwise); the
        // session join only accepts user 0. +0x5C holds the Social Club name.
        let user = (gamer + 0x58) as *mut i32;
        if *user != 0 {
            *user = 0;
            let name = (gamer + 0x5C) as *mut u8;
            let nickname = crate::nickname().as_bytes();
            let len = nickname.len().min(15);
            std::ptr::copy_nonoverlapping(nickname.as_ptr(), name, len);
            *name.add(len) = 0;
            log!("host: set the local gamer as signed-in user 0");
        }
        // The gamer info starts with the gamer's peer address (rlGamerInfo); the join request
        // serialises it (0x6BDA50) and fails without keys and an account id.
        if std::ptr::read_unaligned((gamer + 0x30) as *const u64) == 0 {
            crate::peer::write_peer_address(gamer as *mut u8, id);
            log!("host: gave the local gamer our peer address");
        }
        // rline's own table of signed-in users (0x80 bytes each, same layout). The session join
        // (0x61FD20) copies user 0 from it and requires its online id to match the gamer's.
        let rline_user = crate::memory::from_ghidra(0x019F_3230);
        if *((rline_user + 0x50) as *const u8) != 3 {
            std::ptr::copy_nonoverlapping(gamer as *const u8, rline_user as *mut u8, 0x70);
            log!("host: signed user 0 in to rline with the local gamer");
        }
    }
}

/// Diagnostics for starting play after a join: the local player index, the network player
/// manager's view (0x18E51E8: +0x58 local physical index, +0xBBC remote players) and which player
/// infos (0x11A8808[32]) exist with their "told to start" (+0x4E8) and "in session" (+0x578)
/// fields.
fn log_players() {
    let read = |address: usize| unsafe { *(crate::memory::from_ghidra(address) as *const u32) };
    let manager = crate::memory::from_ghidra(0x018E_51E8);
    let local_index = read(0x0103_6F14) as i32;
    let manager_local = unsafe { *((manager + 0x58) as *const u8) } as i8;
    let state = read(0x017F_5EA4);
    let mut infos = Vec::new();
    for i in 0..32usize {
        let info = read(0x011A_8808 + 4 * i) as usize;
        let remote = unsafe { *((manager + 0xBBC + 4 * i) as *const usize) };
        if info == 0 && remote == 0 {
            continue;
        }
        let (told, active) = if info == 0 {
            (-1, -1)
        } else {
            unsafe { (*((info + 0x4E8) as *const i32), *((info + 0x578) as *const i32)) }
        };
        let remote_index = if remote == 0 { -1 } else { unsafe { *((remote + 0x18) as *const i8) as i32 } };
        // CPlayerInfo +0x598 = the player's ped; ped +0x6C = its network object (CNetObjPlayer),
        // created by the ped's vtable slot 0xBC (0xA2A620) when registered with the network.
        let ped = if info == 0 { 0 } else { unsafe { *((info + 0x598) as *const usize) } };
        let net_object = if ped == 0 { 0 } else { unsafe { *((ped + 0x6C) as *const usize) } };
        infos.push(format!(
            "[{i}: info told {told} active {active}, net player index {remote_index}, ped {ped:#x}, net object {net_object:#x}]"
        ));
    }
    // Network object ids this machine may hand out (count at 0x19392D0, ring at 0x19360C8),
    // and peds waiting to be registered (10 slots of 12 bytes at 0x18B6788, ped first).
    let free_ids = read(0x0193_92D0);
    let queued: Vec<String> = (0..10usize)
        .map(|slot| read(0x018B_6788 + slot * 12))
        .filter(|ped| *ped != 0)
        .map(|ped| format!("{ped:#x}"))
        .collect();
    log!(
        "host: players: local index {local_index}, manager local {manager_local}, network state {state}, free object ids {free_ids} {:?}, queued peds [{}], {}",
        first_object_ids(),
        queued.join(" "),
        infos.join(" ")
    );
}

/// Network object ids this machine may use: a ring of i16 (0x1900 entries at 0x19360C8) with
/// write position 0x19392C8, read position 0x19392CC and count 0x19392D0. The joining copy
/// should get a block from the host (message read by 0x4A5EB0) but receives none, so its
/// player never becomes a network object. Every client (host too) uses the block the server
/// gave it instead (`ivmp_sync::ids`), so no two engines create entities with the same id.
const OBJECT_ID_RING: usize = 0x0193_60C8;
const OBJECT_ID_WRITE: usize = 0x0193_92C8;
const OBJECT_ID_READ: usize = 0x0193_92CC;
const OBJECT_ID_COUNT: usize = 0x0193_92D0;
const OBJECT_ID_RING_LEN: u32 = 0x1900;

fn object_id_count() -> u32 {
    unsafe { *(crate::memory::from_ghidra(OBJECT_ID_COUNT) as *const u32) }
}

/// First few free object ids, for comparing host and joiner ranges in the log.
fn first_object_ids() -> Vec<i16> {
    unsafe {
        let ring = crate::memory::from_ghidra(OBJECT_ID_RING) as *const i16;
        let read = *(crate::memory::from_ghidra(0x0193_92CC) as *const u32);
        (0..object_id_count().min(4)).map(|i| *ring.add(((read + 1 + i) % OBJECT_ID_RING_LEN) as usize)).collect()
    }
}

/// Replaces the engine's free object ids with the server's block `first..first + count`.
fn use_object_ids(first: u16, count: u16) {
    unsafe {
        let ring = crate::memory::from_ghidra(OBJECT_ID_RING) as *mut i16;
        let write = crate::memory::from_ghidra(OBJECT_ID_WRITE) as *mut u32;
        let read = crate::memory::from_ghidra(OBJECT_ID_READ) as *const u32;
        let free = crate::memory::from_ghidra(OBJECT_ID_COUNT) as *mut u32;
        let dropped = *free;
        // Empty ring (write == read), then push the block.
        *write = *read;
        *free = 0;
        for object_id in first..first + count.min(OBJECT_ID_RING_LEN as u16) {
            *write = (*write + 1) % OBJECT_ID_RING_LEN;
            *ring.add(*write as usize) = object_id as i16;
            *free += 1;
        }
        log!("host: using the server's object ids {first}..{} (dropped {dropped} engine ids)", first + count);
    }
}

/// NETWORK_JOIN_GAME's game handler, while scripts' calls are overridden.
static ORIGINAL_JOIN_GAME: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// NETWORK_JOIN_GAME from a script. In a lobby game (`gamemode = -1`) a joiner is already in the
/// host's session (IVMP joined it) when it starts its lobby script; that script's network_main
/// then joins again, the join refuses (already in a session) and its failure path (0x47D9F1 ->
/// 0x47CDF0) deadlocked the game thread. Report success instead; the join pending / succeeded
/// natives then answer for IVMP's join, which succeeded.
extern "C" fn script_join_game(context: *mut u32) {
    let joined = net::lobby_mode() && SYSTEM.try_lock().is_ok_and(|s| s.state == State::Joined);
    if joined {
        unsafe {
            let ret = *context as *mut u32;
            if !ret.is_null() {
                *ret = 1;
            }
        }
        log!("host: a script joined again; we already are in the host's session");
        return;
    }
    let original: extern "C" fn(*mut u32) = unsafe { std::mem::transmute(ORIGINAL_JOIN_GAME.load(std::sync::atomic::Ordering::SeqCst)) };
    original(context);
}

static ORIGINAL_WANTS_TO_JOIN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static ORIGINAL_TELL_START: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// Last PLAYER_WANTS_TO_JOIN answer per player (bit set = true), to log changes only.
static WANTS_TO_JOIN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static WANTS_TO_JOIN_ASKED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Diagnostics for the lobby: the host's lobby script lets players in with
/// PLAYER_WANTS_TO_JOIN(player) and TELL_NET_PLAYER_TO_START_PLAYING(player, ...); log which
/// players it asks about and the answers (on change).
extern "C" fn script_wants_to_join(context: *mut u32) {
    use std::sync::atomic::Ordering;
    let original: extern "C" fn(*mut u32) = unsafe { std::mem::transmute(ORIGINAL_WANTS_TO_JOIN.load(Ordering::SeqCst)) };
    original(context);
    unsafe {
        let (ret, args) = (*context as *const u32, *context.add(2) as *const u32);
        if ret.is_null() || args.is_null() {
            return;
        }
        let (player, answer) = (*args, *ret & 0xFF != 0);
        if player >= 32 {
            return;
        }
        let bit = 1 << player;
        let first = WANTS_TO_JOIN_ASKED.fetch_or(bit, Ordering::Relaxed) & bit == 0;
        let before = WANTS_TO_JOIN.load(Ordering::Relaxed) & bit != 0;
        if first || before != answer {
            if answer {
                WANTS_TO_JOIN.fetch_or(bit, Ordering::Relaxed);
            } else {
                WANTS_TO_JOIN.fetch_and(!bit, Ordering::Relaxed);
            }
            log!("host: lobby script asked PLAYER_WANTS_TO_JOIN({player}) -> {answer}");
        }
    }
}

extern "C" fn script_tell_start(context: *mut u32) {
    use std::sync::atomic::Ordering;
    unsafe {
        let args = *context.add(2) as *const u32;
        if !args.is_null() {
            log!("host: lobby script TELL_NET_PLAYER_TO_START_PLAYING({}, {})", *args, *args.add(1));
        }
    }
    let original: extern "C" fn(*mut u32) = unsafe { std::mem::transmute(ORIGINAL_TELL_START.load(Ordering::SeqCst)) };
    original(context);
}

/// Overrides natives scripts must not run in a lobby game, and traces the lobby's player
/// natives. Call after the native table is built.
pub fn install_lobby_natives() {
    use std::sync::atomic::Ordering;
    use ivmp_natives::nativelog::set_override;
    if let Some(original) = set_override(NETWORK_JOIN_GAME, script_join_game as usize) {
        ORIGINAL_JOIN_GAME.store(original, Ordering::SeqCst);
    }
    if let Some(original) = set_override(PLAYER_WANTS_TO_JOIN, script_wants_to_join as usize) {
        ORIGINAL_WANTS_TO_JOIN.store(original, Ordering::SeqCst);
    }
    if let Some(original) = set_override(TELL_NET_PLAYER_TO_START_PLAYING, script_tell_start as usize) {
        ORIGINAL_TELL_START.store(original, Ordering::SeqCst);
    }
}

/// The network session has loaded: start hosting on the next frame.
pub fn on_network_loaded() {
    if let Some(id) = net::our_id() {
        crate::peer::set_online_address(id);
    }
    crate::relay::start();
    let mut system = SYSTEM.lock().unwrap();
    system.attempts = 0;
    system.set(State::Loaded);
}

/// Once per frame on the game thread.
pub fn tick() {
    crate::relay::pump();
    SYSTEM.lock().unwrap().process();
}
