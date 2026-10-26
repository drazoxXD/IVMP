//! The server loop: receives client messages, keeps the lobby, relays game packets and feeds
//! them to the entity registry (docs/onesync-plan.md).

use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use ivmp_host::session::HostSession;
use ivmp_scripting::Scripting;
use ivmp_sync::{events::EventLog, ids, nodes, registry::Registry};
use crate::resources;
use NetProtocol::reliable::Accept;
use NetProtocol::{ClientId, ClientMessage, MAX_SCRIPT_PAYLOAD, PROTOCOL_VERSION, RejectReason, ServerMessage, virtual_ip};

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
    /// The game events the clients send (read and logged only).
    events: EventLog,
    last_summary: Instant,
    /// Server-owned entities.
    world: World,
    last_world: Instant,
    console: Receiver<Result<Command, String>>,
    /// The game session host, when the server hosts (`--host`).
    host: Option<HostSession>,
    /// The resources (Lua scripts), docs/scripting-plan.md.
    scripting: Scripting,
    /// Script events each client triggered in the current second (a flood is dropped).
    script_rate: HashMap<ClientId, (Instant, u32)>,
    /// What clients download of each running resource that has client files.
    offers: HashMap<String, Vec<resources::FileEntry>>,
    /// Files on their way to each client.
    transfers: HashMap<ClientId, resources::Queue>,
    /// Where each player says they are (ClientMessage::Position).
    reported_positions: HashMap<ClientId, [f32; 3]>,
    /// What a script got from `CreateVehicle` (a token) -> the server entity it became.
    script_entities: HashMap<u32, u32>,
    /// What clients host and join (server.cfg `gamemode`, `slots`).
    gamemode: i32,
    slots: u8,
    /// Which content the clients load (server.cfg `dlc`).
    dlc: u8,
    /// Human-readable server name.
    name: Option<String>,
}

/// How many unacknowledged messages a client may have outstanding while files are sent to it.
const TRANSFER_WINDOW: usize = 24;

/// Script events one player may trigger per second.
const SCRIPT_EVENTS_PER_SECOND: u32 = 100;

impl Server {
    pub fn start(config: &Config) -> std::io::Result<Self> {
        let socket = UdpSocket::bind(&config.bind)?;
        // Short, so the session host's resends and acks go out on time.
        socket.set_read_timeout(Some(Duration::from_millis(20)))?;
        say!("IVMP server listening on {} (protocol {PROTOCOL_VERSION})", config.bind);
        if config.upnp {
            if let Ok(local) = socket.local_addr() {
                crate::upnp::open(local.port());
            }
        }
        let capture = match &config.capture {
            Some(path) => {
                say!("capturing game packets to {path}");
                Some(Capture::create(path)?)
            }
            None => None,
        };
        if let Some(ref name) = config.name {
            say!("server name: \"{name}\"");
        }
        if config.host {
            say!("the server hosts the game session (10.0.0.{HOST_ID}); every player joins it");
            if config.gamemode >= 0 && config.gamemode != 16 {
                say!("the server host only knows Free Mode sessions so far; gamemode {} is sent to clients anyway", config.gamemode);
            }
        } else {
            say!("the first player hosts the game session (peer to peer)");
        }
        say!(
            "game mode {}, {} slots, dlc {} ({})",
            config.gamemode,
            config.slots,
            config.dlc,
            ["GTA IV", "EFLC", "GTA IV + EFLC"][config.dlc as usize - 1]
        );
        say!("{}", console::HELP);
        let resources_dir = match &config.resources {
            Some(dir) => std::path::PathBuf::from(dir),
            None => std::env::current_exe().ok().and_then(|exe| exe.parent().map(|d| d.join("resources"))).unwrap_or_else(|| "resources".into()),
        };
        let scripting = Scripting::new(resources_dir.clone(), |line| say!("{line}"));
        for name in &config.start {
            if let Err(e) = scripting.start(name) {
                say!("could not start resource {name}: {e}");
            }
        }
        Ok(Server {
            scripting,
            script_rate: HashMap::new(),
            offers: HashMap::new(),
            transfers: HashMap::new(),
            script_entities: HashMap::new(),
            reported_positions: HashMap::new(),
            socket,
            lobby: Lobby { preset_ids: config.ids.clone(), ..Lobby::default() },
            capture,
            entities: Registry::default(),
            events: EventLog::default(),
            last_summary: Instant::now(),
            world: World::default(),
            last_world: Instant::now(),
            console: console::start(),
            host: config.host.then(HostSession::default),
            gamemode: config.gamemode,
            dlc: config.dlc,
            slots: config.slots,
            name: config.name.clone(),
        })
    }

