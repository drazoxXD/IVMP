//! Game events in the clients' traffic (CMsgPackedEvents, message 0x4F): what the server can see of
//! damage, fire, explosions, pickups, control hand-overs. Stage one of server-side events: read and
//! log them, change nothing (docs/onesync-plan.md, "Game events and array elements").
//!
//! 0x4F body (writer 0x4AC010): 5-bit count, 15-bit length in bits, then the events. Each event
//! (reader 0x4AEFB0): 5-bit type, 15-bit id (low 7 bits the number, the rest a sequence), 15-bit
//! data length in bits, a flag (16 more bits when set), then the data.

use std::collections::{BTreeMap, HashSet, VecDeque};

use crate::bits::BitReader;
use crate::packet;

/// CMsgPackedEvents.
pub const PACKED_EVENTS: u32 = 0x4F;

/// The event types known so far (the rest print as `type N`).
pub fn kind_name(kind: u8) -> Option<&'static str> {
    Some(match kind {
        1 => "GIVE_CONTROL",
        3 => "WEAPON_DAMAGE",
        11 => "VEHICLE_COMPONENT_CONTROL",
        12 => "REQUEST_FIRE",
        14 => "REQUEST_EXPLOSION",
        19 => "CREATE_PICKUP",
        25 => "DOOR_BREAK",
        _ => return None,
    })
}

/// Event types worth a log line each (the others, like control hand-overs, are only counted).
fn logged(kind: u8) -> bool {
    matches!(kind, 3 | 12 | 14 | 19 | 25)
}

/// One event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameEvent {
    pub kind: u8,
    /// The event's number and sequence (`id & 0x7F`, `id >> 7`); `NAME_<number>_<sequence>` in the
    /// game's own log.
    pub number: u8,
    pub sequence: u16,
    /// The 16 extra bits some events carry.
    pub extra: Option<u16>,
    pub data_bits: usize,
    /// The first bytes of the data, for looking at.
    pub data: Vec<u8>,
}

/// The events in a 0x4F body.
pub fn parse(mut body: BitReader<'_>) -> Option<Vec<GameEvent>> {
    body.read(5)?;
    let len = body.read(15)? as usize;
    let mut block = body.sub(len)?;
    let mut out = Vec::new();
    while block.remaining() >= 35 {
        let kind = block.read(5)? as u8;
        let id = block.read(15)?;
        let data_bits = block.read(15)? as usize;
        let extra = if block.read(1)? == 1 { Some(block.read(16)? as u16) } else { None };
        let mut data_reader = block.sub(data_bits)?;
        let mut data = Vec::new();
        let mut left = data_bits.min(128);
        while left > 0 {
            let take = left.min(8);
            data.push((data_reader.read(take)? << (8 - take)) as u8);
            left -= take;
        }
        out.push(GameEvent { kind, number: (id & 0x7F) as u8, sequence: (id >> 7) as u16, extra, data_bits, data });
    }
    Some(out)
}

/// Looks at every client's game packets for events, counts them, and says what is worth saying.
#[derive(Default)]
pub struct EventLog {
    /// Events already reported (the game resends an event until it is acknowledged).
    seen: HashSet<(u8, u8, u8, u16)>,
    order: VecDeque<(u8, u8, u8, u16)>,
    /// (sender, type) -> distinct events.
    counts: BTreeMap<(u8, u8), u64>,
    changed: bool,
}

const SEEN_LIMIT: usize = 4096;

impl EventLog {
    /// One game packet sent by client `from`: lines to log for the events in it.
    pub fn observe(&mut self, from: u8, packet: &[u8]) -> Vec<String> {
        let mut lines = Vec::new();
        for frame in packet::frames(packet) {
            for message in packet::messages(&frame) {
                if message.id != PACKED_EVENTS {
                    continue;
                }
                let Some(events) = parse(message.body) else { continue };
                for event in events {
                    let key = (from, event.kind, event.number, event.sequence);
                    if !self.seen.insert(key) {
                        continue;
                    }
                    self.order.push_back(key);
                    if self.order.len() > SEEN_LIMIT {
                        if let Some(old) = self.order.pop_front() {
                            self.seen.remove(&old);
                        }
                    }
                    *self.counts.entry((from, event.kind)).or_default() += 1;
                    self.changed = true;
                    if logged(event.kind) {
                        let name = kind_name(event.kind).map_or_else(|| format!("type {}", event.kind), str::to_string);
                        let hex: Vec<String> = event.data.iter().take(16).map(|b| format!("{b:02x}")).collect();
                        let extra = event.extra.map(|e| format!(" +{e:#x}")).unwrap_or_default();
                        lines.push(format!(
                            "client {from} event {name} {}_{}{extra}: {} bits [{}]",
                            event.number,
                            event.sequence,
                            event.data_bits,
                            hex.join(" ")
                        ));
                    }
                }
            }
        }
        lines
    }

    /// Event counts by client and type since the last summary, None when nothing new.
    pub fn summary(&mut self) -> Option<String> {
        if !std::mem::take(&mut self.changed) {
            return None;
        }
        let parts: Vec<String> = self
            .counts
            .iter()
            .map(|((from, kind), n)| {
                let name = kind_name(*kind).map_or_else(|| format!("type {kind}"), str::to_string);
                format!("client {from} {name} x{n}")
            })
            .collect();
        Some(format!("game events so far: {}", parts.join(", ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MSB-first bit writer for building test bodies.
    struct Bits(Vec<bool>);
    impl Bits {
        fn put(&mut self, value: u32, bits: usize) {
            for i in (0..bits).rev() {
                self.0.push(value >> i & 1 == 1);
            }
        }
        fn bytes(&self) -> Vec<u8> {
            let mut out = vec![0u8; self.0.len().div_ceil(8)];
            for (i, bit) in self.0.iter().enumerate() {
                if *bit {
                    out[i / 8] |= 0x80 >> (i % 8);
                }
            }
            out
        }
    }

    #[test]
    fn parses_events() {
        // Two events: type 3 (id 5, sequence 2, 12 data bits 0xABC) and type 14 (id 1, extra bits).
        let mut events = Bits(Vec::new());
        events.put(3, 5);
        events.put(5 | 2 << 7, 15);
        events.put(12, 15);
        events.put(0, 1);
        events.put(0xABC, 12);
        events.put(14, 5);
        events.put(1, 15);
        events.put(4, 15);
        events.put(1, 1);
        events.put(0x1234, 16);
        events.put(0b1010, 4);
        let mut body = Bits(Vec::new());
        body.put(2, 5);
        body.put(events.0.len() as u32, 15);
        body.0.extend(events.0);
        let bytes = body.bytes();
        let parsed = parse(BitReader::new(&bytes)).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!((parsed[0].kind, parsed[0].number, parsed[0].sequence, parsed[0].data_bits), (3, 5, 2, 12));
        assert_eq!(parsed[0].data, vec![0xAB, 0xC0]);
        assert_eq!((parsed[1].kind, parsed[1].extra, parsed[1].data_bits), (14, Some(0x1234), 4));
    }

    #[test]
    fn names_and_log_filter() {
        assert_eq!(kind_name(3), Some("WEAPON_DAMAGE"));
        assert_eq!(kind_name(2), None);
        assert!(logged(14) && !logged(1));
    }
}
