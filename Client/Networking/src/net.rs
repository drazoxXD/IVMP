//! Connection to the IVMP server and the tunnel for the engine's own UDP traffic.
//!
//! The engine sends and receives everything through `rage::netSocket` (Send 0x6F1A60, Receive
//! 0x6F1AE0). Once we are connected, both are redirected: outgoing packets go to the server
//! wrapped in [`ClientMessage::Tunnel`], and packets the server forwards are handed back to the
//! engine as if they came from the peer's virtual address (10.0.0.<id>).

use std::collections::VecDeque;
use std::net::{Ipv4Addr, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use NetProtocol::{ClientMessage, KEEPALIVE_SECS, MAX_GAME_PAYLOAD, PROTOCOL_VERSION, ServerMessage, virtual_ip};

use crate::instance::SockAddrIn;
use crate::{log, memory};

const NET_SOCKET_SEND: usize = 0x006F_1A60;
/// ws2_32 import slots: `sendto` (ordinal 20) and `recvfrom` (ordinal 17). Some engine traffic
/// (session join among it) goes straight through these rather than the netSocket wrappers, so
/// we tunnel at this level too, as LibertyM does.
const SENDTO_IMPORT: usize = 0x00E7_34FC;
const RECVFROM_IMPORT: usize = 0x00E7_3504;
const NET_SOCKET_RECEIVE: usize = 0x006F_1AE0;
/// Packets waiting for the engine; older ones are dropped beyond this.
const INBOX_LIMIT: usize = 1024;
/// How many tunnelled packets to log in each direction (diagnostics).
const LOG_PACKETS: u32 = 20;

/// The engine's `netSocketAddress`: IPv4 and port in host byte order.
#[repr(C)]
struct NetSocketAddress {
    ip: u32,
    port: u16,
}

struct Packet {
    from: Ipv4Addr,
    from_port: u16,
    payload: Vec<u8>,
}

struct Connection {
    socket: UdpSocket,
    inbox: Mutex<VecDeque<Packet>>,
}

static CONNECTION: OnceLock<Connection> = OnceLock::new();
/// Our client id once welcomed (0 = not yet).
static OUR_ID: AtomicU8 = AtomicU8::new(0);
static HOST_ID: AtomicU8 = AtomicU8::new(0);
/// Our network object id block from the server: first << 16 | count (0 = not given yet).
static OBJECT_IDS: AtomicU32 = AtomicU32::new(0);
static ORIGINAL_SEND: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_RECEIVE: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_SENDTO: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_RECVFROM: AtomicUsize = AtomicUsize::new(0);
static SENDTO_LOGGED: AtomicU32 = AtomicU32::new(0);
static SENT: AtomicU32 = AtomicU32::new(0);
static RECEIVED: AtomicU32 = AtomicU32::new(0);

/// Tunnelled packets so far: (sent, received).
pub fn packet_counts() -> (u32, u32) {
    (SENT.load(Ordering::Relaxed), RECEIVED.load(Ordering::Relaxed))
}

pub fn our_id() -> Option<u8> {
    Some(OUR_ID.load(Ordering::SeqCst)).filter(|id| *id != 0)
}

pub fn host_id() -> Option<u8> {
    Some(HOST_ID.load(Ordering::SeqCst)).filter(|id| *id != 0)
}

/// The network object ids the server gave us: `first..first + count`.
pub fn object_ids() -> Option<(u16, u16)> {
    let ids = OBJECT_IDS.load(Ordering::SeqCst);
    (ids != 0).then_some(((ids >> 16) as u16, ids as u16))
}

pub fn is_host() -> bool {
    our_id().is_some_and(|id| id == HOST_ID.load(Ordering::SeqCst))
}

/// Connects to the server in the background and installs the netSocket hooks.
pub fn start(server: &str, nickname: &str) {
    let _ = crate::NICKNAME.set(nickname.to_string());
    install_socket_hooks();
    let (server, nickname) = (server.to_string(), nickname.to_string());
    std::thread::spawn(move || {
        if let Err(e) = run(&server, &nickname) {
            log!("net: {e}");
        }
    });
}

fn run(server: &str, nickname: &str) -> std::io::Result<()> {
    let address = server
        .to_socket_addrs()?
        .find(|a| a.is_ipv4())
        .ok_or_else(|| std::io::Error::other(format!("no IPv4 address for {server}")))?;
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.connect(address)?;
    socket.set_read_timeout(Some(Duration::from_millis(250)))?;
    let connection = CONNECTION.get_or_init(|| Connection { socket, inbox: Mutex::new(VecDeque::new()) });
    log!("net: connecting to {address} as {nickname:?}");

    let hello = ClientMessage::Hello { version: PROTOCOL_VERSION, nickname: nickname.to_string() }.encode();
    let mut last_sent = Instant::now() - Duration::from_secs(60);
    let mut buf = [0u8; 2048];
    loop {
        let interval = if our_id().is_some() { Duration::from_secs(KEEPALIVE_SECS) } else { Duration::from_secs(1) };
        if last_sent.elapsed() >= interval {
            let message = if our_id().is_some() { ClientMessage::KeepAlive.encode() } else { hello.clone() };
            let _ = connection.socket.send(&message);
            last_sent = Instant::now();
        }
        let len = match connection.socket.recv(&mut buf) {
            Ok(len) => len,
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => continue,
            // The server isn't up yet (ICMP port unreachable); keep trying.
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => continue,
            Err(e) => return Err(e),
        };
        match ServerMessage::decode(&buf[..len]) {
            Ok(ServerMessage::Welcome { id, host }) => {
                if our_id().is_none() {
                    log!("net: joined as {} (id {id}), host is id {host}", virtual_ip(id));
                }
                OUR_ID.store(id, Ordering::SeqCst);
                HOST_ID.store(host, Ordering::SeqCst);
            }
            Ok(ServerMessage::ObjectIds { first, count }) => {
                if object_ids().is_none() {
                    log!("net: server gave us object ids {first}..{}", first + count);
                }
                OBJECT_IDS.store((first as u32) << 16 | count as u32, Ordering::SeqCst);
            }
            Ok(ServerMessage::Reject(reason)) => {
                log!("net: server rejected us: {reason:?}");
                return Ok(());
            }
            Ok(ServerMessage::PeerJoined { id, nickname }) => log!("net: {nickname} is {}", virtual_ip(id)),
            Ok(ServerMessage::PeerLeft { id }) => log!("net: {} left", virtual_ip(id)),
            Ok(ServerMessage::HostChanged { host }) => {
                HOST_ID.store(host, Ordering::SeqCst);
                log!("net: host is now id {host}");
            }
            Ok(ServerMessage::Tunnel { from, from_port, to_port: _, payload }) => {
                if RECEIVED.fetch_add(1, Ordering::Relaxed) < LOG_PACKETS {
                    log!("net: <- {from}:{from_port} {} bytes", payload.len());
                }
                let mut inbox = connection.inbox.lock().unwrap();
                if inbox.len() >= INBOX_LIMIT {
                    inbox.pop_front();
                }
                inbox.push_back(Packet { from, from_port, payload });
            }
            Err(e) => log!("net: bad packet from server: {e:?}"),
        }
    }
}

fn connected() -> Option<&'static Connection> {
    CONNECTION.get().filter(|_| our_id().is_some())
}

