//! The server's side of the game session: it answers every game the way a hosting game would.
//!
//! Per client there is one engine connection with three reliable channels (13 session, 7 game,
//! 15). A channel opens with an empty reliable chunk (a "SYN", sent in a frame with `has_seq`),
//! then carries numbered chunks; each side acks with the next sequence it expects and resends
//! what isn't acked. The join replies are the host's messages from a recording
//! ([`crate::templates`]); only sequence numbers, acks and the time sync are live.
//!
//! Join, from the client: connection check (raw, echoed), key exchange (flag-1 packet, answered
//! with our peer address), session detail request (out of band, ch13), join request (reliable,
//! ch13). We then open our three channels, send the four ch13 join messages and, once the client
//! talks on ch7, the four ch7 messages (object ids, voice and text status, get ready to start).
//!
//! Players learn about each other from the host: snMsgAddGamerToSessionCmd carries the same
//! gamer block a player sent in its join request (both read by 0x626E70), followed by that
//! player's CMsgPeerData. A player's peer data is the start of the CMsgJoinRequest it sent
//! (121 bits), with its player slot in the first byte and the next bit set (as a host does).
//! Client `id` gets slot `32 - id` (the client creates its player there too).

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use ivmp_sync::bits::BitReader;
use ivmp_sync::packet;

use crate::bits::BitWriter;
use crate::frame::{Chunk, Frame, Packet};
use crate::templates;

/// The channels a connection uses.
pub const CHANNELS: [u8; 3] = [13, 7, 15];
pub const CH_SESSION: u8 = 13;
pub const CH_GAME: u8 = 7;
/// Chunk tag seen in every capture.
const TAG: u8 = 13;
/// Unacked chunks are sent again after this long.
const RESEND_AFTER: Duration = Duration::from_millis(250);
/// An unacked SYN is sent again after this long (the engine waits about as long).
const SYN_RESEND_AFTER: Duration = Duration::from_millis(500);
/// Ack-only frames for every open channel at least this often (the engine sends one a second).
const KEEPALIVE: Duration = Duration::from_secs(1);
/// Packet budget for frames (the engine reads 0x400 bytes at most).
const MAX_PAYLOAD: usize = 900;

const MSG_SESSION_DETAIL_REQUEST: u32 = 0x22;
const MSG_JOIN_REQUEST: u32 = 0x41;
const MSG_TIME_SYNC: u32 = 0x38;
const MSG_ADD_GAMER: u32 = 0x64;
const MSG_PEER_DATA: u32 = 0x90;
/// CMsgJoinRequest: the game's part of a join request; starts with the player's peer data.
const MSG_GAME_JOIN_REQUEST: u32 = 0x68;
/// Length of a CMsgPeerData body.
const PEER_DATA_BITS: usize = 121;
/// Token the host puts in time sync answers (any constant works for the client).
const TIME_TOKEN: u32 = 0x10BA_F2FF;

/// Signed distance between two 16-bit sequence numbers.
fn seq_diff(a: u16, b: u16) -> i16 {
    a.wrapping_sub(b) as i16
}

struct Outgoing {
    seq: u16,
    data: Vec<u8>,
    sent: Option<Instant>,
}

#[derive(Default)]
struct Channel {
    /// Our SYN sequence once we opened the channel.
    syn: Option<u16>,
    /// The client acked our SYN.
    established: bool,
    /// When we last sent the SYN (it is resent like data until acked).
    syn_sent: Option<Instant>,
    next_seq: u16,
    unacked: VecDeque<Outgoing>,
    /// Next sequence we expect from the client (after its SYN).
    their_next: Option<u16>,
    /// We owe the client an ack on this channel.
    ack_due: bool,
    /// The client sent us data on this channel.
    heard: bool,
}

impl Channel {
    fn open(&mut self, start: u16) {
        if self.syn.is_none() {
            self.syn = Some(start);
            self.next_seq = start.wrapping_add(1);
        }
    }

