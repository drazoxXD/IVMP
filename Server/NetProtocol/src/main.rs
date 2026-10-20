//! IVMP server: lobby + relay.
//!
//! Usage: `ivmp_server [bind address] [--capture <file>]` (default `0.0.0.0:9999`). Output also
//! goes to `ivmp_server.log` next to the executable.
//!
//! `--capture` records every tunnelled game packet for analysis (sync work, docs/onesync-plan.md):
//! records of `u64 milliseconds since start, u8 from id, u8 to id (0 = broadcast), u16 length,
//! payload`, little endian.
//!
//! Clients say hello, get a virtual address (10.0.0.<id>) and the id of the client that hosts the
//! engine session. After that the server forwards game packets between clients by virtual address.
//! The first client to join hosts; when the host leaves, the oldest remaining client takes over.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use ivmp_sync::registry::Registry;
use NetProtocol::{
    BROADCAST, ClientId, ClientMessage, DEFAULT_PORT, PROTOCOL_VERSION, RejectReason, ServerMessage, TIMEOUT_SECS,
    client_id_of, virtual_ip,
};

const MAX_CLIENTS: usize = 32;

/// `ivmp_server.log` next to the executable, so what the console shows can be read later.
static LOG: std::sync::Mutex<Option<File>> = std::sync::Mutex::new(None);

fn open_log() {
    let path = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("ivmp_server.log")))
        .unwrap_or_else(|| "ivmp_server.log".into());
    *LOG.lock().unwrap() = File::create(path).ok();
}

/// Prints a line to the console and the log file.
macro_rules! say {
    ($($arg:tt)*) => {{
        let line = format!($($arg)*);
        println!("{line}");
        if let Some(file) = LOG.lock().unwrap().as_mut() {
            let _ = writeln!(file, "{line}");
        }
    }};
}

struct Client {
    addr: SocketAddr,
    nickname: String,
    last_seen: Instant,
}

struct Server {
    socket: UdpSocket,
    /// Ordered by id, which is also join order, so the first entry is the oldest client.
    clients: BTreeMap<ClientId, Client>,
    host: Option<ClientId>,
    capture: Option<BufWriter<File>>,
    started: Instant,
    /// Every entity seen in the clients' game traffic (docs/onesync-plan.md).
    entities: Registry,
    last_summary: Instant,
}

impl Server {
    fn send(&self, to: SocketAddr, message: &ServerMessage) {
        if let Err(e) = self.socket.send_to(&message.encode(), to) {
            say!("send to {to} failed: {e}");
        }
    }

    fn broadcast(&self, except: Option<ClientId>, message: &ServerMessage) {
        let bytes = message.encode();
        for (id, client) in &self.clients {
            if Some(*id) != except {
                let _ = self.socket.send_to(&bytes, client.addr);
            }
        }
    }

    fn id_of(&self, addr: SocketAddr) -> Option<ClientId> {
        self.clients.iter().find(|(_, c)| c.addr == addr).map(|(id, _)| *id)
    }

    fn free_id(&self) -> Option<ClientId> {
        (1..=MAX_CLIENTS as ClientId).find(|id| !self.clients.contains_key(id))
    }

