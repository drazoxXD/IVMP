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
//! gamer block a player sent in its join request (both read by 0x626E70: session part, gamer
//! info, peer address, a field, then app data as a 10-bit byte length + bytes, then a 6-bit gamer
//! count). In a join request the app data is the CMsgJoinRequest; in AddGamer it is the player's
//! CMsgPeerData. A player's peer data is the start of the CMsgJoinRequest it sent
//! (121 bits), with its player slot in the first byte and the next bit set (as a host does).
//! Client `id` gets slot `32 - id` (the client creates its player there too).
//!
//! The server is a player in the session, so every game sends it its clone creates, removes and
//! syncs and waits for them to be confirmed (an entity only changes owner, e.g. to the player who
//! gets in a car, once everyone has it). We confirm like a player whose game has them all:
//! creates and removes in CMsgPackedReliables sections 1 and 3 (14-bit object id + 3-bit result,
//! 0 = ok; reliable), syncs with CMsgPackedCloneSyncAcks (16-bit sequence, 8 bits 0, 1 bit 1;
//! unreliable), as the games do for each other. Game events (CMsgPackedEvents) that have the
//! server in scope are acked (CMsgPackedEventReliablesMsgs, reliable), array element updates
//! (CMsgArrayElements) too (CMsgArrayElementsAck, unreliable); otherwise the games send them
//! again forever.
//!
//! When a player leaves, the others get snMsgRemoveGamersFromSessionCmd with its gamer handle
//! and drop it (and its player) from their session.

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
const MSG_REMOVE_GAMERS: u32 = 0x53;
const MSG_PEER_DATA: u32 = 0x90;
const MSG_CLONE_SYNC: u32 = 0x8A;
const MSG_CLONE_SYNC_ACKS: u32 = 0x8B;
const MSG_PACKED_RELIABLES: u32 = 0x8C;
const MSG_PACKED_EVENTS: u32 = 0x4F;
const MSG_EVENT_RELIABLES: u32 = 0x89;
const MSG_ARRAY_ELEMENTS: u32 = 0x86;
const MSG_ARRAY_ELEMENTS_ACK: u32 = 0x87;
/// Items per event block (5-bit count).
const MAX_BLOCK_ITEMS: usize = 31;
/// Items per CMsgPackedReliables section (5-bit count).
const MAX_SECTION_ITEMS: usize = 31;
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
    /// The reliable sequence before this one (the chunk's `previous` field).
    previous: u16,
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
    /// Our last reliable sequence (the SYN's at first).
    last_reliable: u16,
    unacked: VecDeque<Outgoing>,
    /// Next sequence we expect from the client (after its SYN).
    their_next: Option<u16>,
    /// The client's last reliable sequence we took.
    their_last_reliable: Option<u16>,
    /// We owe the client an ack on this channel.
    ack_due: bool,
    /// The client sent us data on this channel.
    heard: bool,
    /// Unreliable messages to send next (they use up a sequence number too).
    unreliable: Vec<Vec<u8>>,
}

impl Channel {
    fn open(&mut self, start: u16) {
        if self.syn.is_none() {
            self.syn = Some(start);
            self.next_seq = start.wrapping_add(1);
            self.last_reliable = start;
        }
    }

