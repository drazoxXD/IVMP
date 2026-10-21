//! The engine's connection layer (rage::netConnection, GTA IV 1.2.0.59).
//!
//! * packet: `[flags u8][nonce u32][payload][mac u32]` (send 0x6D2020). Flag 1 = not frames (key
//!   exchange). IVMP clients don't encrypt (crypt.rs) or check the MAC, so ours is zero.
//! * payload: big-endian u16 offset of the second area, a flags byte (2 = first area LZSS
//!   compressed), then two areas of frames.
//! * frame (receive 0x6CF560): 9-byte header, MSB first: size 10 | out of band 1 | direct 1 |
//!   has ack 1 | has seq 1 | channel 4 | seq 16 | ack 16 | 22 bits (zero). `ack` = the next
//!   reliable sequence expected on that channel. An out-of-band frame holds one message.
//! * chunk (inside a connection frame, 0x6F60B0): size 10 | reliable 1 | previous seq 16 |
//!   seq 16 (reliable only) | 5 bits (13 so far): 6 bytes (4 when unreliable); then messages.

use ivmp_sync::bits::BitReader;

use crate::bits::BitWriter;

pub const FRAME_HEADER: usize = 9;
pub const RELIABLE_CHUNK_HEADER: usize = 6;
pub const CHUNK_HEADER: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Chunk {
    pub reliable: bool,
    /// The sequence before this one (`seq - 1` in every capture).
    pub previous: u16,
    pub seq: u16,
    /// The 5 bits after the sequence numbers (unknown; 13 in every capture so far).
    pub tag: u8,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Frame {
    pub out_of_band: bool,
    pub direct: bool,
    pub channel: u8,
    pub has_seq: bool,
    pub seq: u16,
    /// Next reliable sequence we expect from the other side on this channel.
    pub ack: Option<u16>,
    pub tail: u32,
    /// Out-of-band frames: the message bytes.
    pub body: Vec<u8>,
    /// Connection frames: the chunks.
    pub chunks: Vec<Chunk>,
}

impl Frame {
    pub fn encode(&self, out: &mut Vec<u8>) {
        let body = if self.out_of_band {
            self.body.clone()
        } else {
            let mut body = Vec::new();
            for chunk in &self.chunks {
                let header = if chunk.reliable { RELIABLE_CHUNK_HEADER } else { CHUNK_HEADER };
                let mut w = BitWriter::new();
                w.write((header + chunk.data.len()) as u32, 10)
                    .write(chunk.reliable as u32, 1)
                    .write(chunk.previous as u32, 16);
                if chunk.reliable {
                    w.write(chunk.seq as u32, 16);
                }
                w.write(chunk.tag as u32, 5);
                let mut bytes = w.finish();
                bytes.resize(header, 0);
                body.extend_from_slice(&bytes);
                body.extend_from_slice(&chunk.data);
            }
            body
        };
        let mut w = BitWriter::new();
        w.write((FRAME_HEADER + body.len()) as u32, 10)
            .write(self.out_of_band as u32, 1)
            .write(self.direct as u32, 1)
            .write(self.ack.is_some() as u32, 1)
            .write(self.has_seq as u32, 1)
            .write(self.channel as u32, 4)
            .write(self.seq as u32, 16)
            .write(self.ack.unwrap_or(0) as u32, 16)
            .write(self.tail, 22);
        out.extend_from_slice(&w.finish());
        out.extend_from_slice(&body);
    }

    /// Parses the frames of one area.
    pub fn parse_area(area: &[u8]) -> Option<Vec<Frame>> {
        let mut frames = Vec::new();
        let mut at = 0;
        while area.len() > at {
            let mut r = BitReader::new(&area[at..]);
            let size = r.read(10)? as usize;
            if size < FRAME_HEADER || at + size > area.len() {
                return None;
            }
            let out_of_band = r.read(1)? != 0;
            let direct = r.read(1)? != 0;
            let has_ack = r.read(1)? != 0;
            let has_seq = r.read(1)? != 0;
            let channel = r.read(4)? as u8;
            let seq = r.read(16)? as u16;
            let ack = r.read(16)? as u16;
            let tail = r.read(22)?;
            let body = &area[at + FRAME_HEADER..at + size];
            let mut frame = Frame {
                out_of_band,
                direct,
                channel,
                has_seq,
                seq,
                ack: has_ack.then_some(ack),
                tail,
                ..Frame::default()
            };
            if out_of_band {
                frame.body = body.to_vec();
            } else {
                let mut c = 0;
                while c < body.len() {
                    let mut r = BitReader::new(&body[c..]);
                    let size = r.read(10)? as usize;
                    let reliable = r.read(1)? != 0;
                    let previous = r.read(16)? as u16;
                    let seq = if reliable { r.read(16)? as u16 } else { 0 };
                    let tag = r.read(5)? as u8;
                    let header = if reliable { RELIABLE_CHUNK_HEADER } else { CHUNK_HEADER };
                    if size < header || c + size > body.len() {
                        return None;
                    }
                    frame.chunks.push(Chunk { reliable, previous, seq, tag, data: body[c + header..c + size].to_vec() });
                    c += size;
                }
            }
            frames.push(frame);
            at += size;
        }
        Some(frames)
    }
}

/// A packet's frames: the first area (may arrive compressed) and the second.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Packet {
    pub first: Vec<Frame>,
    pub second: Vec<Frame>,
}

