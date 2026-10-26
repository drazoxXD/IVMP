//! Peer identity without Rockstar services, ported from LibertyM (docs/libertym-notes.md). Every
//! LibertyM pattern used here matches GTAIV.exe 1.2.0.59 exactly once.
//!
//! * Our peer address (local / relay / public) is our virtual IP from the IVMP server, so the
//!   other engines send to an address our tunnel (net.rs) routes.
//! * Peer keys and session IDs are deterministic, derived from the client id.
//! * Parsing a found session (`ReadSession`) yields the host the server picked.

use std::net::Ipv4Addr;

use crate::{log, memory, net};

/// `bool __cdecl GetLocalPeerAddress(netPeerAddress* out)`.
const GET_LOCAL_PEER_ADDRESS: &str = "55 8B EC 83 E4 F8 83 EC 18 33 C0";
/// Call inside it (+0xA0) to the key generator `void __fastcall(u64* out, u32 seed)`.
const KEY_GENERATOR_CALL: usize = 0xA0;
/// `push imm32` of the online address block (two ip/port pairs).
const ONLINE_ADDRESS: (&str, usize) = ("50 53 8D 44 24 24 50 68 ? ? ? ? 8D", 8);
/// Calls that create a peer / session id (joinee, migrate, host): `bool __fastcall(char* id)`.
const UUID_CALLS: [(&str, usize); 3] = [
    ("84 DB 74 17 8D 4C 24 1C E8", 8),
    ("8D 4C 24 08 66 0F 13 44 24 10 E8", 10),
    ("10 E8 ? ? ? ? 84 C0 74 5E 8B", 1),
];
/// `bool __stdcall ReadSession(void* tree, rlSessionInfo* out)` starts 0x48 before this.
const READ_SESSION: (&str, isize) = ("74 36 83 7E 24 00 74 30", -0x48);

/// NETWORK_FIND_GAME (0x47ACA0) calls the rline matchmaking search (0x6DA2E0) here. That asks
/// Rockstar's servers, so it fails at once and "find" ends with 0 games. We answer it instead.
const MATCHMAKING_CALL: (&str, usize) = ("38 15 ? ? ? ? 0F 95 C2 42 E8", 10);
/// Status of the matchmaking operation (0 none, 1 pending, 2 failed); "find pending" reads it.
const MATCHMAKING_STATUS: usize = 0x018E_5094;
const MATCHMAKING_STATUS_DETAIL: usize = 0x018E_5098;
/// Found games are 0x670 bytes each; join (0x47A870) uses the session at +0x40 of entry N.
const FOUND_GAME_SIZE: usize = 0x670;
const FOUND_GAME_SESSION: usize = 0x40;

/// Globals GetLocalPeerAddress reads (same addresses as LibertyM's 1.2.0.43 build):
/// "key time initialised" flag, key time, and the two 64-bit peer keys.
const KEY_TIME_SET: usize = 0x018B_82CC;
const KEY_TIME: usize = 0x018B_82D0;
const PEER_KEYS: usize = 0x01BB_3970;

pub const GAME_PORT: u16 = 6672;

static ORIGINAL_GET_LOCAL_PEER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// rage::netPeerAddress (0x38 bytes).
#[repr(C)]
struct NetPeerAddress {
    key1: u64,
    key2: u64,
    key_time: u32,
    relay_ip: u32,
    relay_port: u16,
    _pad0: u16,
    public_ip: u32,
    public_port: u16,
    _pad1: u16,
    local_ip: u32,
    local_port: u16,
    _pad2: u16,
    unk: u32,
    account_id: u64,
}

/// rage::rlSessionInfo: two session tokens, then the host's peer address.
#[repr(C)]
struct RlSessionInfo {
    token1: u64,
    token2: u64,
    peer: NetPeerAddress,
}

/// Per-client key seed, derived from the client id so every client can compute every other
/// client's keys without extra messages.
pub fn base_of(id: u8) -> u32 {
    0x1F00_0000 | ((id as u32) << 8) | 0x5E
}