    fn queue(&mut self, data: Vec<u8>) {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        self.unacked.push_back(Outgoing { seq, previous: self.last_reliable, data, sent: None });
        self.last_reliable = seq;
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
    /// `client` left: the others drop it from their session (and its player with it).
    pub fn remove(&mut self, client: u8) {
        let Some(gone) = self.connections.remove(&client) else { return };
        let Some(handle) = gone.gamer.as_ref().and_then(gamer_handle) else { return };
        for (id, connection) in &mut self.connections {
            if !connection.joined {
                continue;
            }
            self.envelope_seq = self.envelope_seq.wrapping_add(1);
            connection.channels.get_mut(&CH_SESSION).unwrap().queue(remove_gamers(&[handle], self.envelope_seq));
            self.log.push(format!("host: told client {id} that client {client} left"));
        }
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
    let (mut sync_acks, mut create_acks, mut remove_acks) = (Vec::new(), Vec::new(), Vec::new());
    let (mut event_acks, mut array_acks) = (Vec::new(), Vec::new());
    // As the engine reads chunks (0x6F60B0): every chunk has a sequence number, a reliable one
    // in its header, an unreliable one counted on from the frame's `seq`. A chunk's `previous`
    // is the sender's last reliable sequence; a chunk is taken in order when that is the last
    // reliable we took and its sequence isn't behind. Unreliable chunks are never resent, so a
    // gap before one is skipped; anything else (a resend we have, a reliable chunk after a lost
    // one) is dropped and comes again.
    let mut running = frame.seq;
    for chunk in &frame.chunks {
        channel.ack_due = true;
        let seq = if chunk.reliable { chunk.seq } else { running };
        if seq == running {
            running = running.wrapping_add(1);
        }
        let expected = channel.their_next.unwrap_or(seq);
        let last = channel.their_last_reliable.unwrap_or(chunk.previous);
        if chunk.previous != last || seq_diff(seq, expected) < 0 {
            continue;
        }
        channel.their_next = Some(seq.wrapping_add(1));
        if chunk.reliable {
            channel.their_last_reliable = Some(seq);
        }
        if chunk.data.is_empty() {
            continue;
        }
        channel.heard = true;
        let messages = packet::messages(&chunk.data);
        for (i, message) in messages.iter().enumerate() {
            if message.id == MSG_JOIN_REQUEST && frame.channel == CH_SESSION {
                join = true;
                // The gamer block up to its app data: the app data is a 10-bit byte length and
                // the embedded CMsgJoinRequest (the next "message" found), then a gamer count.
                let start = message.body.position();
                let Some(app) = messages.get(i + 1) else { continue };
                let end = app.body.position() - 23 - 10;
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
            // Messages start at the chunk's first bit; anything found later is part of their data.
            if let Some(message) = messages.iter().find(|m| m.body.position() == 23) {
                match message.id {
                    MSG_CLONE_SYNC => {
                        if let Some(sequence) = message.body.clone().read(16) {
                            sync_acks.push(sequence as u16);
                        }
                    }
                    MSG_PACKED_RELIABLES => {
                        if let Some(reliables) = packet::packed_reliables(message.body.clone()) {
                            create_acks.extend(reliables.creates.iter().map(|c| c.object_id));
                            remove_acks.extend(reliables.removes.iter().copied());
                        }
                    }
                    MSG_PACKED_EVENTS => event_acks.extend(event_ids(message.body.clone()).unwrap_or_default()),
                    MSG_ARRAY_ELEMENTS => array_acks.extend(array_ack(message.body.clone())),
                    _ => {}
                }
            }
        }
    }
    if let Some(game_channel) = connection.channels.get_mut(&CH_GAME) {
        for sequence in sync_acks {
            game_channel.unreliable.push(sync_ack(sequence));
        }
        for message in reliable_acks(&create_acks, &remove_acks) {
            game_channel.queue(message);
        }
        for ids in event_acks.chunks(MAX_BLOCK_ITEMS) {
            game_channel.queue(event_ack(ids));
        }
        game_channel.unreliable.extend(array_acks);
    }
    let accepted = join && !connection.joined;
    if accepted {
        connection.joined = true;
        connection.gamer = gamer;
        connection.peer_data = peer_data;
        log.push(format!(
            "host: client {client} asks to join, accepting (slot {}, gamer block {} bits, handle {})",
            player_slot(client),
            connection.gamer.as_ref().map_or(0, |g| g.1),
            connection.gamer.as_ref().and_then(gamer_handle).map_or("?".into(), |h| h.iter().map(|b| format!("{b:02x}")).collect::<String>())
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
    // App data: the player's CMsgPeerData, as bytes with a 10-bit length (0x627640), then the
    // gamer count (0, as in the host's own AddGamer).
    let mut app = BitWriter::new();
    app.write(packet::MAGIC, 14).write(0, 1).write(MSG_PEER_DATA, 8);
    for i in 0..peer_data.1 {
        app.write(((peer_data.0[i / 8] >> (7 - i % 8)) & 1) as u32, 1);
    }
    let app = app.finish();
    w.write(app.len() as u32, 10);
    w.bytes(&app);
    w.write(0, 6);
    w.finish()
}

/// The gamer handle (two u32, 0x6BCFD0) at the start of the gamer info: in a player's gamer
/// block, after the session id and one set bit.
fn gamer_handle(gamer: &(Vec<u8>, usize)) -> Option<[u8; 8]> {
    if gamer.1 < 129 {
        return None;
    }
    let (bytes, _) = copy_bits(&gamer.0, 65, 129);
    bytes.try_into().ok()
}

/// snMsgRemoveGamersFromSessionCmd (writer 0x627060, reader 0x6271A0): the session envelope,
/// the message header (id 0x53), session id, 6-bit gamer count, the gamer handles, then a 16-bit
/// reason (sign bit + 15 bits). Members drop the gamers if it comes from the host and the session
/// id matches (0x623EE0, applied by 0x622360).
fn remove_gamers(handles: &[[u8; 8]], envelope_seq: u8) -> Vec<u8> {
    let mut w = BitWriter::new();
    let mut envelope = templates::SESSION_ENVELOPE;
    envelope[6] = envelope_seq;
    w.bytes(&envelope);
    w.write(packet::MAGIC, 14).write(0, 1).write(MSG_REMOVE_GAMERS, 8);
    w.bytes(&templates::SESSION_ID);
    w.write(handles.len() as u32, 6);
    for handle in handles {
        w.bytes(handle);
    }
    w.write(0, 16);
    w.finish()
}

/// CMsgPackedCloneSyncAcks for one clone sync: sequence, 8 bits 0, then a set bit (as the games
/// send it).
fn sync_ack(sequence: u16) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.write(packet::MAGIC, 14).write(0, 1).write(MSG_CLONE_SYNC_ACKS, 8);
    w.write(sequence as u32, 16).write(0, 8).write(1, 1);
    w.finish()
}

/// CMsgPackedReliables with create acks (section 1) and remove acks (section 3), result 0.
fn reliable_acks(creates: &[u16], removes: &[u16]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let (mut creates, mut removes) = (creates.chunks(MAX_SECTION_ITEMS), removes.chunks(MAX_SECTION_ITEMS));
    loop {
        let (c, r) = (creates.next(), removes.next());
        if c.is_none() && r.is_none() {
            return out;
        }
        let mut w = BitWriter::new();
        w.write(packet::MAGIC, 14).write(0, 1).write(MSG_PACKED_RELIABLES, 8);
        let mask = (c.is_some() as u32) << 1 | (r.is_some() as u32) << 3;
        w.write(mask, 4);
        for items in [c, r].into_iter().flatten() {
            w.write(items.len() as u32, 5).write((items.len() * 17) as u32, 10);
            for id in items {
                w.write(*id as u32, 14).write(0, 3);
            }
        }
        out.push(w.finish());
    }
}

/// The ids of the events in a CMsgPackedEvents (0x4F; writer 0x4AC010, events read by
/// 0x4AEFB0): 5-bit event count, 15-bit length in bits, then per event a 5-bit type, 15-bit id
/// (low 7 bits number, the rest a sequence), 15-bit data length, a flag (16 more bits if set)
/// and the data. The games send events to every player in their scope, the server included,
/// and send them again until each one acks or replies.
fn event_ids(mut body: BitReader<'_>) -> Option<Vec<u16>> {
    body.read(5)?;
    let len = body.read(15)? as usize;
    let mut events = body.sub(len)?;
    let mut ids = Vec::new();
    while events.remaining() >= 35 {
        events.read(5)?;
        ids.push(events.read(15)? as u16);
        let data = events.read(15)? as usize;
        if events.read(1)? == 1 {
            events.read(16)?;
        }
        events.skip(data)?;
    }
    Some(ids)
}

/// CMsgPackedEventReliablesMsgs (0x89; writer 0x4B18C0) acking events: a 2-bit mask (1 = acks,
/// 2 = replies), then the ack block: 5-bit count, 15-bit length, the 15-bit event ids (0x4AF330).
/// An ack ends an event for us like a reply would (0x4AFBB0), without answering it; replies
/// carry event-specific data (0x4AF3E0) and the server has nothing to say.
fn event_ack(ids: &[u16]) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.write(packet::MAGIC, 14).write(0, 1).write(MSG_EVENT_RELIABLES, 8);
    w.write(1, 2).write(ids.len() as u32, 5).write((ids.len() * 15) as u32, 15);
    for id in ids {
        w.write(*id as u32, 15);
    }
    w.finish()
}

/// CMsgArrayElementsAck (0x87; writer 0x4B8940) for a CMsgArrayElements (0x86: 13-bit length,
/// then data starting with a 16-bit sequence, a 3-bit array handler and a 6-bit index): the
/// handler, the index and the sequence, as the games ack each other.
fn array_ack(mut body: BitReader<'_>) -> Option<Vec<u8>> {
    let len = body.read(13)?;
    if len < 25 {
        return None;
    }
    let (sequence, handler, index) = (body.read(16)?, body.read(3)?, body.read(6)?);
    let mut w = BitWriter::new();
    w.write(packet::MAGIC, 14).write(0, 1).write(MSG_ARRAY_ELEMENTS_ACK, 8);
    w.write(handler, 3).write(index, 6).write(sequence, 16);
    Some(w.finish())
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
                    previous: outgoing.previous,
                    seq: outgoing.seq,
                    tag: TAG,
                    data: outgoing.data.clone(),
                });
            }
        }
        if channel.established {
            // An unreliable chunk's number goes in its frame's `seq` (see `handle_frame`).
            for data in channel.unreliable.drain(..) {
                let seq = channel.next_seq;
                channel.next_seq = channel.next_seq.wrapping_add(1);
                chunks.push(Chunk { reliable: false, previous: channel.last_reliable, seq, tag: TAG, data });
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
            // The frame's `seq` numbers its unreliable chunks (the games send 0 without any).
            let seq = if chunk.reliable { 0 } else { chunk.seq };
            frames.push(Frame { channel: *id, ack, has_seq, seq, chunks: vec![chunk], ..Frame::default() });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gamer_handle_follows_session_id() {
        // The host's AddGamer: envelope (56 bits) and header (23 bits), then its gamer block.
        let add_gamer = templates::JOIN_SESSION[1];
        let block = copy_bits(add_gamer, 56 + 23, add_gamer.len() * 8);
        assert_eq!(gamer_handle(&block), Some(*b"IVMPSRV\xFE"));
    }

    fn chunk(reliable: bool, previous: u16, seq: u16) -> Chunk {
        Chunk { reliable, previous, seq: if reliable { seq } else { 0 }, tag: TAG, data: vec![0] }
    }

    #[test]
    fn unreliable_chunks_between_reliables_keep_the_channel_going() {
        // As in host11: reliable 100, unreliable 101..=103 (frame seq), then reliable 104 whose
        // `previous` is the last reliable, 100.
        let mut connection = Connection::new();
        let (mut seed, mut log) = (0, Vec::new());
        let mut frame = |seq: u16, chunks: Vec<Chunk>| {
            let frame = Frame { channel: CH_GAME, seq, chunks, ..Frame::default() };
            handle_frame(&mut connection, &frame, 0, &mut seed, &mut log, 1);
            connection.channels[&CH_GAME].their_next
        };
        assert_eq!(frame(0, vec![chunk(true, 99, 100)]), Some(101));
        for seq in 101..=103 {
            assert_eq!(frame(seq, vec![chunk(false, 100, 0)]), Some(seq + 1));
        }
        assert_eq!(frame(0, vec![chunk(true, 100, 104)]), Some(105));
        // A resend of 104 is dropped, a reliable chunk after a lost one too.
        assert_eq!(frame(0, vec![chunk(true, 100, 104)]), Some(105));
        assert_eq!(frame(0, vec![chunk(true, 105, 106)]), Some(105));
    }

    #[test]
    fn sent_chunks_name_the_last_reliable() {
        let mut connection = Connection::new();
        let channel = connection.channels.get_mut(&CH_GAME).unwrap();
        channel.open(10);
        channel.established = true;
        channel.queue(vec![1]); // reliable 11
        channel.unreliable.push(vec![2]); // unreliable 12
        let mut frames = Vec::new();
        for packet in flush(&mut connection, false) {
            let packet = Packet::parse(&packet).unwrap();
            frames.extend(packet.first.into_iter().filter(|f| f.channel == CH_GAME));
        }
        let channel = connection.channels.get_mut(&CH_GAME).unwrap();
        channel.queue(vec![3]); // reliable 13, after unreliable 12
        for packet in flush(&mut connection, false) {
            let packet = Packet::parse(&packet).unwrap();
            frames.extend(packet.first.into_iter().filter(|f| f.channel == CH_GAME));
        }
        let seen: Vec<(bool, u16, u16, u16)> =
            frames.iter().flat_map(|f| f.chunks.iter().map(move |c| (c.reliable, c.previous, c.seq, f.seq))).collect();
        assert_eq!(seen, [(true, 10, 11, 0), (false, 11, 0, 12), (true, 11, 13, 0)]);
    }

    /// A message from a bit string ('0'/'1') after its header.
    fn message(id: u32, body: &str) -> Vec<u8> {
        let mut w = BitWriter::new();
        w.write(packet::MAGIC, 14).write(0, 1).write(id, 8);
        for c in body.chars() {
            w.write((c == '1') as u32, 1);
        }
        w.finish()
    }

    fn bits(data: &[u8], from: usize, to: usize) -> String {
        (from..to).map(|i| if (data[i / 8] >> (7 - i % 8)) & 1 == 1 { '1' } else { '0' }).collect()
    }

    #[test]
    fn events_are_acked_like_the_games_do() {
        // host12, client 2 -> 1 at 334.5 s: one event (type 3, id 130, 23 bits of data); client 1
        // answered with acks block [1 | 15b 000000010000010].
        let body = "00001".to_owned()
            + "000000000111011"
            + "00011000000010000010000000000010111011000000100000110000000";
        let events = message(MSG_PACKED_EVENTS, &body);
        let ids = event_ids(packet::messages(&events)[0].body.clone()).unwrap();
        assert_eq!(ids, [130]);
        let ack = event_ack(&ids);
        assert_eq!(bits(&ack, 23, 23 + 2 + 5 + 15 + 15), "01".to_owned() + "00001" + "000000000001111" + "000000010000010");
    }

    #[test]
    fn array_elements_are_acked_like_the_games_do() {
        // host12, client 2 -> 1 at 455.2 s (start of its 83 bits); client 1 acked "6 0 2".
        let data = "00000000000000101100000000100000000001111101111011000111000111000011101001010000100";
        let body = format!("{:013b}{data}", data.len());
        let elements = message(MSG_ARRAY_ELEMENTS, &body);
        let ack = array_ack(packet::messages(&elements)[0].body.clone()).unwrap();
        assert_eq!(bits(&ack, 23, 23 + 25), format!("{:03b}{:06b}{:016b}", 6, 0, 2));
    }

    #[test]
    fn remove_gamers_layout() {
        let message = remove_gamers(&[*b"IVMPSRV\xFE"], 0x42);
        let mut r = BitReader::new(&message);
        r.skip(56);
        assert_eq!(r.read(14), Some(packet::MAGIC));
        assert_eq!(r.read(1), Some(0));
        assert_eq!(r.read(8), Some(MSG_REMOVE_GAMERS));
        assert_eq!(r.read(32), Some(2));
        assert_eq!(r.read(32), Some(0));
        assert_eq!(r.read(6), Some(1));
        assert_eq!(r.read(32), Some(0x49564D50));
        assert_eq!(r.read(32), Some(0x535256FE));
        assert_eq!(r.read(16), Some(0));
    }
}
