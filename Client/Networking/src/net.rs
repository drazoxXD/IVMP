//! Connection to the IVMP server and the tunnel for the engine's own UDP traffic.
//!
//! The engine sends and receives everything through `rage::netSocket` (Send 0x6F1A60, Receive
//! 0x6F1AE0). Once we are connected, both are redirected: outgoing packets go to the server
//! wrapped in [`ClientMessage::Tunnel`], and packets the server forwards are handed back to the
//! engine as if they came from the peer's virtual address (10.0.0.<id>).

use std::collections::VecDeque;
use std::net::{Ipv4Addr, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use NetProtocol::reliable::{Accept, ReliableIn, ReliableOut};
use NetProtocol::{ClientMessage, KEEPALIVE_SECS, MAX_GAME_PAYLOAD, PROTOCOL_VERSION, RejectReason, ServerMessage, virtual_ip};

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
/// Control messages to the server that are resent until it acknowledges them, and the ones from
/// the server (delivered once, in order). Both start over with each join.
static RELIABLE_OUT: Mutex<Option<ReliableOut>> = Mutex::new(None);
static RELIABLE_IN: Mutex<Option<ReliableIn>> = Mutex::new(None);

static OUR_ID: AtomicU8 = AtomicU8::new(0);
static HOST_ID: AtomicU8 = AtomicU8::new(0);
/// The session's game mode and slots from the server (server.cfg); Free Mode, 32 until told.
static GAME_MODE: AtomicI32 = AtomicI32::new(FREE_MODE);
static SLOTS: AtomicU8 = AtomicU8::new(32);
/// The game's stored network game config (30 dwords: +0 mode, +8 slots), found by the boot code.
static NETWORK_CONFIG: AtomicUsize = AtomicUsize::new(0);
const FREE_MODE: i32 = 16;
/// `gamemode = -1`: players land in the game's own multiplayer lobby (network_startup) and
/// host, find and pick the game there; IVMP doesn't host or join for them.
pub const LOBBY_MODE: i32 = -1;
/// Called (from the network thread) when the server's game mode changes; the boot code uses it.
static SETTINGS_HOOK: OnceLock<fn(i32)> = OnceLock::new();

/// Registers the function told about the server's game mode.
pub fn on_game_settings(hook: fn(i32)) {
    let _ = SETTINGS_HOOK.set(hook);
}

/// The session host told the server it starts a new session (lobby game type change); taken
/// by the host system on the game thread.
static SESSION_RESTART: AtomicBool = AtomicBool::new(false);

/// True once after the host started a new session ([`ServerMessage::SessionRestart`]).
pub fn take_session_restart() -> bool {
    SESSION_RESTART.swap(false, Ordering::SeqCst)
}

/// The host's new session is in its lobby ([`ServerMessage::SessionReady`]); taken by the host
/// system before it joins.
static SESSION_READY: AtomicBool = AtomicBool::new(false);
/// We host and told the server we start a new session; [`tell_session_ready`] is due.
static RESTART_ANNOUNCED: AtomicBool = AtomicBool::new(false);

/// True once after the host's new session became ready.
pub fn take_session_ready() -> bool {
    SESSION_READY.swap(false, Ordering::SeqCst)
}

/// Forgets a ready notice from before (the host is restarting again).
pub fn clear_session_ready() {
    SESSION_READY.store(false, Ordering::SeqCst);
}

/// We host and our game ends its session for a new one: the server tells the others
/// ([`ServerMessage::SessionRestart`]).
pub fn tell_session_restart() {
    if is_host() {
        log!("net: telling the server we start a new session");
        RESTART_ANNOUNCED.store(true, Ordering::SeqCst);
        send_reliable(&ClientMessage::SessionRestart);
    }
}

/// Our new session (after [`tell_session_restart`]) is in its lobby: the others may join now.
/// Sent once per restart.
pub fn tell_session_ready() {
    if RESTART_ANNOUNCED.swap(false, Ordering::SeqCst) {
        log!("net: telling the server our new session is ready");
        send_reliable(&ClientMessage::SessionReady);
    }
}

/// The server wants the game's own lobby (`gamemode = -1`).
pub fn lobby_mode() -> bool {
    GAME_MODE.load(Ordering::SeqCst) == LOBBY_MODE
}

/// The network game mode and slots to host and join with. IVMP's own modes (negative, e.g. -2
/// scripted) run on Free Mode for now.
pub fn game_settings() -> (i32, u32) {
    let mode = GAME_MODE.load(Ordering::SeqCst);
    (if mode < 0 { FREE_MODE } else { mode }, SLOTS.load(Ordering::SeqCst) as u32)
}

/// Where the game keeps its network game config; the server's settings are written there too.
pub fn set_network_config(config: usize) {
    NETWORK_CONFIG.store(config, Ordering::SeqCst);
    write_network_config();
}

fn write_network_config() {
    let config = NETWORK_CONFIG.load(Ordering::SeqCst) as *mut i32;
    if config.is_null() {
        return;
    }
    let (mode, slots) = game_settings();
    unsafe {
        *config = mode;
        *config.add(2) = slots as i32;
    }
}

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

/// What the connection to the IVMP server is doing, in words (for the overlay and the console).
static STATUS: Mutex<String> = Mutex::new(String::new());

fn set_status(text: impl Into<String>) {
    let text = text.into();
    let mut status = STATUS.lock().unwrap();
    if *status != text {
        log!("net: {text}");
        *status = text;
    }
}

/// The connection state as a sentence: "Connecting to ...", "Connected ...", or what is wrong.
pub fn status() -> String {
    let status = STATUS.lock().unwrap();
    if status.is_empty() { "Not started".to_string() } else { status.clone() }
}

/// The status when something is wrong (the server doesn't answer, was lost, refused us, ...), None
/// while connecting or connected.
pub fn connection_problem() -> Option<String> {
    let status = status();
    (!status.starts_with("Connecting") && !status.starts_with("Connected") && status != "Not started").then_some(status)
}

fn reject_text(reason: &RejectReason) -> String {
    match reason {
        RejectReason::VersionMismatch => {
            format!("The server refused us: version mismatch (this client speaks protocol {PROTOCOL_VERSION}; update the client or the server)")
        }
        RejectReason::ServerFull => "The server refused us: it is full".to_string(),
        RejectReason::BadNickname => "The server refused us: the nickname is not allowed".to_string(),
        RejectReason::Refused => "The server refused us: a script rejected the connection".to_string(),
    }
}

/// Connects to the server in the background and installs the netSocket hooks.
pub fn start(server: &str, nickname: &str) {
    let _ = crate::NICKNAME.set(nickname.to_string());
    install_socket_hooks();
    let (server, nickname) = (server.to_string(), nickname.to_string());
    std::thread::spawn(move || {
        set_status(format!("Connecting to {server}..."));
        if let Err(e) = run(&server, &nickname) {
            set_status(format!("Could not connect to {server}: {e}"));
        }
    });
}

/// The port a server listens on unless its address says otherwise.
const DEFAULT_PORT: u16 = 9999;

/// `host` or `host:port` to something `to_socket_addrs` takes: no port means the default one.
fn with_port(server: &str) -> String {
    let server = server.trim();
    if server.rsplit_once(':').is_some_and(|(_, port)| port.parse::<u16>().is_ok()) {
        server.to_string()
    } else {
        format!("{server}:{DEFAULT_PORT}")
    }
}

fn run(server: &str, nickname: &str) -> std::io::Result<()> {
    let address = with_port(server)
        .to_socket_addrs()
        .map_err(|e| std::io::Error::other(format!("{e} (write the server as host:port, e.g. play.example.com:25568)")))?
        .find(|a| a.is_ipv4())
        .ok_or_else(|| std::io::Error::other(format!("no IPv4 address for {server}")))?;
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.connect(address)?;
    socket.set_read_timeout(Some(Duration::from_millis(250)))?;
    let connection = CONNECTION.get_or_init(|| Connection { socket, inbox: Mutex::new(VecDeque::new()) });
    log!("net: connecting to {address} as {nickname:?}");
    set_status(format!("Connecting to {address}..."));
    let mut started = Instant::now();
    let mut last_heard = Instant::now();
    let mut connected_text = String::new();

    let hello = ClientMessage::Hello { version: PROTOCOL_VERSION, nickname: nickname.to_string() }.encode();
    let mut last_sent = Instant::now() - Duration::from_secs(60);
    let mut buf = [0u8; 2048];
    loop {
        let interval = if our_id().is_some() { Duration::from_secs(KEEPALIVE_SECS.min(2)) } else { Duration::from_secs(1) };
        if last_sent.elapsed() >= interval {
            // While connected the ping doubles as the keep-alive.
            let message = if our_id().is_some() { ClientMessage::Ping { stamp: clock_ms() }.encode() } else { hello.clone() };
            let _ = connection.socket.send(&message);
            last_sent = Instant::now();
        }
        // Control messages the server has not acknowledged yet.
        if let Some(out) = RELIABLE_OUT.lock().unwrap().as_mut() {
            let (resend, dropped) = out.due(Instant::now());
            for packet in resend {
                let _ = connection.socket.send(&packet);
            }
            if dropped > 0 {
                log!("net: gave up on {dropped} control message(s) to the server: no acknowledgement");
            }
        }
        // What is wrong, if anything: no answer to the hello, or a connection that went quiet.
        if our_id().is_none() && started.elapsed() > Duration::from_secs(5) {
            set_status(format!("No answer from {address}: is the server running, and is its UDP port open (firewall, port forwarding)?"));
        } else if our_id().is_some() && last_heard.elapsed() > Duration::from_secs(8) {
            // The server may have been restarted: it knows nobody then and ignores everything but a
            // hello, so say hello again like on the first connect (the game's own session goes on
            // between the players meanwhile). A new welcome starts new reliable streams.
            set_status(format!("Connection lost: nothing from the server for {} s, reconnecting to {address}...", last_heard.elapsed().as_secs()));
            OUR_ID.store(0, Ordering::SeqCst);
            HOST_ID.store(0, Ordering::SeqCst);
            connected_text.clear();
            started = Instant::now();
            last_heard = Instant::now();
            last_sent = Instant::now() - Duration::from_secs(60);
        }
        let len = match connection.socket.recv(&mut buf) {
            Ok(len) => {
                last_heard = Instant::now();
                if our_id().is_some() && !connected_text.is_empty() {
                    set_status(connected_text.clone());
                }
                len
            }
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => continue,
            // The server isn't up yet (ICMP port unreachable); keep trying.
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => continue,
            Err(e) => return Err(e),
        };
        let message = match ServerMessage::decode(&buf[..len]) {
            Ok(ServerMessage::Reliable { seq, body }) => {
                let accepted = RELIABLE_IN.lock().unwrap().get_or_insert_with(ReliableIn::default).accept(seq);
                match accepted {
                    Accept::Deliver { ack } => {
                        send(&ClientMessage::Ack { seq: ack });
                        match ServerMessage::decode(&body) {
                            Ok(inner) => inner,
                            Err(e) => {
                                log!("net: bad reliable message from the server: {e:?}");
                                continue;
                            }
                        }
                    }
                    Accept::Duplicate { ack } => {
                        send(&ClientMessage::Ack { seq: ack });
                        continue;
                    }
                    Accept::Ahead => continue,
                }
            }
            Ok(ServerMessage::Ack { seq }) => {
                if let Some(out) = RELIABLE_OUT.lock().unwrap().as_mut() {
                    out.ack(seq);
                }
                continue;
            }
            Ok(message) => message,
            Err(e) => {
                log!("net: bad packet from server: {e:?}");
                continue;
            }
        };
        match message {
            ServerMessage::Welcome { id, host } => {
                if our_id().is_none() {
                    log!("net: joined as {} (id {id}), host is id {host}", virtual_ip(id));
                    // A new join starts new reliable streams (the server's side is fresh too).
                    *RELIABLE_OUT.lock().unwrap() = Some(ReliableOut::default());
                    *RELIABLE_IN.lock().unwrap() = Some(ReliableIn::default());
                }
                OUR_ID.store(id, Ordering::SeqCst);
                HOST_ID.store(host, Ordering::SeqCst);
                connected_text = format!("Connected to {address} as {} (id {id})", virtual_ip(id));
                set_status(connected_text.clone());
            }
            ServerMessage::ObjectIds { first, count } => {
                if object_ids().is_none() {
                    log!("net: server gave us object ids {first}..{}", first + count);
                }
                OBJECT_IDS.store((first as u32) << 16 | count as u32, Ordering::SeqCst);
            }
            ServerMessage::GameSettings { mode, slots } => {
                let mode_changed = GAME_MODE.swap(mode, Ordering::SeqCst) != mode;
                if mode_changed || SLOTS.swap(slots, Ordering::SeqCst) != slots {
                    log!("net: server game mode {mode}, {slots} slots (playing mode {})", game_settings().0);
                }
                if mode_changed {
                    if let Some(hook) = SETTINGS_HOOK.get() {
                        hook(mode);
                    }
                }
                write_network_config();
            }
            ServerMessage::Reject(reason) => {
                set_status(reject_text(&reason));
                return Ok(());
            }
            ServerMessage::PeerJoined { id, nickname } => log!("net: {nickname} is {}", virtual_ip(id)),
            ServerMessage::PeerLeft { id } => log!("net: {} left", virtual_ip(id)),
            ServerMessage::HostChanged { host } => {
                HOST_ID.store(host, Ordering::SeqCst);
                log!("net: host is now id {host}");
            }
            ServerMessage::SessionRestart { host } => {
                log!("net: the host {} starts a new session", virtual_ip(host));
                SESSION_READY.store(false, Ordering::SeqCst);
                SESSION_RESTART.store(true, Ordering::SeqCst);
            }
            ServerMessage::Pong { stamp, up, down } => {
                PING_MS.store(clock_ms().wrapping_sub(stamp).max(1), Ordering::Relaxed);
                SERVER_UP.store(up, Ordering::Relaxed);
                SERVER_DOWN.store(down, Ordering::Relaxed);
                relay_check(up, down);
            }
            ServerMessage::SessionReady { host } => {
                log!("net: the host {}'s new session is ready", virtual_ip(host));
                SESSION_READY.store(true, Ordering::SeqCst);
            }
            ServerMessage::Tunnel { from, from_port, to_port: _, payload } => {
                if RECEIVED.fetch_add(1, Ordering::Relaxed) < LOG_PACKETS {
                    log!("net: <- {from}:{from_port} {} bytes", payload.len());
                }
                let mut inbox = connection.inbox.lock().unwrap();
                if inbox.len() >= INBOX_LIMIT {
                    inbox.pop_front();
                }
                inbox.push_back(Packet { from, from_port, payload });
            }
            ServerMessage::Spawn { request, kind, model, position, heading } => {
                crate::spawner::request(crate::spawner::Request { id: request, kind, model, position, heading });
            }
            ServerMessage::Despawn { kind, object_id } => crate::spawner::despawn(kind, object_id),
            ServerMessage::ScriptEvent { name, payload } => {
                let json = String::from_utf8_lossy(&payload).to_string();
                log!("net: script event '{name}' {json}");
                let mut events = SCRIPT_EVENTS.lock().unwrap();
                if events.len() >= MAX_QUEUED_SCRIPT_EVENTS {
                    events.pop_front();
                }
                events.push_back((name, json));
            }
            // Handled above; a reliable message inside a reliable one is ignored.
            ServerMessage::Reliable { .. } | ServerMessage::Ack { .. } => {}
        }
    }
}

/// Milliseconds since the first call (wraps after 49 days), the stamp of a ping.
fn clock_ms() -> u32 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u32
}