    fn queue(&mut self, data: Vec<u8>) {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        self.unacked.push_back(Outgoing { seq, data, sent: None });
    }

    fn acked(&mut self, ack: u16) {
        if let Some(syn) = self.syn {
            if seq_diff(ack, syn) > 0 {
                self.established = true;
            }
        }
        while self.unacked.front().is_some_and(|o| seq_diff(ack, o.seq) > 0) {
            self.unacked.pop_front();
        }
    }
}

/// One client's connection to the server host.
struct Connection {
    channels: BTreeMap<u8, Channel>,
    /// The gamer block from the client's join request (bits), for telling the others about it.
    gamer: Option<(Vec<u8>, usize)>,
    /// The client's CMsgPeerData body (bits), from its CMsgJoinRequest.
    peer_data: Option<(Vec<u8>, usize)>,
    joined: bool,
    game_messages_sent: bool,
    last_sent: Instant,
    /// Raw packets to send next (key exchange answers, connection checks).
    raw: Vec<Vec<u8>>,
    /// Out-of-band frames to send next.
    out_of_band: Vec<Frame>,
}

impl Connection {
    fn new() -> Self {
        Connection {
            channels: CHANNELS.iter().map(|c| (*c, Channel::default())).collect(),
            gamer: None,
            peer_data: None,
            joined: false,
            game_messages_sent: false,
            last_sent: Instant::now(),
            raw: Vec::new(),
            out_of_band: Vec::new(),
        }
    }
}

/// The server's session host.
pub struct HostSession {
    connections: BTreeMap<u8, Connection>,
    started: Instant,
    /// Seed for our channel start sequences.
    seed: u16,
    /// Last session message number used (after the recording's, which end at 0x39).
    envelope_seq: u8,
    pub log: Vec<String>,
}

impl Default for HostSession {
    fn default() -> Self {
        HostSession { connections: BTreeMap::new(), started: Instant::now(), seed: 0x4D2B, envelope_seq: 0x40, log: Vec::new() }
    }
}

impl HostSession {
    pub fn remove(&mut self, client: u8) {
        self.connections.remove(&client);
    }

    /// Clients that finished joining.
    pub fn joined(&self) -> Vec<u8> {
        self.connections.iter().filter(|(_, c)| c.joined).map(|(id, _)| *id).collect()
    }

    /// A game packet from `client` to the host; returns packets to send back.
    pub fn receive(&mut self, client: u8, data: &[u8]) -> Vec<Vec<u8>> {
        let now_ms = self.started.elapsed().as_millis() as u32;
        let seed = &mut self.seed;
        let log = &mut self.log;
        let connection = self.connections.entry(client).or_insert_with(Connection::new);
        if data.first().is_some_and(|f| f & 1 != 0) {
            // Key exchange offer: answer with the host's peer address.
            connection.raw.push(templates::KEY_ANSWER.to_vec());
        } else if let Some(packet) = Packet::parse(data) {
            let mut joined = false;
            for frame in packet.first.iter().chain(&packet.second) {
                joined |= handle_frame(connection, frame, now_ms, seed, log, client);
            }
            if joined {
                self.introduce(client);
            }
            let connection = self.connections.get_mut(&client).unwrap();
            return flush(connection, false);
        } else if data.len() == 11 {
            // Connection check: echoed with the last byte cleared.
            let mut answer = data.to_vec();
            answer[10] = 0;
            connection.raw.push(answer);
        }
        flush(connection, false)
    }

