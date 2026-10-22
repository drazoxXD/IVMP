//! The server loop: receives client messages, keeps the lobby, relays game packets and feeds
//! them to the entity registry (docs/onesync-plan.md).

use std::net::{SocketAddr, UdpSocket};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use ivmp_host::session::HostSession;
use ivmp_sync::{ids, nodes, registry::Registry};
use NetProtocol::{ClientId, ClientMessage, PROTOCOL_VERSION, RejectReason, ServerMessage, virtual_ip};

use crate::capture::Capture;
use crate::config::Config;
use crate::console::{self, Command};
use crate::lobby::Lobby;
use crate::relay::{self, Target};
use crate::world::{self, World};

/// How often the entity summary is logged.
const SUMMARY_EVERY: Duration = Duration::from_secs(10);
/// How often server entities are checked (spawned, re-created).
const WORLD_EVERY: Duration = Duration::from_secs(1);
/// Where `spawn` without a position puts things, from the first player.
const SPAWN_OFFSET: [f32; 3] = [4.0, 4.0, 0.5];
/// The client id (virtual address 10.0.0.254) the server hosts the game session as.
pub const HOST_ID: ClientId = 254;
/// The engine's game port.
const GAME_PORT: u16 = 6672;

pub struct Server {
    socket: UdpSocket,
    lobby: Lobby,
    capture: Option<Capture>,
    /// Every entity seen in the clients' game traffic.
    entities: Registry,
    last_summary: Instant,
    /// Server-owned entities.
    world: World,
    last_world: Instant,
    console: Receiver<Result<Command, String>>,
    /// The game session host, when the server hosts (`--host`).
    host: Option<HostSession>,
    /// What clients host and join (server.cfg `gamemode`, `slots`).
    gamemode: i32,
    slots: u8,
}

