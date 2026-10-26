//! The engine's game socket and relay, driven the LibertyM way (docs/libertym-notes.md).
//!
//! In our boot the game socket is never opened (no `bind` on 6672 ever happens), so a join
//! request has nothing to go out on. Like LibertyM we bind it ourselves, mark the relay as up,
//! and every frame read what arrived (our tunnel, net.rs) and hand it to the connection manager
//! as relay packets, followed by a "receive thread ticked" event.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use crate::{log, memory};

/// `mov ...` holding the game's rage::netSocket (imm32 at +10).
const GAME_SOCKET: (&str, usize) = ("8D 44 24 0C 50 56 FF 75 0C", 10);
/// `void __fastcall bind(netSocket*, u16 port)` starts 0x29 before this.
const BIND: (&str, isize) = ("F6 86 A0 00 00 00 20 75 3A", -0x29);
/// `cmp byte ptr [relayActive], 0` (address at +2).
const RELAY_ACTIVE: (&str, usize) = ("80 3D ? ? ? ? 00 74 32 6A 00 68", 2);
/// Connection manager event handlers (thiscall manager, event).
const ON_RELAY_RECEIVED: &str = "8B 44 24 04 83 78 20 02";
const ON_RECEIVE_TICKED: (&str, isize) = ("83 78 20 04 8B F1", -5);
/// `int __thiscall receive(netSocket*, netSocketAddress* from, void* buf, int len, int* err)`.
const RECEIVE: &str = "83 EC 1C 53 55 8B E9 56 83 7D 04";
/// `push connectionManager` (imm32 at +5).
const CONNECTION_MANAGER: (&str, usize) = ("8B 01 6A 07 68", 5);

const GAME_PORT: u16 = 6672;
const EVENT_PACKET_RECEIVED: u32 = 2;
const EVENT_RECEIVE_TICKED: u32 = 4;
const LOG_PACKETS: u32 = 20;

/// rage relay / socket event as the connection manager reads it.
#[repr(C)]
struct Event {
    vtable: usize,
    _pad: [u8; 28],
    kind: u32,
    /// Relay address (0xFFFFFFFF = none) then the direct netSocketAddress.
    address: [u8; 16],
    data: *const u8,
    len: usize,
}

struct Functions {
    socket: usize,
    manager: usize,
    receive: usize,
    on_relay_received: usize,
    on_receive_ticked: usize,
}

static STARTED: AtomicBool = AtomicBool::new(false);
static SOCKET: AtomicUsize = AtomicUsize::new(0);
static MANAGER: AtomicUsize = AtomicUsize::new(0);
static RECEIVE_FN: AtomicUsize = AtomicUsize::new(0);
static ON_RELAY_FN: AtomicUsize = AtomicUsize::new(0);
static ON_TICKED_FN: AtomicUsize = AtomicUsize::new(0);
static FED: AtomicU32 = AtomicU32::new(0);
static FRAMES: AtomicU32 = AtomicU32::new(0);
/// Frames between traffic summaries in the log.
const SUMMARY_FRAMES: u32 = 600;

fn imm32(pattern: &str, offset: usize) -> Option<usize> {
    memory::find(pattern).map(|a| unsafe { std::ptr::read_unaligned((a + offset) as *const u32) } as usize)
}

fn at(pattern: &str, offset: isize) -> Option<usize> {
    memory::find(pattern).map(|a| (a as isize + offset) as usize)
}

fn locate() -> Option<(Functions, usize, usize)> {
    Some((
        Functions {
            socket: imm32(GAME_SOCKET.0, GAME_SOCKET.1)?,
            manager: imm32(CONNECTION_MANAGER.0, CONNECTION_MANAGER.1)?,
            receive: at(RECEIVE, 0)?,
            on_relay_received: at(ON_RELAY_RECEIVED, 0)?,
            on_receive_ticked: at(ON_RECEIVE_TICKED.0, ON_RECEIVE_TICKED.1)?,
        },
        at(BIND.0, BIND.1)?,
        imm32(RELAY_ACTIVE.0, RELAY_ACTIVE.1)?,
    ))
}

static BIND_FN: AtomicUsize = AtomicUsize::new(0);
static RELAY_ACTIVE_FLAG: AtomicUsize = AtomicUsize::new(0);