fn fill_peer(address: &mut NetPeerAddress, id: u8) {
    let ip = u32::from(NetProtocol::virtual_ip(id));
    let base = base_of(id) as u64;
    address.key1 = base;
    address.key2 = base;
    address.key_time = base_of(id) ^ 0xABCD;
    address.relay_ip = ip;
    address.relay_port = GAME_PORT;
    address.public_ip = ip;
    address.public_port = GAME_PORT;
    address.local_ip = ip;
    address.local_port = GAME_PORT;
    address.account_id = id as u64;
}

extern "C" fn get_local_peer_address(out: *mut NetPeerAddress) -> bool {
    let original: extern "C" fn(*mut NetPeerAddress) -> bool =
        unsafe { std::mem::transmute(ORIGINAL_GET_LOCAL_PEER.load(std::sync::atomic::Ordering::SeqCst)) };
    let result = original(out);
    let Some(id) = net::our_id() else { return result };
    unsafe {
        let base = base_of(id);
        *(memory::from_ghidra(KEY_TIME_SET) as *mut u8) = 1;
        *(memory::from_ghidra(KEY_TIME) as *mut u32) = base ^ 0xABCD;
        *(memory::from_ghidra(PEER_KEYS) as *mut u64) = base as u64;
        *((memory::from_ghidra(PEER_KEYS) + 8) as *mut u64) = base as u64;
        if !out.is_null() {
            fill_peer(&mut *out, id);
        }
    }
    true
}

/// rage key generator replacement: deterministic keys from the seed.
extern "fastcall" fn generate_keys(out: *mut u64, seed: u32) {
    unsafe {
        *out = (seed ^ 0xABCD) as u64;
        *out.add(1) = (seed ^ 0xABCD) as u64;
    }
}

extern "fastcall" fn forced_uuid(id: *mut u8) -> bool {
    unsafe { std::ptr::write_unaligned(id as *mut u64, 2) };
    true
}

/// Session parsing replacement: the session to join is always the server's host.
extern "stdcall" fn read_session(_tree: *mut u8, out: *mut RlSessionInfo) -> bool {
    let (Some(us), Some(host)) = (net::our_id(), net::host_id()) else { return false };
    if host == us || out.is_null() {
        return false;
    }
    fill_session(out, host);
    log!("peer: session to join is the host {}", NetProtocol::virtual_ip(host));
    true
}

/// Writes the peer address of client `id` (0x38 bytes, as GetLocalPeerAddress returns it) at
/// `out`. Used for the local gamer info on a copy that isn't signed in.
///
/// # Safety
/// `out` must point to 0x38 writable bytes.
pub unsafe fn write_peer_address(out: *mut u8, id: u8) {
    unsafe {
        std::ptr::write_bytes(out, 0, size_of::<NetPeerAddress>());
        fill_peer(&mut *(out as *mut NetPeerAddress), id);
    }
}

/// Session info (tokens and peer address) of the host the IVMP server picked.
fn fill_session(out: *mut RlSessionInfo, host: u8) {
    unsafe {
        std::ptr::write_bytes(out as *mut u8, 0, size_of::<RlSessionInfo>());
        (*out).token1 = 2;
        (*out).token2 = 2;
        fill_peer(&mut (*out).peer, host);
    }
}

/// Our answer to the matchmaking search. Arguments after the return address are the caller's
/// stack arguments: two unknown dwords, the query, the results array, its capacity, an unknown
/// dword, the found count, an unknown dword.
extern "C" fn matchmaking_find(
    _ecx: u32,
    _edx: u32,
    _return: u32,
    _a: u32,
    _b: u32,
    _query: u32,
    results: *mut u8,
    capacity: u32,
    _c: u32,
    count: *mut u32,
) -> u8 {
    let status = memory::from_ghidra(MATCHMAKING_STATUS) as *mut u32;
    let host = match (net::our_id(), net::host_id()) {
        (Some(us), Some(host)) if us != host => Some(host),
        _ => None,
    };
    unsafe {
        *(memory::from_ghidra(MATCHMAKING_STATUS_DETAIL) as *mut u32) = 0;
        let (Some(host), false, false) = (host, results.is_null(), count.is_null() || capacity == 0) else {
            *status = 2;
            log!("peer: find game: no host to offer");
            return 0;
        };
        std::ptr::write_bytes(results, 0, FOUND_GAME_SIZE);
        fill_session(results.add(FOUND_GAME_SESSION) as *mut RlSessionInfo, host);
        *count = 1;
        // Done, not pending: "find pending" goes false and the count reads 1.
        *status = 0;
    }
    log!("peer: find game answered with the host {}", NetProtocol::virtual_ip(host.unwrap_or(0)));
    1
}