    /// Tells the joined players about `client` and `client` about them.
    fn introduce(&mut self, client: u8) {
        let Some(new) = self.connections.get(&client).and_then(|c| Some((c.gamer.clone()?, c.peer_data.clone()?))) else {
            return;
        };
        let others: Vec<(u8, (Vec<u8>, usize), (Vec<u8>, usize))> = self
            .connections
            .iter()
            .filter(|(id, c)| **id != client && c.joined)
            .filter_map(|(id, c)| Some((*id, c.gamer.clone()?, c.peer_data.clone()?)))
            .collect();
        for (other, gamer, peer_data) in &others {
            self.envelope_seq = self.envelope_seq.wrapping_add(1);
            let to_new = add_gamer(gamer, peer_data, self.envelope_seq);
            self.connections.get_mut(&client).unwrap().channels.get_mut(&CH_SESSION).unwrap().queue(to_new);
            self.envelope_seq = self.envelope_seq.wrapping_add(1);
            let to_other = add_gamer(&new.0, &new.1, self.envelope_seq);
            self.connections.get_mut(other).unwrap().channels.get_mut(&CH_SESSION).unwrap().queue(to_other);
            self.log.push(format!("host: introduced clients {client} and {other}"));
        }
    }

    /// Resends and keep-alive acks; call often. Returns (client, packet) pairs.
    pub fn tick(&mut self) -> Vec<(u8, Vec<u8>)> {
        let mut out = Vec::new();
        for (client, connection) in &mut self.connections {
            let keepalive = connection.last_sent.elapsed() > KEEPALIVE;
            for packet in flush(connection, keepalive) {
                out.push((*client, packet));
            }
        }
        out
    }
}

/// Handles one frame; returns true when the client was just accepted into the session.
fn handle_frame(connection: &mut Connection, frame: &Frame, now_ms: u32, seed: &mut u16, log: &mut Vec<String>, client: u8) -> bool {
    if frame.out_of_band {
        for message in packet::messages(&frame.body) {
            match message.id {
                MSG_SESSION_DETAIL_REQUEST if frame.channel == CH_SESSION => {
                    log.push(format!("host: session details for client {client}"));
                    connection.out_of_band.push(Frame {
                        out_of_band: true,
                        channel: CH_SESSION,
                        body: templates::SESSION_DETAIL.to_vec(),
                        ..Frame::default()
                    });
                }
                MSG_TIME_SYNC => {
                    if let Some(answer) = time_sync_answer(message.body, now_ms) {
                        connection.out_of_band.push(Frame {
                            out_of_band: true,
                            channel: frame.channel,
                            body: answer,
                            ..Frame::default()
                        });
                    }
                }
                _ => {}
            }
        }
        return false;
    }
    let Some(channel) = connection.channels.get_mut(&frame.channel) else { return false };
    if let Some(ack) = frame.ack {
        channel.acked(ack);
    }
    let mut join = false;
    let mut game = false;
    let mut gamer = None;
    let mut peer_data = None;
    for chunk in &frame.chunks {
        channel.ack_due = true;
        // Unreliable chunks use up a sequence number too (`previous + 1`); they are never
        // resent, so a gap before one is skipped.
        let seq = if chunk.reliable { chunk.seq } else { chunk.previous.wrapping_add(1) };
        let expected = channel.their_next.unwrap_or(seq);
        let in_order = if chunk.reliable { seq == expected } else { seq_diff(seq, expected) >= 0 };
        if !in_order {
            continue; // a resend we already have, or ahead of a gap (it comes again)
        }
        channel.their_next = Some(seq.wrapping_add(1));
        if chunk.data.is_empty() {
            continue;
        }
        channel.heard = true;
        let messages = packet::messages(&chunk.data);
        for (i, message) in messages.iter().enumerate() {
            if message.id == MSG_JOIN_REQUEST && frame.channel == CH_SESSION {
                join = true;
                // The gamer block runs to the next message (CMsgJoinRequest) or the end.
                let start = message.body.position();
                let end = messages.get(i + 1).map_or(chunk.data.len() * 8, |next| next.body.position() - 23);
                gamer = Some(copy_bits(&chunk.data, start, end));
            }
            if message.id == MSG_GAME_JOIN_REQUEST && frame.channel == CH_SESSION {
                let start = message.body.position();
                let (mut bits, len) = copy_bits(&chunk.data, start, start + PEER_DATA_BITS);
                if len == PEER_DATA_BITS {
                    set_bits(&mut bits, 0, player_slot(client) as u32, 8);
                    set_bits(&mut bits, 8, 1, 1);
                    peer_data = Some((bits, len));
                }
            }
        }
        if frame.channel == CH_GAME {
            game = true;
        }
    }
    let accepted = join && !connection.joined;
    if accepted {
        connection.joined = true;
        connection.gamer = gamer;
        connection.peer_data = peer_data;
        log.push(format!(
            "host: client {client} asks to join, accepting (slot {}, gamer block {} bits)",
            player_slot(client),
            connection.gamer.as_ref().map_or(0, |g| g.1)
        ));
        for (i, id) in CHANNELS.iter().enumerate() {
            *seed = seed.wrapping_mul(25173).wrapping_add(13849);
            let start = seed.wrapping_add(i as u16 * 7919);
            connection.channels.get_mut(id).unwrap().open(start);
        }
        let session = connection.channels.get_mut(&CH_SESSION).unwrap();
        for (i, message) in templates::JOIN_SESSION.iter().enumerate() {
            let mut message = message.to_vec();
            if i == 0 {
                // The joiner's own peer data (its slot).
                if let Some((bits, len)) = &connection.peer_data {
                    for k in 0..*len {
                        let bit = (bits[k / 8] >> (7 - k % 8)) & 1;
                        set_bits(&mut message, templates::JOIN_PEER_DATA_BIT + k, bit as u32, 1);
                    }
                }
            }
            session.queue(message);
        }
    }
    if game && connection.joined && !connection.game_messages_sent {
        connection.game_messages_sent = true;
        log.push(format!("host: client {client} is in, sending game state"));
        let game_channel = connection.channels.get_mut(&CH_GAME).unwrap();
        for message in templates::JOIN_GAME {
            game_channel.queue(message.to_vec());
        }
    }
    accepted
}