fn install_socket_hooks() {
    for (target, hook, original, name) in [
        (NET_SOCKET_SEND, socket_send as usize, &ORIGINAL_SEND, "send"),
        (NET_SOCKET_RECEIVE, socket_receive as usize, &ORIGINAL_RECEIVE, "receive"),
    ] {
        let target = memory::from_ghidra(target);
        original.store(target, Ordering::SeqCst);
        let calls = memory::find_calls_to(target);
        for call in &calls {
            if let Err(e) = unsafe { memory::redirect_call(*call, hook) } {
                log!("net: netSocket {name} hook at {call:#x} failed: {e}");
            }
        }
        log!("net: netSocket {name} hooked at {} call sites", calls.len());
    }
    for (slot, hook, original, name) in [
        (SENDTO_IMPORT, sendto as usize, &ORIGINAL_SENDTO, "sendto"),
        (RECVFROM_IMPORT, recvfrom as usize, &ORIGINAL_RECVFROM, "recvfrom"),
    ] {
        let slot = memory::from_ghidra(slot);
        original.store(unsafe { std::ptr::read_unaligned(slot as *const usize) }, Ordering::SeqCst);
        match unsafe { memory::patch(slot, &(hook as u32).to_le_bytes()) } {
            Ok(()) => log!("net: {name} hooked"),
            Err(e) => log!("net: {name} hook failed: {e}"),
        }
    }
}

type SendTo = unsafe extern "system" fn(usize, *const u8, i32, i32, *const SockAddrIn, i32) -> i32;
type RecvFrom = unsafe extern "system" fn(usize, *mut u8, i32, i32, *mut SockAddrIn, *mut i32) -> i32;

/// Peers live at our virtual addresses (10.0.0.x).
fn is_virtual(ip: Ipv4Addr) -> bool {
    ip.octets()[0] == 10
}