/// The search is called fastcall (ECX, EDX) with caller-cleaned stack arguments.
#[unsafe(naked)]
extern "C" fn matchmaking_thunk() {
    core::arch::naked_asm!(
        "push edx",
        "push ecx",
        "call {find}",
        "add esp, 8",
        "ret",
        find = sym matchmaking_find,
    );
}

/// Writes our virtual address into the game's online address block (two ip/port pairs).
pub fn set_online_address(id: u8) {
    let Some(site) = memory::find(ONLINE_ADDRESS.0) else { return };
    unsafe {
        let block = std::ptr::read_unaligned((site + ONLINE_ADDRESS.1) as *const u32) as usize;
        let ip = u32::from(NetProtocol::virtual_ip(id));
        for pair in 0..2 {
            *((block + pair * 8) as *mut u32) = ip;
            *((block + pair * 8 + 4) as *mut u16) = GAME_PORT;
        }
        log!("peer: online address set to {}:{GAME_PORT}", Ipv4Addr::from(ip));
    }
}

fn jump(from: usize, to: usize) -> windows::core::Result<()> {
    let rel = (to as isize - (from as isize + 5)) as i32;
    let mut bytes = [0xE9u8, 0, 0, 0, 0];
    bytes[1..].copy_from_slice(&rel.to_le_bytes());
    unsafe { memory::patch(from, &bytes) }
}

pub fn install() {
    // GetLocalPeerAddress: wrap every call to it.
    match memory::find(GET_LOCAL_PEER_ADDRESS) {
        Some(function) => {
            ORIGINAL_GET_LOCAL_PEER.store(function, std::sync::atomic::Ordering::SeqCst);
            let calls = memory::find_calls_to(function);
            for call in &calls {
                let _ = unsafe { memory::redirect_call(*call, get_local_peer_address as usize) };
            }
            log!("peer: local peer address hook at {} call sites", calls.len());

            // The key generator it calls, replaced everywhere (as LibertyM does).
            let call = function + KEY_GENERATOR_CALL;
            let rel = unsafe { std::ptr::read_unaligned((call + 1) as *const i32) };
            let generator = (call as isize + 5 + rel as isize) as usize;
            match jump(generator, generate_keys as usize) {
                Ok(()) => log!("peer: key generator replaced at {generator:#x}"),
                Err(e) => log!("peer: key generator patch failed: {e}"),
            }
        }
        None => log!("peer: local peer address function not found"),
    }

    for (pattern, offset) in UUID_CALLS {
        match memory::find(pattern).map(|a| a + offset) {
            Some(call) => {
                let _ = unsafe { memory::redirect_call(call, forced_uuid as usize) };
                log!("peer: forced id at {call:#x}");
            }
            None => log!("peer: id call {pattern:?} not found"),
        }
    }

    match memory::find(MATCHMAKING_CALL.0).map(|a| a + MATCHMAKING_CALL.1) {
        Some(call) => match unsafe { memory::redirect_call(call, matchmaking_thunk as usize) } {
            Ok(_) => log!("peer: find game answered by us (call at {call:#x})"),
            Err(e) => log!("peer: find game hook failed: {e}"),
        },
        None => log!("peer: find game call not found"),
    }

    match memory::find(READ_SESSION.0).map(|a| (a as isize + READ_SESSION.1) as usize) {
        Some(function) => match jump(function, read_session as usize) {
            Ok(()) => log!("peer: session reader replaced at {function:#x}"),
            Err(e) => log!("peer: session reader patch failed: {e}"),
        },
        None => log!("peer: session reader not found"),
    }
}
