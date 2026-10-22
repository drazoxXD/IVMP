//! IVMP wire protocol between the client DLL and the server (UDP).
//!
//! The server is a lobby and a relay: it gives every client a virtual IPv4 address and forwards the
//! game engine's own UDP packets (rage netSocket) between clients by those addresses. The engine
//! never sees real addresses, so it treats every player as a LAN peer.
//!
//! Every datagram starts with [`MAGIC`] and a one-byte message tag; integers are little-endian.

use std::net::Ipv4Addr;

pub const MAGIC: [u8; 2] = *b"IV";
/// Bump when the wire format changes; the server rejects other versions.
pub const PROTOCOL_VERSION: u16 = 8;
pub const DEFAULT_PORT: u16 = 9999;
/// The engine reads at most 0x400 bytes per packet.
pub const MAX_GAME_PAYLOAD: usize = 0x400;
/// Virtual addresses are `10.0.0.<client id>`.
pub const VIRTUAL_NETWORK: [u8; 3] = [10, 0, 0];
/// Clients send a keep-alive at least this often; the server drops them after [`TIMEOUT_SECS`].
pub const KEEPALIVE_SECS: u64 = 2;
pub const TIMEOUT_SECS: u64 = 10;
/// Destination meaning "everyone else" (the engine's LAN broadcast).
pub const BROADCAST: Ipv4Addr = Ipv4Addr::BROADCAST;

pub type ClientId = u8;

pub fn virtual_ip(id: ClientId) -> Ipv4Addr {
    Ipv4Addr::new(VIRTUAL_NETWORK[0], VIRTUAL_NETWORK[1], VIRTUAL_NETWORK[2], id)
}

