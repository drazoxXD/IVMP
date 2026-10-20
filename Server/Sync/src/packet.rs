//! The engine's packet layout (GTA IV 1.2.0.59, see docs/onesync-plan.md):
//!
//! * packet: `[flags u8][nonce u32][payload][mac u32]` (0x6D2020 / 0x6D2320).
//! * payload: big-endian u16 split offset `h` and a flags byte, then two frame areas,
//!   `payload[3..h]` and `payload[h..]` (0x6CF560).
//! * frame: 9-byte header whose first 10 bits are the frame size (header included), then data.
//! * message: 14-bit magic 0x3246, 1 bit (16-bit id follows when set), 8 or 16-bit id
//!   (0x47F8A0 / 0x47EFE0), then the message body.

use crate::bits::BitReader;

pub const MAGIC: u32 = 0x3246;

/// Message ids (fixed at init by the game's AutoIdDescriptors).
pub mod id {
    pub const CLONE_SYNC: u32 = 0x8A;
    pub const PACKED_CLONE_SYNC_ACKS: u32 = 0x8B;
    pub const PACKED_RELIABLES: u32 = 0x8C;
    pub const INFORM_OBJECT_IDS: u32 = 0x95;
}

pub const OBJECT_TYPES: [&str; 10] =
    ["PLAYER", "PED", "DUMMYPED", "AUTOMOBILE", "OBJECT", "BIKE", "TRAIN", "BOAT", "HELI", "PLANE"];

const PACKET_HEADER: usize = 5;
const PACKET_MAC: usize = 4;
const FRAME_HEADER: usize = 9;

/// The engine's packet compression (LZSS, 0x670EB0): little-endian u16 length of what follows,
/// then groups of a control byte (MSB first; 1 = literal byte, 0 = match) and 8 items. A match
/// is two bytes `a, b`: copy `(a & 0xF) + 3` bytes from `((a >> 4) | (b << 4)) + 1` back.
pub fn decompress(input: &[u8]) -> Option<Vec<u8>> {
    if input.len() < 2 || u16::from_le_bytes([input[0], input[1]]) as usize + 2 != input.len() {
        return None;
    }
    let mut out = Vec::with_capacity(input.len() * 2);
    let mut at = 2;
    while at < input.len() {
        let mut control = input[at];
        at += 1;
        for _ in 0..8 {
            if at >= input.len() {
                break;
            }
            if control & 0x80 != 0 {
                out.push(input[at]);
                at += 1;
            } else {
                let (a, b) = (*input.get(at)?, *input.get(at + 1)?);
                at += 2;
                let back = ((a >> 4) as usize | (b as usize) << 4) + 1;
                let len = (a & 0xF) as usize + 3;
                let from = out.len().checked_sub(back)?;
                for i in 0..len {
                    out.push(out[from + i]);
                }
            }
            control <<= 1;
        }
    }
    Some(out)
}

fn split_frames(area: &[u8], out: &mut Vec<Vec<u8>>) {
    let mut at = 0;
    while area.len() - at > FRAME_HEADER - 1 {
        let size = (u16::from_be_bytes([area[at], area[at + 1]]) >> 6) as usize;
        if size < FRAME_HEADER || at + size > area.len() {
            break;
        }
        out.push(area[at + FRAME_HEADER..at + size].to_vec());
        at += size;
    }
}

/// Frames of a packet (unencrypted). The first frame area may be compressed (flag 2).
pub fn frames(packet: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    if packet.len() < PACKET_HEADER + PACKET_MAC + 3 {
        return out;
    }
    let payload = &packet[PACKET_HEADER..packet.len() - PACKET_MAC];
    let split = u16::from_be_bytes([payload[0], payload[1]]) as usize;
    // Flag 1: not a frame packet.
    if payload[2] & 1 != 0 || split > payload.len() || split < 3 {
        return out;
    }
    if payload[2] & 2 != 0 {
        let Some(first) = decompress(&payload[3..split]) else { return out };
        split_frames(&first, &mut out);
    } else {
        split_frames(&payload[3..split], &mut out);
    }
    split_frames(&payload[split..], &mut out);
    out
}

/// A message found in a frame: its id and a reader positioned at its body.
pub struct Message<'a> {
    pub id: u32,
    pub body: BitReader<'a>,
}

/// Messages in a frame. Frames carry a small channel header before the message, so this looks
/// for the magic at every bit offset; callers validate the body.
pub fn messages(frame: &[u8]) -> Vec<Message<'_>> {
    let mut out = Vec::new();
    let bits = frame.len() * 8;
    let mut reader = BitReader::new(frame);
    let mut start = 0;
    while start + 23 <= bits {
        reader.seek(start);
        if reader.read(14) == Some(MAGIC) {
            let id = match reader.read(1) {
                Some(1) => reader.read(16),
                _ => reader.read(8),
            };
            if let Some(id) = id {
                out.push(Message { id, body: reader.clone() });
            }
        }
        start += 1;
    }
    out
}