    pub fn run(mut self) -> std::io::Result<()> {
        // The resources the config started have files for clients too.
        self.sync_resources(None);
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
            self.resend_reliable();
            self.scripting.tick();
            self.script_requests();
            self.pump_transfers();
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
                debug!("{}", self.entities.summary());
                if let Some(events) = self.events.summary() {
                    debug!("{events}");
                }
            }
        }
    }

    fn send(&self, to: SocketAddr, message: &ServerMessage) {
        if let Err(e) = self.socket.send_to(&message.encode(), to) {
            say!("send to {to} failed: {e}");
        }
    }

    /// Sends a control message that is resent until the client acknowledges it, and delivered once
    /// and in order (spawns, session restarts, object ids, ...). Not for game packets.
    fn reliable(&mut self, to: ClientId, message: &ServerMessage) {
        let Some(client) = self.lobby.clients.get_mut(&to) else { return };
        let body = message.encode();
        let bytes = client.out.queue(|seq| ServerMessage::Reliable { seq, body }.encode());
        let _ = self.socket.send_to(&bytes, client.addr);
    }

    fn reliable_broadcast(&mut self, except: Option<ClientId>, message: &ServerMessage) {
        let ids: Vec<ClientId> = self.lobby.clients.keys().copied().filter(|id| Some(*id) != except).collect();
        for id in ids {
            self.reliable(id, message);
        }
    }

    /// What scripts asked for: players dropped, events for clients (stage 2 delivers those).
    fn script_requests(&mut self) {
        let positions: HashMap<u32, [f32; 3]> =
            self.player_positions().into_iter().filter_map(|(id, p)| p.map(|p| (id as u32, p))).collect();
        self.scripting.set_positions(positions);
        for request in self.scripting.take_entity_requests() {
            match request {
                ivmp_scripting::EntityRequest::Create { token, ped, model, position, heading } => {
                    let kind = if ped { NetProtocol::SpawnKind::Ped } else { NetProtocol::SpawnKind::Vehicle };
                    let id = self.world.add(kind, world::model_hash(&model), position, heading);
                    self.script_entities.insert(token, id);
                    say!("script entity {id}: {kind:?} {model} at {:.1} {:.1} {:.1}", position[0], position[1], position[2]);
                    self.update_world();
                }
                ivmp_scripting::EntityRequest::Delete { token } => match self.script_entities.remove(&token) {
                    Some(id) => self.despawn_server_entity(id),
                    None => say!("DeleteEntity: no entity {token}"),
                },
            }
        }
        for (id, reason) in self.scripting.take_drops() {
            if let Ok(client) = ClientId::try_from(id) {
                if self.lobby.clients.contains_key(&client) {
                    self.remove(client, &format!("was dropped by a script: {reason}"));
                }
            }
        }
        for event in self.scripting.take_client_events() {
            let payload = ivmp_scripting::args_to_json(&event.args).into_bytes();
            if payload.len() > MAX_SCRIPT_PAYLOAD || event.name.len() > 255 {
                say!("TriggerClientEvent '{}': the event is too big ({} bytes, at most {MAX_SCRIPT_PAYLOAD})", event.name, payload.len());
                continue;
            }
            let message = ServerMessage::ScriptEvent { name: event.name.clone(), payload };
            if event.target < 0 {
                self.reliable_broadcast(None, &message);
            } else {
                match ClientId::try_from(event.target) {
                    Ok(id) if self.lobby.clients.contains_key(&id) => self.reliable(id, &message),
                    _ => say!("TriggerClientEvent '{}': there is no player {}", event.name, event.target),
                }
            }
        }
    }

    /// Deletes a server entity: its owner deletes it, which removes it for everyone.
    fn despawn_server_entity(&mut self, id: u32) {
        match self.world.remove(id) {
            Some(entity) => {
                // The owner deletes it, which removes it for everyone.
                let owner = entity.object_id.and_then(|object_id| {
                    let owner = self.entities.entities.get(&object_id).map(|e| e.owner);
                    owner.or(entity.creator.map(|(c, _)| c)).map(|owner| (owner, object_id))
                });
                match owner.filter(|(owner, _)| self.lobby.clients.contains_key(owner)) {
                    Some((owner, object_id)) => {
                        self.reliable(owner, &ServerMessage::Despawn { kind: entity.kind, object_id });
                        say!("server entity {id} deleted (asked client {owner})");
                    }
                    None => say!("server entity {id} forgotten (nobody has it)"),
                }
            }
            None => say!("no server entity {id}"),
        }
    }

    /// Tells every client what changed among the running resources: resources that stopped, and
    /// new ones (or `restarted`) with their files.
    fn sync_resources(&mut self, restarted: Option<&str>) {
        let running = self.scripting.running();
        let gone: Vec<String> = self.offers.keys().filter(|name| !running.contains(name)).cloned().collect();
        for name in gone {
            self.offers.remove(&name);
            self.reliable_broadcast(None, &ServerMessage::ResourceStop { resource: name });
        }
        for name in running {
            let is_new = !self.offers.contains_key(&name);
            let restart = restarted == Some(name.as_str());
            if !is_new && !restart {
                continue;
            }
            let Some(manifest) = self.scripting.manifest_of(&name) else { continue };
            match resources::offer(&self.scripting.resource_dir(&name), &manifest) {
                Ok(Some(files)) => {
                    if restart {
                        self.reliable_broadcast(None, &ServerMessage::ResourceStop { resource: name.clone() });
                    }
                    self.offers.insert(name.clone(), files);
                    self.announce(&name, None);
                }
                Ok(None) => {
                    if restart {
                        self.offers.remove(&name);
                        self.reliable_broadcast(None, &ServerMessage::ResourceStop { resource: name.clone() });
                    }
                }
                Err(e) => say!("resource {name}: nothing is sent to clients: {e}"),
            }
        }
    }

    /// Announces a resource's files to one client, or to everyone.
    fn announce(&mut self, name: &str, to: Option<ClientId>) {
        let Some(files) = self.offers.get(name).cloned() else { return };
        let mut messages: Vec<ServerMessage> = files
            .into_iter()
            .map(|f| ServerMessage::ResourceFile { resource: name.to_string(), path: f.path, size: f.size, hash: f.hash })
            .collect();
        messages.push(ServerMessage::ResourceReady { resource: name.to_string() });
        for message in messages {
            match to {
                Some(id) => self.reliable(id, &message),
                None => self.reliable_broadcast(None, &message),
            }
        }
    }

    /// Everything running that has client files, for a client that just joined.
    fn announce_all(&mut self, id: ClientId) {
        let names: Vec<String> = self.offers.keys().cloned().collect();
        for name in names {
            self.announce(&name, Some(id));
        }
    }

    /// A client asks for a file it was told about.
    fn file_request(&mut self, id: ClientId, resource: String, path: String) {
        let Some(entry) = self.offers.get(&resource).and_then(|files| files.iter().find(|f| f.path == path)).cloned() else {
            say!("player {id} asked for {resource}/{path}, which was not offered");
            return;
        };
        let file = self.scripting.resource_dir(&resource).join(&path);
        match std::fs::read(&file) {
            Ok(data) if NetProtocol::file_hash(&data) == entry.hash => {
                let queue = self.transfers.entry(id).or_default();
                // The same file asked for twice is sent once.
                if !queue.iter().any(|t| t.resource == resource && t.path == path) {
                    queue.push_back(resources::Transfer::new(resource, path, data));
                }
            }
            Ok(_) => say!("{resource}/{path} changed on disk after it was announced; restart the resource to send the new version"),
            Err(e) => say!("cannot read {resource}/{path}: {e}"),
        }
    }

    /// Sends file chunks, as many as each client's reliable window has room for.
    fn pump_transfers(&mut self) {
        let ids: Vec<ClientId> = self.transfers.keys().copied().collect();
        for id in ids {
            loop {
                let Some(client) = self.lobby.clients.get(&id) else {
                    self.transfers.remove(&id);
                    break;
                };
                if client.out.pending() >= TRANSFER_WINDOW {
                    break;
                }
                let Some(queue) = self.transfers.get_mut(&id) else { break };
                let Some(transfer) = queue.front_mut() else {
                    self.transfers.remove(&id);
                    break;
                };
                let (resource, path) = (transfer.resource.clone(), transfer.path.clone());
                match transfer.next_chunk() {
                    Some((offset, data)) => self.reliable(id, &ServerMessage::FileChunk { resource, path, offset, data }),
                    None => {
                        queue.pop_front();
                    }
                }
            }
        }
    }

    /// `TriggerServerEvent` from a client: the handlers a resource registered with RegisterNetEvent.
    fn script_event(&mut self, id: ClientId, name: &str, payload: &[u8]) {
        let now = Instant::now();
        let (since, count) = self.script_rate.entry(id).or_insert((now, 0));
        if now.duration_since(*since) >= Duration::from_secs(1) {
            (*since, *count) = (now, 0);
        }
        *count += 1;
        if *count > SCRIPT_EVENTS_PER_SECOND {
            if *count == SCRIPT_EVENTS_PER_SECOND + 1 {
                say!("player {id} sends too many script events, dropping them");
            }
            return;
        }
        if payload.len() > MAX_SCRIPT_PAYLOAD {
            say!("player {id}: script event '{name}' is too big ({} bytes)", payload.len());
            return;
        }
        if let Err(e) = self.scripting.net_event_json(id as u32, name, payload) {
            say!("player {id}: bad script event '{name}': {e}");
        }
    }

    /// Sends reliable messages again that were not acknowledged in time.
    fn resend_reliable(&mut self) {
        let now = Instant::now();
        for (id, client) in self.lobby.clients.iter_mut() {
            let (packets, dropped) = client.out.due(now);
            for packet in packets {
                let _ = self.socket.send_to(&packet, client.addr);
            }
            if dropped > 0 {
                say!("gave up on {dropped} control message(s) to client {id}: no acknowledgement");
            }
        }
    }

    fn broadcast(&self, except: Option<ClientId>, message: &ServerMessage) {
        let bytes = message.encode();
        for (id, client) in &self.lobby.clients {
            if Some(*id) != except {
                let _ = self.socket.send_to(&bytes, client.addr);
                if matches!(message, ServerMessage::Tunnel { .. }) {
                    client.down.set(client.down.get().wrapping_add(1));
                }
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
            (ClientMessage::ScriptEvent { name, payload }, Some(id)) => self.script_event(id, &name, &payload),
            (ClientMessage::FileRequest { resource, path }, Some(id)) => self.file_request(id, resource, path),
            (ClientMessage::Tunnel { from_port, to, to_port, payload }, Some(id)) => {
                if let Some(client) = self.lobby.clients.get(&id) {
                    client.up.set(client.up.get().wrapping_add(1));
                }
                let target = relay::target(to);
                if let Some(capture) = self.capture.as_mut() {
                    capture.record(id, if let Target::Client(to) = target { to } else { 0 }, &payload);
                }
                for line in self.entities.observe(id, &payload) {
                    debug!("{line}");
                }
                if crate::log::verbose() {
                    for line in self.events.observe(id, &payload) {
                        say!("{line}");
                    }
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
                            client.down.set(client.down.get().wrapping_add(1));
                        }
                    }
                    Target::Nobody => {}
                }
            }
            (ClientMessage::Spawned { request, object_id }, Some(id)) => {
                if self.world.spawned(request, id, object_id).is_some() {
                    match object_id {
                        0 => say!("client {id} could not create server entity {request}"),
                        _ => debug!("server entity {request} is object {object_id} (created by client {id})"),
                    }
                }
            }
            // The session host's game ends its session for a new one (lobby game type change):
            // tell the others now, or they only notice when the old session times out (~30 s).
            (ClientMessage::SessionRestart, Some(id)) => {
                if self.lobby.host == Some(id) {
                    debug!("client {id} (the host) is starting a new session; telling the others");
                    self.reliable_broadcast(Some(id), &ServerMessage::SessionRestart { host: id });
                }
            }
            // The host's new session is in its lobby: the others may join it now.
            (ClientMessage::SessionReady, Some(id)) => {
                if self.lobby.host == Some(id) {
                    debug!("client {id} (the host) has its new session ready; telling the others");
                    self.reliable_broadcast(Some(id), &ServerMessage::SessionReady { host: id });
                }
            }
            (ClientMessage::KeepAlive, Some(_)) => {}
            (ClientMessage::Position { position }, Some(id)) => {
                if position.iter().all(|v| v.is_finite()) {
                    self.reported_positions.insert(id, position);
                }
            }
            (ClientMessage::Ping { stamp }, Some(id)) => {
                let (up, down) = self.lobby.clients.get(&id).map(|c| (c.up.get(), c.down.get())).unwrap_or((0, 0));
                self.send(from, &ServerMessage::Pong { stamp, up, down });
            }
            // A control message from the client: acknowledge it, and process it once.
            (ClientMessage::Reliable { seq, body }, Some(id)) => {
                let accepted = self.lobby.clients.get_mut(&id).map(|c| c.inn.accept(seq));
                match accepted {
                    Some(Accept::Deliver { ack }) => {
                        self.send(from, &ServerMessage::Ack { seq: ack });
                        match ClientMessage::decode(&body) {
                            Ok(inner) => self.handle(from, inner),
                            Err(e) => say!("bad reliable message from {from}: {e:?}"),
                        }
                    }
                    Some(Accept::Duplicate { ack }) => self.send(from, &ServerMessage::Ack { seq: ack }),
                    Some(Accept::Ahead) | None => {}
                }
            }
            (ClientMessage::Ack { seq }, Some(id)) => {
                if let Some(client) = self.lobby.clients.get_mut(&id) {
                    client.out.ack(seq);
                }
            }
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
        if let Some(client) = self.lobby.clients.get(&client) {
            client.down.set(client.down.get().wrapping_add(1));
        }
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
            None if self.scripting.player_connecting(nickname).is_some_and(|reason| {
                say!("{nickname} ({from}) was refused by a script: {reason}");
                true
            }) =>
            {
                return self.send(from, &ServerMessage::Reject(RejectReason::Refused));
            }
            None => match self.lobby.join(from, nickname) {
                Ok(id) => {
                    say!("{nickname} joined from {from} as {} (id {id}), object ids {:?}", virtual_ip(id), ids::block(id));
                    self.scripting.player_joined(id as u32, nickname);
                    self.reliable_broadcast(Some(id), &ServerMessage::PeerJoined { id, nickname: nickname.to_string() });
                    self.announce_all(id);
                    let others: Vec<(ClientId, String)> =
                        self.lobby.clients.iter().filter(|(other, _)| **other != id).map(|(other, c)| (*other, c.nickname.clone())).collect();
                    for (other, other_nickname) in others {
                        self.reliable(id, &ServerMessage::PeerJoined { id: other, nickname: other_nickname });
                    }
                    id
                }
                Err(reason) => return self.send(from, &ServerMessage::Reject(reason)),
            },
        };
        let host = if self.host.is_some() { HOST_ID } else { self.lobby.host.unwrap_or(id) };
        self.send(from, &ServerMessage::Welcome { id, host });
        // Only on the first hello: a repeated one (the welcome was lost) is answered with the
        // welcome alone, the reliable messages are already on their way.
        if existing.is_none() {
            let (first, count) = ids::block(id);
            self.reliable(id, &ServerMessage::ObjectIds { first, count });
            self.reliable(id, &ServerMessage::GameSettings { mode: self.gamemode, slots: self.slots, dlc: self.dlc });
            if let Some(ref name) = self.name {
                let payload = serde_json::to_vec(name).unwrap_or_else(|_| name.as_bytes().to_vec());
                self.reliable(id, &ServerMessage::ScriptEvent { name: "ivmp:server_name".into(), payload });
            }
        }
    }

    /// Each client with its player's position, when the registry knows it.
    fn player_positions(&self) -> Vec<(ClientId, Option<[f32; 3]>)> {
        self.lobby
            .clients
            .keys()
            .map(|id| {
                let position = self.reported_positions.get(id).copied().or_else(|| {
                    self.entities.entities.values().find(|e| e.object_type == nodes::PLAYER && e.owner == *id).and_then(|e| e.position)
                });
                (*id, position)
            })
            .collect()
    }

    fn update_world(&mut self) {
        let clients = self.player_positions();
        for order in self.world.orders(&self.entities, &clients) {
            if !self.lobby.clients.contains_key(&order.client) {
                continue;
            }
            let e = &order.entity;
            debug!("asking client {} to create server entity {} ({:?} {:#010x})", order.client, order.request, e.kind, e.model);
            self.reliable(
                order.client,
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
            Command::Despawn(id) => self.despawn_server_entity(id),
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
            Command::Resources => {
                let running = self.scripting.running();
                for name in self.scripting.available() {
                    say!("{name}: {}", if running.contains(&name) { "started" } else { "stopped" });
                }
                for name in running {
                    if !self.scripting.available().contains(&name) {
                        say!("{name}: started (its folder is gone)");
                    }
                }
            }
            Command::Resource { action, name } => {
                let result = match action.as_str() {
                    "start" => self.scripting.start(&name),
                    "stop" => self.scripting.stop(&name),
                    "restart" => self.scripting.restart(&name),
                    _ => self.scripting.ensure(&name),
                };
                if let Err(e) = result {
                    say!("{action} {name}: {e}");
                }
                self.sync_resources(if action == "restart" { Some(name.as_str()) } else { None });
            }
            Command::Other(line) => {
                if !self.scripting.run_command(0, &line) {
                    say!("unknown command; {}", console::HELP);
                }
            }
        }
    }

    fn remove(&mut self, id: ClientId, why: &str) {
        self.script_rate.remove(&id);
        self.reported_positions.remove(&id);
        self.transfers.remove(&id);
        if let Some(host) = self.host.as_mut() {
            host.remove(id);
        }
        let Some((client, new_host)) = self.lobby.leave(id) else { return };
        say!("{} ({}) {why}", client.nickname, virtual_ip(id));
        self.scripting.player_dropped(id as u32, why);
        self.reliable_broadcast(None, &ServerMessage::PeerLeft { id });
        // With the server hosting, the session host never changes.
        if let Some(host) = new_host.filter(|_| self.host.is_none()) {
            say!("host is now {}", self.lobby.clients[&host].nickname);
            self.reliable_broadcast(None, &ServerMessage::HostChanged { host });
        }
    }
}