pub fn client_id_of(ip: Ipv4Addr) -> Option<ClientId> {
    let o = ip.octets();
    (o[..3] == VIRTUAL_NETWORK && o[3] != 0 && o[3] != 255).then_some(o[3])
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientMessage {
    Hello { version: u16, nickname: String },
    /// A game packet from the local engine (`from_port` = its socket port) to a virtual address.
    Tunnel { from_port: u16, to: Ipv4Addr, to_port: u16, payload: Vec<u8> },
    KeepAlive,
    /// Round trip measurement; the server answers with [`ServerMessage::Pong`] and the same stamp.
    Ping { stamp: u32 },
    Bye,
    /// Answer to [`ServerMessage::Spawn`]: the network object id of the created entity, or 0 if
    /// it couldn't be created.
    Spawned { request: u32, object_id: u16 },
    /// The session host's game is ending its session to start a new one (`gamemode = -1`:
    /// the game type was changed in the lobby).
    SessionRestart,
    /// The session host's new session (after [`ClientMessage::SessionRestart`]) is up and in its
    /// lobby: the others can join it now.
    SessionReady,
}

/// What a [`ServerMessage::Spawn`] creates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnKind {
    Vehicle,
    Ped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RejectReason {
    VersionMismatch,
    ServerFull,
    BadNickname,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ServerMessage {
    /// Accepted: your virtual address, and who hosts the engine session.
    Welcome { id: ClientId, host: ClientId },
    Reject(RejectReason),
    PeerJoined { id: ClientId, nickname: String },
    PeerLeft { id: ClientId },
    HostChanged { host: ClientId },
    /// The network object ids this client's engine may use: `first..first + count`. The server
    /// gives every client its own block, so entities never collide (docs/onesync-plan.md).
    ObjectIds { first: u16, count: u16 },
    /// Create a server-owned entity: `model` is the model name hash, `heading` in degrees. The
    /// client answers with [`ClientMessage::Spawned`].
    Spawn { request: u32, kind: SpawnKind, model: u32, position: [f32; 3], heading: f32 },
    /// The game session's settings from server.cfg: the network game mode clients host and join
    /// (16 = Free Mode; negative = IVMP's own modes) and its player slots.
    GameSettings { mode: i32, slots: u8 },
    /// Delete a server-owned entity; sent to the client that owns it.
    Despawn { kind: SpawnKind, object_id: u16 },
    /// A game packet from another client's engine, to hand to the local engine.
    Tunnel { from: Ipv4Addr, from_port: u16, to_port: u16, payload: Vec<u8> },
    /// The session host (`host`) is starting a new session: leave the old one and join the new.
    SessionRestart { host: ClientId },
    /// The session host's new session is in its lobby: join it now.
    SessionReady { host: ClientId },
    /// Answer to [`ClientMessage::Ping`].
    Pong { stamp: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    BadMagic,
    UnknownTag(u8),
    Truncated,
    BadString,
}

// Tags. Client -> server 0x0_, server -> client 0x8_.
const C_HELLO: u8 = 0x01;
const C_TUNNEL: u8 = 0x02;
const C_KEEPALIVE: u8 = 0x03;
const C_PING: u8 = 0x0A;
const C_BYE: u8 = 0x04;
const C_SPAWNED: u8 = 0x05;
const C_SESSION_RESTART: u8 = 0x06;
const C_SESSION_READY: u8 = 0x07;
const S_WELCOME: u8 = 0x81;
const S_REJECT: u8 = 0x82;
const S_PEER_JOINED: u8 = 0x83;
const S_PEER_LEFT: u8 = 0x84;
const S_HOST_CHANGED: u8 = 0x85;
const S_TUNNEL: u8 = 0x86;
const S_OBJECT_IDS: u8 = 0x87;
const S_SPAWN: u8 = 0x88;
const S_DESPAWN: u8 = 0x89;
const S_GAME_SETTINGS: u8 = 0x8A;
const S_SESSION_RESTART: u8 = 0x8B;
const S_SESSION_READY: u8 = 0x8C;
const S_PONG: u8 = 0x8D;

fn kind_byte(kind: SpawnKind) -> u8 {
    match kind {
        SpawnKind::Vehicle => 0,
        SpawnKind::Ped => 1,
    }
}

fn byte_kind(byte: u8) -> SpawnKind {
    if byte == 0 { SpawnKind::Vehicle } else { SpawnKind::Ped }
}

struct Writer(Vec<u8>);

impl Writer {
    fn new(tag: u8) -> Self {
        let mut v = Vec::with_capacity(16);
        v.extend_from_slice(&MAGIC);
        v.push(tag);
        Writer(v)
    }
    fn u8(mut self, v: u8) -> Self {
        self.0.push(v);
        self
    }
    fn u16(mut self, v: u16) -> Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn u32(mut self, v: u32) -> Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn f32(self, v: f32) -> Self {
        self.u32(v.to_bits())
    }
    fn ip(mut self, v: Ipv4Addr) -> Self {
        self.0.extend_from_slice(&v.octets());
        self
    }
    fn str(self, s: &str) -> Self {
        let bytes = &s.as_bytes()[..s.len().min(u8::MAX as usize)];
        let mut w = self.u8(bytes.len() as u8);
        w.0.extend_from_slice(bytes);
        w
    }
    fn rest(mut self, bytes: &[u8]) -> Vec<u8> {
        self.0.extend_from_slice(bytes);
        self.0
    }
    fn done(self) -> Vec<u8> {
        self.0
    }
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if self.0.len() < n {
            return Err(DecodeError::Truncated);
        }
        let (head, tail) = self.0.split_at(n);
        self.0 = tail;
        Ok(head)
    }
    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, DecodeError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32, DecodeError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn f32(&mut self) -> Result<f32, DecodeError> {
        Ok(f32::from_bits(self.u32()?))
    }
    fn ip(&mut self) -> Result<Ipv4Addr, DecodeError> {
        let b = self.take(4)?;
        Ok(Ipv4Addr::new(b[0], b[1], b[2], b[3]))
    }
    fn str(&mut self) -> Result<String, DecodeError> {
        let len = self.u8()? as usize;
        String::from_utf8(self.take(len)?.to_vec()).map_err(|_| DecodeError::BadString)
    }
    fn rest(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.0).to_vec()
    }
}

fn header(bytes: &[u8]) -> Result<(u8, Reader<'_>), DecodeError> {
    if bytes.len() < 3 {
        return Err(DecodeError::Truncated);
    }
    if bytes[..2] != MAGIC {
        return Err(DecodeError::BadMagic);
    }
    Ok((bytes[2], Reader(&bytes[3..])))
}

impl ClientMessage {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            ClientMessage::Hello { version, nickname } => Writer::new(C_HELLO).u16(*version).str(nickname).done(),
            ClientMessage::Tunnel { from_port, to, to_port, payload } => {
                Writer::new(C_TUNNEL).u16(*from_port).ip(*to).u16(*to_port).rest(payload)
            }
            ClientMessage::KeepAlive => Writer::new(C_KEEPALIVE).done(),
            ClientMessage::Ping { stamp } => Writer::new(C_PING).u32(*stamp).done(),
            ClientMessage::Bye => Writer::new(C_BYE).done(),
            ClientMessage::Spawned { request, object_id } => {
                Writer::new(C_SPAWNED).u32(*request).u16(*object_id).done()
            }
            ClientMessage::SessionRestart => Writer::new(C_SESSION_RESTART).done(),
            ClientMessage::SessionReady => Writer::new(C_SESSION_READY).done(),
        }
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let (tag, mut r) = header(bytes)?;
        Ok(match tag {
            C_HELLO => ClientMessage::Hello { version: r.u16()?, nickname: r.str()? },
            C_TUNNEL => ClientMessage::Tunnel { from_port: r.u16()?, to: r.ip()?, to_port: r.u16()?, payload: r.rest() },
            C_KEEPALIVE => ClientMessage::KeepAlive,
            C_PING => ClientMessage::Ping { stamp: r.u32()? },
            C_BYE => ClientMessage::Bye,
            C_SPAWNED => ClientMessage::Spawned { request: r.u32()?, object_id: r.u16()? },
            C_SESSION_RESTART => ClientMessage::SessionRestart,
            C_SESSION_READY => ClientMessage::SessionReady,
            other => return Err(DecodeError::UnknownTag(other)),
        })
    }
}

