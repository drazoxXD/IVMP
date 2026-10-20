//! The server loop: receives client messages, keeps the lobby, relays game packets and feeds
//! them to the entity registry (docs/onesync-plan.md).

use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use ivmp_sync::{ids, registry::Registry};
use NetProtocol::{ClientId, ClientMessage, PROTOCOL_VERSION, RejectReason, ServerMessage, virtual_ip};

use crate::capture::Capture;
use crate::config::Config;
use crate::lobby::Lobby;
use crate::relay::{self, Target};

/// How often the entity summary is logged.
const SUMMARY_EVERY: Duration = Duration::from_secs(10);

pub struct Server {
    socket: UdpSocket,
    lobby: Lobby,
    capture: Option<Capture>,
    /// Every entity seen in the clients' game traffic.
    entities: Registry,
    last_summary: Instant,
}

impl Server {
    pub fn start(config: &Config) -> std::io::Result<Self> {
        let socket = UdpSocket::bind(&config.bind)?;
        socket.set_read_timeout(Some(Duration::from_millis(250)))?;
        say!("IVMP server listening on {} (protocol {PROTOCOL_VERSION})", config.bind);
        let capture = match &config.capture {
            Some(path) => {
                say!("capturing game packets to {path}");
                Some(Capture::create(path)?)
            }
            None => None,
        };
        Ok(Server { socket, lobby: Lobby::default(), capture, entities: Registry::default(), last_summary: Instant::now() })
    }

    pub fn run(mut self) -> std::io::Result<()> {
        let mut buf = [0u8; 2048];
        loop {
            match self.socket.recv_from(&mut buf) {
                Ok((len, from)) => match ClientMessage::decode(&buf[..len]) {
                    Ok(message) => self.handle(from, message),
                    Err(e) => say!("bad packet from {from}: {e:?}"),
                },
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
                // Windows reports ICMP "port unreachable" from an earlier send as a receive error.
                Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {}
                Err(e) => say!("receive failed: {e}"),
            }
            for id in self.lobby.timed_out() {
                self.remove(id, "timed out");
            }
            if self.last_summary.elapsed() > SUMMARY_EVERY && self.entities.packets > 0 {
                self.last_summary = Instant::now();
                say!("{}", self.entities.summary());
            }
        }
    }

    fn send(&self, to: SocketAddr, message: &ServerMessage) {
        if let Err(e) = self.socket.send_to(&message.encode(), to) {
            say!("send to {to} failed: {e}");
        }
    }

    fn broadcast(&self, except: Option<ClientId>, message: &ServerMessage) {
        let bytes = message.encode();
        for (id, client) in &self.lobby.clients {
            if Some(*id) != except {
                let _ = self.socket.send_to(&bytes, client.addr);
            }
        }
    }

    fn handle(&mut self, from: SocketAddr, message: ClientMessage) {
        let sender = self.lobby.id_of(from);
        if let Some(id) = sender {
            self.lobby.touch(id);
        }
        match (message, sender) {
            (ClientMessage::Hello { version, nickname }, existing) => self.hello(from, version, &nickname, existing),
            (ClientMessage::Tunnel { from_port, to, to_port, payload }, Some(id)) => {
                let target = relay::target(to);
                if let Some(capture) = self.capture.as_mut() {
                    capture.record(id, if let Target::Client(to) = target { to } else { 0 }, &payload);
                }
                for line in self.entities.observe(id, &payload) {
                    say!("{line}");
                }
                let packet = ServerMessage::Tunnel { from: virtual_ip(id), from_port, to_port, payload };
                match target {
                    Target::Everyone => self.broadcast(Some(id), &packet),
                    Target::Client(to) => {
                        if let Some(client) = self.lobby.clients.get(&to) {
                            self.send(client.addr, &packet);
                        }
                    }
                    Target::Nobody => {}
                }
            }
            (ClientMessage::KeepAlive, Some(_)) => {}
            (ClientMessage::Bye, Some(id)) => self.remove(id, "left"),
            // Anything but hello from an unknown address is ignored.
            (_, None) => {}
        }
    }

    fn hello(&mut self, from: SocketAddr, version: u16, nickname: &str, existing: Option<ClientId>) {
        if version != PROTOCOL_VERSION {
            return self.send(from, &ServerMessage::Reject(RejectReason::VersionMismatch));
        }
        let nickname = nickname.trim();
        // A repeated hello (lost welcome) just gets the welcome again.
        let id = match existing {
            Some(id) => id,
            None => match self.lobby.join(from, nickname) {
                Ok(id) => {
                    say!("{nickname} joined from {from} as {} (id {id}), object ids {:?}", virtual_ip(id), ids::block(id));
                    self.broadcast(Some(id), &ServerMessage::PeerJoined { id, nickname: nickname.to_string() });
                    for (other, client) in &self.lobby.clients {
                        if *other != id {
                            self.send(from, &ServerMessage::PeerJoined { id: *other, nickname: client.nickname.clone() });
                        }
                    }
                    id
                }
                Err(reason) => return self.send(from, &ServerMessage::Reject(reason)),
            },
        };
        let host = self.lobby.host.unwrap_or(id);
        self.send(from, &ServerMessage::Welcome { id, host });
        let (first, count) = ids::block(id);
        self.send(from, &ServerMessage::ObjectIds { first, count });
    }

    fn remove(&mut self, id: ClientId, why: &str) {
        let Some((client, new_host)) = self.lobby.leave(id) else { return };
        say!("{} ({}) {why}", client.nickname, virtual_ip(id));
        self.broadcast(None, &ServerMessage::PeerLeft { id });
        if let Some(host) = new_host {
            say!("host is now {}", self.lobby.clients[&host].nickname);
            self.broadcast(None, &ServerMessage::HostChanged { host });
        }
    }
}
