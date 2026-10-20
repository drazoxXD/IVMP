//! IVMP server: lobby + relay.
//!
//! Usage: `ivmp_server [bind address]` (default `0.0.0.0:9999`).
//!
//! Clients say hello, get a virtual address (10.0.0.<id>) and the id of the client that hosts the
//! engine session. After that the server forwards game packets between clients by virtual address.
//! The first client to join hosts; when the host leaves, the oldest remaining client takes over.

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use NetProtocol::{
    BROADCAST, ClientId, ClientMessage, DEFAULT_PORT, PROTOCOL_VERSION, RejectReason, ServerMessage, TIMEOUT_SECS,
    client_id_of, virtual_ip,
};

const MAX_CLIENTS: usize = 32;

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
}

impl Server {
    fn send(&self, to: SocketAddr, message: &ServerMessage) {
        if let Err(e) = self.socket.send_to(&message.encode(), to) {
            eprintln!("send to {to} failed: {e}");
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
                        println!("{nickname} joined from {from} as {} (id {id})", virtual_ip(id));
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
            }
            (ClientMessage::Tunnel { from_port, to, to_port, payload }, Some(id)) => {
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
        println!("{} ({}) {why}", client.nickname, virtual_ip(id));
        self.broadcast(None, &ServerMessage::PeerLeft { id });
        if self.host == Some(id) {
            self.host = self.clients.keys().next().copied();
            if let Some(host) = self.host {
                println!("host is now {}", self.clients[&host].nickname);
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
    let bind = std::env::args().nth(1).unwrap_or_else(|| format!("{}:{DEFAULT_PORT}", Ipv4Addr::UNSPECIFIED));
    let socket = UdpSocket::bind(&bind)?;
    socket.set_read_timeout(Some(Duration::from_millis(250)))?;
    println!("IVMP server listening on {bind} (protocol {PROTOCOL_VERSION})");

    let mut server = Server { socket, clients: BTreeMap::new(), host: None };
    let mut buf = [0u8; 2048];
    loop {
        match server.socket.recv_from(&mut buf) {
            Ok((len, from)) => match ClientMessage::decode(&buf[..len]) {
                Ok(message) => server.handle(from, message),
                Err(e) => eprintln!("bad packet from {from}: {e:?}"),
            },
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            // Windows reports ICMP "port unreachable" from an earlier send as a receive error.
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {}
            Err(e) => eprintln!("receive failed: {e}"),
        }
        server.drop_timed_out();
    }
}