impl Packet {
    /// The payload (between nonce and MAC), uncompressed.
    pub fn encode_payload(&self) -> Vec<u8> {
        let mut first = Vec::new();
        for frame in &self.first {
            frame.encode(&mut first);
        }
        let split = 3 + first.len();
        let mut out = Vec::with_capacity(split + 64);
        out.extend_from_slice(&(split as u16).to_be_bytes());
        out.push(0);
        out.extend_from_slice(&first);
        for frame in &self.second {
            frame.encode(&mut out);
        }
        out
    }

    /// The whole packet: flags 0, `nonce`, payload, zero MAC.
    pub fn encode(&self, nonce: u32) -> Vec<u8> {
        let payload = self.encode_payload();
        let mut out = Vec::with_capacity(payload.len() + 9);
        out.push(0);
        out.extend_from_slice(&nonce.to_le_bytes());
        out.extend_from_slice(&payload);
        out.extend_from_slice(&[0; 4]);
        out
    }

    /// Parses a frame packet (flags bit 1 clear).
    pub fn parse(packet: &[u8]) -> Option<Packet> {
        if packet.len() < 12 || packet[0] & 1 != 0 {
            return None;
        }
        let payload = &packet[5..packet.len() - 4];
        let split = u16::from_be_bytes([payload[0], payload[1]]) as usize;
        if split < 3 || split > payload.len() {
            return None;
        }
        let first = if payload[2] & 2 != 0 {
            ivmp_sync::packet::decompress(&payload[3..split])?
        } else {
            payload[3..split].to_vec()
        };
        Some(Packet { first: Frame::parse_area(&first)?, second: Frame::parse_area(&payload[split..])? })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let frame = Frame {
            channel: 13,
            has_seq: true,
            ack: Some(52429),
            chunks: vec![Chunk { reliable: true, previous: 39004, seq: 39005, tag: 13, data: vec![] }],
            ..Frame::default()
        };
        let mut bytes = Vec::new();
        frame.encode(&mut bytes);
        assert_eq!(Frame::parse_area(&bytes), Some(vec![frame]));
        // An ack-only frame from a capture: size 9, has ack, channel 13, ack 39006.
        let ack = [0x02, 0x4b, 0x40, 0x00, 0x26, 0x17, 0x80, 0x00, 0x00];
        let parsed = Frame::parse_area(&ack).unwrap();
        assert_eq!(parsed[0].ack, Some(39006));
        assert_eq!(parsed[0].channel, 13);
        let mut again = Vec::new();
        parsed[0].encode(&mut again);
        assert_eq!(again, ack);
    }
}