/// The player slot of client `id`: the server is slot 0, clients count down from 31.
pub fn player_slot(client: u8) -> u8 {
    32u8.saturating_sub(client).max(1)
}

/// Bits `start..end` of `data`, left-aligned: (bytes, bit count).
fn copy_bits(data: &[u8], start: usize, end: usize) -> (Vec<u8>, usize) {
    let mut w = BitWriter::new();
    for i in start..end.min(data.len() * 8) {
        w.write(((data[i / 8] >> (7 - i % 8)) & 1) as u32, 1);
    }
    let len = w.len_bits();
    (w.finish(), len)
}

/// Overwrites `bits` bits at bit `at` with `value`.
fn set_bits(data: &mut [u8], at: usize, value: u32, bits: usize) {
    for k in 0..bits {
        let i = at + k;
        let mask = 0x80 >> (i % 8);
        if (value >> (bits - 1 - k)) & 1 == 1 {
            data[i / 8] |= mask;
        } else {
            data[i / 8] &= !mask;
        }
    }
}

/// snMsgAddGamerToSessionCmd for a player: the session envelope, the message header (id 0x64),
/// the player's gamer block from its join request, then its CMsgPeerData with its slot.
fn add_gamer(gamer: &(Vec<u8>, usize), peer_data: &(Vec<u8>, usize), envelope_seq: u8) -> Vec<u8> {
    let mut w = BitWriter::new();
    // The envelope's last byte numbers the host's session messages; a repeated number is taken
    // for a duplicate and dropped (seen as "add gamer: message valid -> 0").
    let mut envelope = templates::SESSION_ENVELOPE;
    envelope[6] = envelope_seq;
    w.bytes(&envelope);
    w.write(packet::MAGIC, 14).write(0, 1).write(MSG_ADD_GAMER, 8);
    // The block starts with the session id, which receivers check against theirs (0x623D80);
    // a join request may carry another value, so use the session's (from the host's AddGamer).
    w.bytes(&templates::SESSION_ID);
    for i in 64..gamer.1 {
        w.write(((gamer.0[i / 8] >> (7 - i % 8)) & 1) as u32, 1);
    }
    w.write(packet::MAGIC, 14).write(0, 1).write(MSG_PEER_DATA, 8);
    for i in 0..peer_data.1 {
        w.write(((peer_data.0[i / 8] >> (7 - i % 8)) & 1) as u32, 1);
    }
    w.finish()
}