    fn handle(&mut self, from: SocketAddr, message: ClientMessage) {
        let sender = self.id_of(from);
        if let Some(id) = sender {
            self.clients.get_mut(&id).unwrap().last_seen = Instant::now();
        }
        match (message, sender) {
            (ClientMessage::Hello { version, nickname }, existing) => {
                if version != PROTOCOL_VERSION {
                    return self.send(from, &ServerMessage::Reject(RejectReason::VersionMismatch));
                }
                let nickname = nickname.trim().to_string();
                if nickname.is_empty() || nickname.len() > 32 {
                    return self.send(from, &ServerMessage::Reject(RejectReason::BadNickname));
                }
                // A repeated hello (lost welcome) just gets the welcome again.
                let id = match existing {
                    Some(id) => id,
                    None => {
                        let Some(id) = self.free_id() else {
                            return self.send(from, &ServerMessage::Reject(RejectReason::ServerFull));
                        };
                        self.clients.insert(id, Client { addr: from, nickname: nickname.clone(), last_seen: Instant::now() });
                        say!(
                            "{nickname} joined from {from} as {} (id {id}), object ids {:?}",
                            virtual_ip(id),
                            ivmp_sync::ids::block(id)
                        );
                        self.broadcast(Some(id), &ServerMessage::PeerJoined { id, nickname: nickname.clone() });
                        for (other, client) in &self.clients {
                            if *other != id {
                                self.send(from, &ServerMessage::PeerJoined { id: *other, nickname: client.nickname.clone() });
                            }
                        }
                        id
                    }
                };
                let host = *self.host.get_or_insert(id);
                self.send(from, &ServerMessage::Welcome { id, host });
                let (first, count) = ivmp_sync::ids::block(id);
                self.send(from, &ServerMessage::ObjectIds { first, count });
            }
            (ClientMessage::Tunnel { from_port, to, to_port, payload }, Some(id)) => {
                if let Some(capture) = self.capture.as_mut() {
                    let to_id = if to == BROADCAST { 0 } else { client_id_of(to).unwrap_or(0) };
                    let mut record = Vec::with_capacity(12 + payload.len());
                    record.extend_from_slice(&(self.started.elapsed().as_millis() as u64).to_le_bytes());
                    record.extend_from_slice(&[id, to_id]);
                    record.extend_from_slice(&(payload.len() as u16).to_le_bytes());
                    record.extend_from_slice(&payload);
                    let _ = capture.write_all(&record);
                    let _ = capture.flush();
                }
                for line in self.entities.observe(id, &payload) {
                    say!("{line}");
                }
                let packet = ServerMessage::Tunnel { from: virtual_ip(id), from_port, to_port, payload };
                if to == BROADCAST {
                    self.broadcast(Some(id), &packet);
                } else if let Some(target) = client_id_of(to).and_then(|t| self.clients.get(&t)) {
                    self.send(target.addr, &packet);
                }
            }
            (ClientMessage::KeepAlive, Some(_)) => {}
            (ClientMessage::Bye, Some(id)) => self.remove(id, "left"),
            // Anything but hello from an unknown address is ignored.
            (_, None) => {}
        }
    }

    fn remove(&mut self, id: ClientId, why: &str) {
        let Some(client) = self.clients.remove(&id) else { return };
        say!("{} ({}) {why}", client.nickname, virtual_ip(id));
        self.broadcast(None, &ServerMessage::PeerLeft { id });
        if self.host == Some(id) {
            self.host = self.clients.keys().next().copied();
            if let Some(host) = self.host {
                say!("host is now {}", self.clients[&host].nickname);
                self.broadcast(None, &ServerMessage::HostChanged { host });
            }
        }
    }

    fn drop_timed_out(&mut self) {
        let timeout = Duration::from_secs(TIMEOUT_SECS);
        let stale: Vec<ClientId> =
            self.clients.iter().filter(|(_, c)| c.last_seen.elapsed() > timeout).map(|(id, _)| *id).collect();
        for id in stale {
            self.remove(id, "timed out");
        }
    }
}

fn main() -> std::io::Result<()> {
    open_log();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let capture_path = args.iter().position(|a| a == "--capture").and_then(|i| args.get(i + 1)).cloned();
    let bind = args
        .iter()
        .enumerate()
        .find(|(i, a)| !a.starts_with("--") && (*i == 0 || args[i - 1] != "--capture"))
        .map(|(_, a)| a.clone())
        .unwrap_or_else(|| format!("{}:{DEFAULT_PORT}", Ipv4Addr::UNSPECIFIED));
    let socket = UdpSocket::bind(&bind)?;
    socket.set_read_timeout(Some(Duration::from_millis(250)))?;
    say!("IVMP server listening on {bind} (protocol {PROTOCOL_VERSION})");

    let capture = match &capture_path {
        Some(path) => {
            say!("capturing game packets to {path}");
            Some(BufWriter::new(File::create(path)?))
        }
        None => None,
    };
    let mut server = Server {
        socket,
        clients: BTreeMap::new(),
        host: None,
        capture,
        started: Instant::now(),
        entities: Registry::default(),
        last_summary: Instant::now(),
    };
    let mut buf = [0u8; 2048];
    loop {
        match server.socket.recv_from(&mut buf) {
            Ok((len, from)) => match ClientMessage::decode(&buf[..len]) {
                Ok(message) => server.handle(from, message),
                Err(e) => say!("bad packet from {from}: {e:?}"),
            },
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            // Windows reports ICMP "port unreachable" from an earlier send as a receive error.
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {}
            Err(e) => say!("receive failed: {e}"),
        }
        server.drop_timed_out();
        if server.last_summary.elapsed() > Duration::from_secs(10) && server.entities.packets > 0 {
            server.last_summary = Instant::now();
            say!("{}", server.entities.summary());
        }
    }
}