/// Game packets the server says it received from us / sent to us (from the latest pong).
static SERVER_UP: AtomicU32 = AtomicU32::new(0);
static SERVER_DOWN: AtomicU32 = AtomicU32::new(0);

/// (sent by us, received by the server, sent by the server, received by us): where game packets are
/// lost shows in the differences (uplink: first minus second, downlink: third minus fourth).
pub fn relay_stats() -> (u32, u32, u32, u32) {
    let (sent, received) = packet_counts();
    (sent, SERVER_UP.load(Ordering::Relaxed), SERVER_DOWN.load(Ordering::Relaxed), received)
}

/// Warns once when game packets are clearly not getting through (nothing arrives at the server, or
/// nothing arrives here although the server sends): "other players stay invisible".
fn relay_check(up: u32, down: u32) {
    static UP_WARNED: AtomicBool = AtomicBool::new(false);
    static DOWN_WARNED: AtomicBool = AtomicBool::new(false);
    let (sent, received) = packet_counts();
    if sent >= 100 && up == 0 && !UP_WARNED.swap(true, Ordering::Relaxed) {
        log!("net: the server received none of our {sent} game packets (control messages get through, so the game port is the problem?)");
    }
    if down >= 100 && received == 0 && !DOWN_WARNED.swap(true, Ordering::Relaxed) {
        log!("net: the server sent us {down} game packets but none arrived: other players will stay invisible");
    }
}