/// Binds the game socket and marks the relay active, on the game thread whenever a network
/// session has loaded. Again after a reload (a lobby game type change loads the network game
/// again): the joiner's join to the host's new session got no answer, the host still read its
/// packets but sent nothing back, so the flag or the binding may not survive the shutdown.
/// Bind skips a socket that's already bound (flag 0x20 at +0xA0).
pub fn start() {
    if !STARTED.swap(true, Ordering::SeqCst) {
        let Some((functions, bind, relay_active)) = locate() else {
            log!("relay: game socket functions not found");
            STARTED.store(false, Ordering::SeqCst);
            return;
        };
        SOCKET.store(functions.socket, Ordering::SeqCst);
        MANAGER.store(functions.manager, Ordering::SeqCst);
        RECEIVE_FN.store(functions.receive, Ordering::SeqCst);
        ON_RELAY_FN.store(functions.on_relay_received, Ordering::SeqCst);
        ON_TICKED_FN.store(functions.on_receive_ticked, Ordering::SeqCst);
        BIND_FN.store(bind, Ordering::SeqCst);
        RELAY_ACTIVE_FLAG.store(relay_active, Ordering::SeqCst);
    } else {
        let socket = SOCKET.load(Ordering::SeqCst);
        let (flag, socket_flags) = unsafe {
            (*(RELAY_ACTIVE_FLAG.load(Ordering::SeqCst) as *const u8), *((socket + 0xA0) as *const u32))
        };
        log!("relay: session loaded again; relay active was {flag}, socket flags {socket_flags:#x}");
    }
    let socket = SOCKET.load(Ordering::SeqCst);
    unsafe {
        let bind: extern "fastcall" fn(usize, u16) = std::mem::transmute(BIND_FN.load(Ordering::SeqCst));
        bind(socket, GAME_PORT);
        *(RELAY_ACTIVE_FLAG.load(Ordering::SeqCst) as *mut u8) = 1;
    }
    log!("relay: game socket {socket:#x} bound, relay marked active");
}

/// Every frame: hand what arrived to the connection manager.
pub fn pump() {
    let socket = SOCKET.load(Ordering::Relaxed);
    if socket == 0 {
        return;
    }
    let manager = MANAGER.load(Ordering::Relaxed);
    if FRAMES.fetch_add(1, Ordering::Relaxed) % SUMMARY_FRAMES == 0 {
        let (sent, received) = crate::net::packet_counts();
        log!(
            "relay: {sent} packets sent, {received} received, {} fed so far (thread {})",
            FED.load(Ordering::Relaxed),
            unsafe { windows::Win32::System::Threading::GetCurrentThreadId() }
        );
    }
    unsafe {
        let receive: extern "thiscall" fn(usize, *mut u8, *mut u8, i32, *mut i32) -> i32 =
            std::mem::transmute(RECEIVE_FN.load(Ordering::Relaxed));
        let on_relay: extern "thiscall" fn(usize, *mut Event) = std::mem::transmute(ON_RELAY_FN.load(Ordering::Relaxed));
        let on_ticked: extern "thiscall" fn(usize, *mut Event) = std::mem::transmute(ON_TICKED_FN.load(Ordering::Relaxed));

        let mut from = [0u8; 8];
        let mut buf = [0u8; 1400];
        loop {
            let len = receive(socket, from.as_mut_ptr(), buf.as_mut_ptr(), buf.len() as i32, std::ptr::null_mut());
            if len <= 0 {
                break;
            }
            if FED.fetch_add(1, Ordering::Relaxed) < LOG_PACKETS {
                log!("relay: fed {len} bytes to the connection manager");
            }
            let mut event = Event {
                vtable: 0,
                _pad: [0; 28],
                kind: EVENT_PACKET_RECEIVED,
                address: [0; 16],
                data: buf.as_ptr(),
                len: len as usize,
            };
            event.address[..4].copy_from_slice(&u32::MAX.to_le_bytes());
            event.address[8..].copy_from_slice(&from);
            on_relay(manager, &mut event);
        }
        let mut ticked =
            Event { vtable: 0, _pad: [0; 28], kind: EVENT_RECEIVE_TICKED, address: [0; 16], data: std::ptr::null(), len: 0 };
        on_ticked(manager, &mut ticked);
    }
}