unsafe extern "system" fn sendto(socket: usize, buf: *const u8, len: i32, flags: i32, to: *const SockAddrIn, to_len: i32) -> i32 {
    unsafe {
        let destination = (!to.is_null()).then(|| (Ipv4Addr::from(u32::from_be((*to).addr)), u16::from_be((*to).port)));
        let tunnel = connected().zip(destination).filter(|(_, (ip, _))| is_virtual(*ip) && !buf.is_null() && len >= 0);
        if SENDTO_LOGGED.fetch_add(1, Ordering::Relaxed) < LOG_PACKETS {
            if let Some((ip, port)) = destination {
                let how = if tunnel.is_some() { "tunnelled" } else { "direct" };
                log!("net: sendto {ip}:{port} {len} bytes ({how})");
            }
        }
        let Some((connection, (ip, port))) = tunnel else {
            let original: SendTo = std::mem::transmute(ORIGINAL_SENDTO.load(Ordering::SeqCst));
            return original(socket, buf, len, flags, to, to_len);
        };
        let payload = std::slice::from_raw_parts(buf, len as usize).to_vec();
        let message = ClientMessage::Tunnel { from_port: crate::peer::GAME_PORT, to: ip, to_port: port, payload };
        let _ = connection.socket.send(&message.encode());
        len
    }
}

unsafe extern "system" fn recvfrom(socket: usize, buf: *mut u8, len: i32, flags: i32, from: *mut SockAddrIn, from_len: *mut i32) -> i32 {
    unsafe {
        let game_socket = crate::instance::game_socket();
        if let Some(connection) = connected().filter(|_| game_socket != 0 && socket == game_socket && !buf.is_null() && len > 0) {
            if let Some(packet) = connection.inbox.lock().unwrap().pop_front() {
                let n = packet.payload.len().min(len as usize);
                std::ptr::copy_nonoverlapping(packet.payload.as_ptr(), buf, n);
                if !from.is_null() {
                    *from = SockAddrIn {
                        family: 2,
                        port: packet.from_port.to_be(),
                        addr: u32::from(packet.from).to_be(),
                        zero: [0; 8],
                    };
                    if !from_len.is_null() {
                        *from_len = size_of::<SockAddrIn>() as i32;
                    }
                }
                return n as i32;
            }
        }
        let original: RecvFrom = std::mem::transmute(ORIGINAL_RECVFROM.load(Ordering::SeqCst));
        original(socket, buf, len, flags, from, from_len)
    }
}

extern "thiscall" fn socket_send(
    this: *mut u8,
    to: *const NetSocketAddress,
    buf: *const u8,
    len: i32,
    err: *mut i32,
) -> bool {
    unsafe {
        let Some(connection) = connected().filter(|_| !to.is_null() && !buf.is_null() && len >= 0) else {
            let original: extern "thiscall" fn(*mut u8, *const NetSocketAddress, *const u8, i32, *mut i32) -> bool =
                std::mem::transmute(ORIGINAL_SEND.load(Ordering::SeqCst));
            return original(this, to, buf, len, err);
        };
        let to = &*to;
        let payload = std::slice::from_raw_parts(buf, len as usize).to_vec();
        if SENT.fetch_add(1, Ordering::Relaxed) < LOG_PACKETS {
            log!("net: -> {}:{} {} bytes", Ipv4Addr::from(to.ip), to.port, payload.len());
        }
        // Peers know us by the game port in our peer address, whatever port this socket really
        // has (the second client binds 6673).
        let message =
            ClientMessage::Tunnel { from_port: crate::peer::GAME_PORT, to: Ipv4Addr::from(to.ip), to_port: to.port, payload };
        let _ = connection.socket.send(&message.encode());
        if !err.is_null() {
            *err = 0;
        }
        true
    }
}

extern "thiscall" fn socket_receive(
    this: *mut u8,
    from: *mut NetSocketAddress,
    buf: *mut u8,
    max_len: i32,
    err: *mut i32,
) -> i32 {
    unsafe {
        let Some(connection) = connected() else {
            let original: extern "thiscall" fn(*mut u8, *mut NetSocketAddress, *mut u8, i32, *mut i32) -> i32 =
                std::mem::transmute(ORIGINAL_RECEIVE.load(Ordering::SeqCst));
            return original(this, from, buf, max_len, err);
        };
        if !err.is_null() {
            *err = 0;
        }
        let Some(packet) = connection.inbox.lock().unwrap().pop_front() else {
            return 0;
        };
        // The engine reads with a 0x400 buffer regardless of max_len.
        let len = packet.payload.len().min(MAX_GAME_PAYLOAD);
        std::ptr::copy_nonoverlapping(packet.payload.as_ptr(), buf, len);
        if !from.is_null() {
            (*from).ip = u32::from(packet.from);
            (*from).port = packet.from_port;
        }
        len as i32
    }
}