/// Answer to a time sync request (netTimeSyncMsg, 0x6BE7E0): 2-bit type (0 request, 1 answer),
/// then u32 sequence, u32 token, u32 client time, and for answers u32 host time.
fn time_sync_answer(mut body: BitReader<'_>, now_ms: u32) -> Option<Vec<u8>> {
    if body.read(2)? != 0 {
        return None;
    }
    let sequence = body.read(32)?;
    body.read(32)?;
    let client_time = body.read(32)?;
    let mut w = BitWriter::new();
    w.write(packet::MAGIC, 14).write(0, 1).write(MSG_TIME_SYNC, 8);
    w.write(1, 2).write(sequence, 32).write(TIME_TOKEN, 32).write(client_time, 32).write(now_ms, 32);
    Some(w.finish())
}

/// Builds the packets for everything due on `connection`.
fn flush(connection: &mut Connection, keepalive: bool) -> Vec<Vec<u8>> {
    let mut packets: Vec<Vec<u8>> = std::mem::take(&mut connection.raw);
    let mut frames: Vec<Frame> = std::mem::take(&mut connection.out_of_band);
    let now = Instant::now();
    for (id, channel) in &mut connection.channels {
        let mut chunks = Vec::new();
        let mut has_seq = false;
        if let Some(syn) = channel.syn {
            let due = channel.syn_sent.is_none_or(|t| now.duration_since(t) >= SYN_RESEND_AFTER);
            if !channel.established && due {
                channel.syn_sent = Some(now);
                chunks.push(Chunk { reliable: true, previous: syn.wrapping_sub(1), seq: syn, tag: TAG, data: Vec::new() });
                has_seq = true;
            }
        }
        if channel.established {
            for outgoing in &mut channel.unacked {
                if outgoing.sent.is_some_and(|t| now.duration_since(t) < RESEND_AFTER) {
                    continue;
                }
                outgoing.sent = Some(now);
                chunks.push(Chunk {
                    reliable: true,
                    previous: outgoing.seq.wrapping_sub(1),
                    seq: outgoing.seq,
                    tag: TAG,
                    data: outgoing.data.clone(),
                });
            }
        }
        let ack = channel.their_next;
        let open = channel.syn.is_some() || ack.is_some();
        if chunks.is_empty() && !(channel.ack_due || (keepalive && open)) {
            continue;
        }
        channel.ack_due = false;
        // One frame per chunk keeps every frame under the engine's frame limit.
        if chunks.is_empty() {
            frames.push(Frame { channel: *id, ack, ..Frame::default() });
        }
        for chunk in chunks {
            frames.push(Frame { channel: *id, ack, has_seq, chunks: vec![chunk], ..Frame::default() });
        }
    }
    let mut packet = Packet::default();
    let mut size = 0;
    for frame in frames {
        let mut bytes = Vec::new();
        frame.encode(&mut bytes);
        if size + bytes.len() > MAX_PAYLOAD && !packet.first.is_empty() {
            packets.push(std::mem::take(&mut packet).encode(nonce()));
            size = 0;
        }
        size += bytes.len();
        packet.first.push(frame);
    }
    if !packet.first.is_empty() {
        packets.push(packet.encode(nonce()));
    }
    if !packets.is_empty() {
        connection.last_sent = now;
    }
    packets
}

fn nonce() -> u32 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u32)
}