/// Round trip to the server in milliseconds, 0 until the first answer.
static PING_MS: AtomicU32 = AtomicU32::new(0);

pub fn ping_ms() -> u32 {
    PING_MS.load(Ordering::Relaxed)
}

/// Script events the server sent (`TriggerClientEvent`), as (name, JSON arguments), until the client
/// script runtime takes them.
static SCRIPT_EVENTS: Mutex<VecDeque<(String, String)>> = Mutex::new(VecDeque::new());
const MAX_QUEUED_SCRIPT_EVENTS: usize = 256;

/// The script events received since the last call.
pub fn take_script_events() -> Vec<(String, String)> {
    SCRIPT_EVENTS.lock().unwrap().drain(..).collect()
}

/// `TriggerServerEvent(name, ...)`: `json` is the JSON array of the arguments. Returns an error
/// text if the event can't be sent.
pub fn trigger_server_event(name: &str, json: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 255 {
        return Err("the event name must be 1 to 255 characters".into());
    }
    if json.len() > NetProtocol::MAX_SCRIPT_PAYLOAD {
        return Err(format!("the arguments are too big ({} bytes, at most {})", json.len(), NetProtocol::MAX_SCRIPT_PAYLOAD));
    }
    if connected().is_none() {
        return Err("not connected".into());
    }
    send_reliable(&ClientMessage::ScriptEvent { name: name.to_string(), payload: json.as_bytes().to_vec() });
    Ok(())
}

/// Sends a control message that is resent until the server acknowledges it (any thread).
pub fn send_reliable(message: &ClientMessage) {
    if let Some(connection) = connected() {
        let body = message.encode();
        let bytes = RELIABLE_OUT
            .lock()
            .unwrap()
            .get_or_insert_with(ReliableOut::default)
            .queue(|seq| ClientMessage::Reliable { seq, body }.encode());
        let _ = connection.socket.send(&bytes);
    }
}

/// Sends a message to the server (any thread). Dropped when not connected yet.
pub fn send(message: &ClientMessage) {
    if let Some(connection) = connected() {
        let _ = connection.socket.send(&message.encode());
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