/// One entity in a clone create (CMsgPackedReliables, first section; reader 0x497C50).
#[derive(Debug, Clone)]
pub struct CloneCreate {
    pub object_type: u8,
    pub object_id: u16,
    pub flags: u8,
    pub data_bits: usize,
    /// The create data, left-aligned.
    pub data: Vec<u8>,
}

/// CMsgPackedReliables (0x49A2D0): four optional sections, handled in this order by 0x492B70:
/// creates (0x497C50), create acks (0x497F40), removes (0x4981F0), remove acks (0x498310).
#[derive(Debug, Default)]
pub struct PackedReliables {
    pub creates: Vec<CloneCreate>,
    /// Object ids removed by their owner (14-bit id + 5-bit token each).
    pub removes: Vec<u16>,
    /// Item counts of the two ack sections (not decoded).
    pub acks: [u32; 2],
}

pub fn packed_reliables(mut body: BitReader<'_>) -> Option<PackedReliables> {
    let mut out = PackedReliables::default();
    let present = body.read(4)?;
    for section in 0..4 {
        if present & (1 << section) == 0 {
            continue;
        }
        let count = body.read(5)?;
        if count == 0 {
            continue;
        }
        let bits = body.read(10)? as usize;
        let mut sub = body.sub(bits)?;
        if section == 0 {
            // The game reads until fewer than 32 bits are left; the count bounds it too, which
            // keeps padding from being read as an extra entity.
            for _ in 0..count {
                if sub.remaining() <= 0x1F {
                    break;
                }
                let object_type = sub.read(4)? as u8;
                let object_id = sub.read(14)? as u16;
                let flags = sub.read(4)? as u8;
                let data_bits = sub.read(10)? as usize;
                if object_type as usize >= OBJECT_TYPES.len() {
                    break;
                }
                let mut data = vec![0u8; data_bits.div_ceil(8)];
                for (i, byte) in data.iter_mut().enumerate() {
                    let n = (data_bits - i * 8).min(8);
                    *byte = (sub.read(n)? << (8 - n)) as u8;
                }
                out.creates.push(CloneCreate { object_type, object_id, flags, data_bits, data });
            }
        } else if section == 2 {
            for _ in 0..count {
                if sub.remaining() < 14 {
                    break;
                }
                let object_id = sub.read(14)? as u16;
                sub.read(5)?;
                out.removes.push(object_id);
            }
        } else {
            out.acks[if section == 1 { 0 } else { 1 }] += count;
        }
    }
    Some(out)
}

/// Width of a clone sync entry's node mask by object type (0x494A20).
pub const NODE_MASK_BITS: [usize; 10] = [16, 15, 6, 14, 9, 13, 13, 13, 14, 14];

/// CMsgCloneSync (0x49A050): sequence, time, then the sync data. The data is a list of entries
/// (read by 0x492820): 4-bit object type, 14-bit object id, 1 bit, the node mask
/// ([`NODE_MASK_BITS`]) and the nodes whose mask bit is set, lowest bit first
/// ([`crate::nodes`]). Only the first entry is decoded: the next one starts after all the
/// nodes, and not every node's size is known.
pub struct CloneSync<'a> {
    pub sequence: u16,
    pub time: u32,
    pub data_bits: usize,
    pub object_type: u8,
    pub object_id: u16,
    pub mask: u32,
    /// Positioned at the first node.
    pub nodes: BitReader<'a>,
}

pub fn clone_sync(mut body: BitReader<'_>) -> Option<CloneSync<'_>> {
    let sequence = body.read(16)? as u16;
    let time = body.read(32)?;
    let data_bits = body.read(13)? as usize;
    let mut nodes = body.sub(data_bits)?;
    let object_type = nodes.read(4)? as u8;
    let object_id = nodes.read(14)? as u16;
    if object_type as usize >= OBJECT_TYPES.len() {
        return None;
    }
    nodes.read(1)?;
    let mask = nodes.read(NODE_MASK_BITS[object_type as usize])?;
    Some(CloneSync { sequence, time, data_bits, object_type, object_id, mask, nodes })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decompresses_literals_and_matches() {
        // "abcabcabc": three literals, then a match of 6 from 3 back.
        // control 0b1110_0000: literal, literal, literal, match.
        let body = [0b1110_0000, b'a', b'b', b'c', 0x23, 0x00];
        let mut input = (body.len() as u16).to_le_bytes().to_vec();
        input.extend_from_slice(&body);
        assert_eq!(decompress(&input).unwrap(), b"abcabcabc");
    }
}