impl ServerMessage {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            ServerMessage::Welcome { id, host } => Writer::new(S_WELCOME).u8(*id).u8(*host).done(),
            ServerMessage::Reject(reason) => Writer::new(S_REJECT)
                .u8(match reason {
                    RejectReason::VersionMismatch => 0,
                    RejectReason::ServerFull => 1,
                    RejectReason::BadNickname => 2,
                })
                .done(),
            ServerMessage::PeerJoined { id, nickname } => Writer::new(S_PEER_JOINED).u8(*id).str(nickname).done(),
            ServerMessage::PeerLeft { id } => Writer::new(S_PEER_LEFT).u8(*id).done(),
            ServerMessage::HostChanged { host } => Writer::new(S_HOST_CHANGED).u8(*host).done(),
            ServerMessage::ObjectIds { first, count } => Writer::new(S_OBJECT_IDS).u16(*first).u16(*count).done(),
            ServerMessage::Spawn { request, kind, model, position, heading } => Writer::new(S_SPAWN)
                .u32(*request)
                .u8(kind_byte(*kind))
                .u32(*model)
                .f32(position[0])
                .f32(position[1])
                .f32(position[2])
                .f32(*heading)
                .done(),
            ServerMessage::GameSettings { mode, slots } => {
                Writer::new(S_GAME_SETTINGS).u32(*mode as u32).u8(*slots).done()
            }
            ServerMessage::Despawn { kind, object_id } => {
                Writer::new(S_DESPAWN).u8(kind_byte(*kind)).u16(*object_id).done()
            }
            ServerMessage::Tunnel { from, from_port, to_port, payload } => {
                Writer::new(S_TUNNEL).ip(*from).u16(*from_port).u16(*to_port).rest(payload)
            }
            ServerMessage::SessionRestart { host } => Writer::new(S_SESSION_RESTART).u8(*host).done(),
            ServerMessage::SessionReady { host } => Writer::new(S_SESSION_READY).u8(*host).done(),
            ServerMessage::Pong { stamp } => Writer::new(S_PONG).u32(*stamp).done(),
        }
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let (tag, mut r) = header(bytes)?;
        Ok(match tag {
            S_WELCOME => ServerMessage::Welcome { id: r.u8()?, host: r.u8()? },
            S_REJECT => ServerMessage::Reject(match r.u8()? {
                0 => RejectReason::VersionMismatch,
                1 => RejectReason::ServerFull,
                _ => RejectReason::BadNickname,
            }),
            S_PEER_JOINED => ServerMessage::PeerJoined { id: r.u8()?, nickname: r.str()? },
            S_PEER_LEFT => ServerMessage::PeerLeft { id: r.u8()? },
            S_HOST_CHANGED => ServerMessage::HostChanged { host: r.u8()? },
            S_OBJECT_IDS => ServerMessage::ObjectIds { first: r.u16()?, count: r.u16()? },
            S_SPAWN => ServerMessage::Spawn {
                request: r.u32()?,
                kind: byte_kind(r.u8()?),
                model: r.u32()?,
                position: [r.f32()?, r.f32()?, r.f32()?],
                heading: r.f32()?,
            },
            S_GAME_SETTINGS => ServerMessage::GameSettings { mode: r.u32()? as i32, slots: r.u8()? },
            S_DESPAWN => ServerMessage::Despawn { kind: byte_kind(r.u8()?), object_id: r.u16()? },
            S_TUNNEL => ServerMessage::Tunnel { from: r.ip()?, from_port: r.u16()?, to_port: r.u16()?, payload: r.rest() },
            S_SESSION_RESTART => ServerMessage::SessionRestart { host: r.u8()? },
            S_SESSION_READY => ServerMessage::SessionReady { host: r.u8()? },
            S_PONG => ServerMessage::Pong { stamp: r.u32()? },
            other => return Err(DecodeError::UnknownTag(other)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_messages_round_trip() {
        for m in [
            ClientMessage::Hello { version: PROTOCOL_VERSION, nickname: "Niko".into() },
            ClientMessage::Tunnel { from_port: 6672, to: virtual_ip(2), to_port: 6672, payload: vec![1, 2, 3] },
            ClientMessage::Tunnel { from_port: 1, to: BROADCAST, to_port: 2, payload: vec![] },
            ClientMessage::KeepAlive,
            ClientMessage::Ping { stamp: 123456 },
            ClientMessage::Bye,
            ClientMessage::Spawned { request: 7, object_id: 500 },
            ClientMessage::SessionRestart,
            ClientMessage::SessionReady,
        ] {
            assert_eq!(ClientMessage::decode(&m.encode()), Ok(m));
        }
    }

    #[test]
    fn server_messages_round_trip() {
        for m in [
            ServerMessage::Welcome { id: 3, host: 1 },
            ServerMessage::Reject(RejectReason::ServerFull),
            ServerMessage::PeerJoined { id: 4, nickname: "Roman".into() },
            ServerMessage::PeerLeft { id: 4 },
            ServerMessage::HostChanged { host: 2 },
            ServerMessage::ObjectIds { first: 490, count: 490 },
            ServerMessage::Spawn {
                request: 1,
                kind: SpawnKind::Ped,
                model: 0x1234_5678,
                position: [1.0, -2.5, 30.0],
                heading: 90.0,
            },
            ServerMessage::Despawn { kind: SpawnKind::Vehicle, object_id: 491 },
            ServerMessage::GameSettings { mode: -2, slots: 32 },
            ServerMessage::SessionRestart { host: 1 },
            ServerMessage::SessionReady { host: 1 },
            ServerMessage::Pong { stamp: 99 },
            ServerMessage::Tunnel { from: virtual_ip(1), from_port: 6672, to_port: 6672, payload: vec![9; 100] },
        ] {
            assert_eq!(ServerMessage::decode(&m.encode()), Ok(m));
        }
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(ClientMessage::decode(b"XX\x01"), Err(DecodeError::BadMagic));
        assert_eq!(ClientMessage::decode(b"IV"), Err(DecodeError::Truncated));
        assert_eq!(ClientMessage::decode(b"IV\x7f"), Err(DecodeError::UnknownTag(0x7f)));
        assert_eq!(ServerMessage::decode(b"IV\x81\x01"), Err(DecodeError::Truncated));
    }

    #[test]
    fn virtual_addresses() {
        assert_eq!(virtual_ip(7), Ipv4Addr::new(10, 0, 0, 7));
        assert_eq!(client_id_of(virtual_ip(7)), Some(7));
        assert_eq!(client_id_of(Ipv4Addr::new(192, 168, 0, 7)), None);
        assert_eq!(client_id_of(Ipv4Addr::new(10, 0, 0, 255)), None);
    }
}