impl Server {
    pub fn start(config: &Config) -> std::io::Result<Self> {
        let socket = UdpSocket::bind(&config.bind)?;
        // Short, so the session host's resends and acks go out on time.
        socket.set_read_timeout(Some(Duration::from_millis(20)))?;
        say!("IVMP server listening on {} (protocol {PROTOCOL_VERSION})", config.bind);
        let capture = match &config.capture {
            Some(path) => {
                say!("capturing game packets to {path}");
                Some(Capture::create(path)?)
            }
            None => None,
        };
        if config.host {
            say!("the server hosts the game session (10.0.0.{HOST_ID}); every player joins it");
            if config.gamemode >= 0 && config.gamemode != 16 {
                say!("the server host only knows Free Mode sessions so far; gamemode {} is sent to clients anyway", config.gamemode);
            }
        } else {
            say!("the first player hosts the game session (peer to peer)");
        }
        say!("game mode {}, {} slots", config.gamemode, config.slots);
        say!("{}", console::HELP);
        Ok(Server {
            socket,
            lobby: Lobby { preset_ids: config.ids.clone(), ..Lobby::default() },
            capture,
            entities: Registry::default(),
            last_summary: Instant::now(),
            world: World::default(),
            last_world: Instant::now(),
            console: console::start(),
            host: config.host.then(HostSession::default),
            gamemode: config.gamemode,
            slots: config.slots,
        })
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
            self.host_tick();
            for id in self.lobby.timed_out() {
                self.remove(id, "timed out");
            }
            while let Ok(command) = self.console.try_recv() {
                match command {
                    Ok(command) => self.command(command),
                    Err(e) => say!("{e}"),
                }
            }
            if self.last_world.elapsed() > WORLD_EVERY {
                self.last_world = Instant::now();
                self.update_world();
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
                if to == virtual_ip(HOST_ID) && self.host.is_some() {
                    return self.to_host(id, from_port, &payload);
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
            (ClientMessage::Spawned { request, object_id }, Some(id)) => {
                if self.world.spawned(request, id, object_id).is_some() {
                    match object_id {
                        0 => say!("client {id} could not create server entity {request}"),
                        _ => say!("server entity {request} is object {object_id} (created by client {id})"),
                    }
                }
            }
            // The session host's game ends its session for a new one (lobby game type change):
            // tell the others now, or they only notice when the old session times out (~30 s).
            (ClientMessage::SessionRestart, Some(id)) => {
                if self.lobby.host == Some(id) {
                    say!("client {id} (the host) is starting a new session; telling the others");
                    self.broadcast(Some(id), &ServerMessage::SessionRestart { host: id });
                }
            }
            // The host's new session is in its lobby: the others may join it now.
            (ClientMessage::SessionReady, Some(id)) => {
                if self.lobby.host == Some(id) {
                    say!("client {id} (the host) has its new session ready; telling the others");
                    self.broadcast(Some(id), &ServerMessage::SessionReady { host: id });
                }
            }
            (ClientMessage::KeepAlive, Some(_)) => {}
            (ClientMessage::Ping { stamp }, Some(_)) => self.send(from, &ServerMessage::Pong { stamp }),
            (ClientMessage::Bye, Some(id)) => self.remove(id, "left"),
            // Anything but hello from an unknown address is ignored.
            (_, None) => {}
        }
    }

    /// A game packet for the session host.
    fn to_host(&mut self, client: ClientId, client_port: u16, payload: &[u8]) {
        let Some(host) = self.host.as_mut() else { return };
        let answers = host.receive(client, payload);
        for line in host.log.drain(..) {
            say!("{line}");
        }
        for answer in answers {
            self.from_host(client, client_port, answer);
        }
    }

    /// A packet from the session host to `client`.
    fn from_host(&mut self, client: ClientId, client_port: u16, payload: Vec<u8>) {
        let Some(addr) = self.lobby.clients.get(&client).map(|c| c.addr) else { return };
        if let Some(capture) = self.capture.as_mut() {
            capture.record(HOST_ID, client, &payload);
        }
        self.send(addr, &ServerMessage::Tunnel { from: virtual_ip(HOST_ID), from_port: GAME_PORT, to_port: client_port, payload });
    }

    fn host_tick(&mut self) {
        let Some(host) = self.host.as_mut() else { return };
        for (client, packet) in host.tick() {
            self.from_host(client, GAME_PORT, packet);
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
        let host = if self.host.is_some() { HOST_ID } else { self.lobby.host.unwrap_or(id) };
        self.send(from, &ServerMessage::Welcome { id, host });
        let (first, count) = ids::block(id);
        self.send(from, &ServerMessage::ObjectIds { first, count });
        self.send(from, &ServerMessage::GameSettings { mode: self.gamemode, slots: self.slots });
    }

    /// Each client with its player's position, when the registry knows it.
    fn player_positions(&self) -> Vec<(ClientId, Option<[f32; 3]>)> {
        self.lobby
            .clients
            .keys()
            .map(|id| {
                let position = self
                    .entities
                    .entities
                    .values()
                    .find(|e| e.object_type == nodes::PLAYER && e.owner == *id)
                    .and_then(|e| e.position);
                (*id, position)
            })
            .collect()
    }

    fn update_world(&mut self) {
        let clients = self.player_positions();
        for order in self.world.orders(&self.entities, &clients) {
            let Some(client) = self.lobby.clients.get(&order.client) else { continue };
            let e = &order.entity;
            say!("asking client {} to create server entity {} ({:?} {:#010x})", order.client, order.request, e.kind, e.model);
            self.send(
                client.addr,
                &ServerMessage::Spawn {
                    request: order.request,
                    kind: e.kind,
                    model: e.model,
                    position: e.position,
                    heading: e.heading,
                },
            );
        }
    }

    fn command(&mut self, command: Command) {
        match command {
            Command::Spawn { kind, model, position, heading } => {
                let position = match position {
                    Some(position) => position,
                    None => match self.player_positions().into_iter().find_map(|(_, p)| p) {
                        Some(p) => [p[0] + SPAWN_OFFSET[0], p[1] + SPAWN_OFFSET[1], p[2] + SPAWN_OFFSET[2]],
                        None => return say!("no player position known yet; give x y z"),
                    },
                };
                let id = self.world.add(kind, world::model_hash(&model), position, heading);
                say!("server entity {id}: {kind:?} {model} at {:.1} {:.1} {:.1}", position[0], position[1], position[2]);
                self.update_world();
            }
            Command::Despawn(id) => match self.world.remove(id) {
                Some(entity) => {
                    // The owner deletes it, which removes it for everyone.
                    let owner = entity.object_id.and_then(|object_id| {
                        let owner = self.entities.entities.get(&object_id).map(|e| e.owner);
                        owner.or(entity.creator.map(|(c, _)| c)).map(|owner| (owner, object_id))
                    });
                    match owner.and_then(|(owner, object_id)| Some((self.lobby.clients.get(&owner)?.addr, owner, object_id))) {
                        Some((addr, owner, object_id)) => {
                            self.send(addr, &ServerMessage::Despawn { kind: entity.kind, object_id });
                            say!("server entity {id} deleted (asked client {owner})");
                        }
                        None => say!("server entity {id} forgotten (nobody has it)"),
                    }
                }
                None => say!("no server entity {id}"),
            },
            Command::Entities => {
                if self.world.entities.is_empty() {
                    say!("no server entities");
                }
                for (id, e) in &self.world.entities {
                    say!(
                        "server entity {id}: {:?} {:#010x} at {:.1} {:.1} {:.1}, object {:?}",
                        e.kind,
                        e.model,
                        e.position[0],
                        e.position[1],
                        e.position[2],
                        e.object_id
                    );
                }
            }
            Command::Help => say!("{}", console::HELP),
        }
    }

    fn remove(&mut self, id: ClientId, why: &str) {
        if let Some(host) = self.host.as_mut() {
            host.remove(id);
        }
        let Some((client, new_host)) = self.lobby.leave(id) else { return };
        say!("{} ({}) {why}", client.nickname, virtual_ip(id));
        self.broadcast(None, &ServerMessage::PeerLeft { id });
        // With the server hosting, the session host never changes.
        if let Some(host) = new_host.filter(|_| self.host.is_none()) {
            say!("host is now {}", self.lobby.clients[&host].nickname);
            self.broadcast(None, &ServerMessage::HostChanged { host });
        }
    }
}
