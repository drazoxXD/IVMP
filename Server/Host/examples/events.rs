//! Dumps CMsgPackedEvents (0x4F) and CMsgPackedEventReliablesMsgs (0x89) in a capture.
//! 0x4F (writer 0x4AC010): 5 bits, 15-bit data length in bits, data. 0x89 (0x4B18C0): 2-bit
//! mask, then for bit 0 and bit 1 one block shaped like 0x4F.
//! `cargo run -p Host --example events -- capture.bin [from] [to] [limit]`.

use ivmp_host::frame::Packet;
use ivmp_sync::bits::BitReader;
use ivmp_sync::packet;

fn block(r: &mut BitReader<'_>) -> Option<String> {
    let head = r.read(5)?;
    let len = r.read(15)? as usize;
    let mut bits = String::new();
    for _ in 0..len {
        bits.push(if r.read(1)? == 1 { '1' } else { '0' });
    }
    Some(format!("[{head} | {len}b {bits}]"))
}

/// The events in a 0x4F block (reader 0x4AEFB0): 5-bit type, 15-bit id (low 7 bits number,
/// rest sequence), 15-bit data length, a flag and 16 more bits if set, then the data.
fn events(r: &mut BitReader<'_>) -> Option<String> {
    r.read(5)?;
    let len = r.read(15)? as usize;
    let mut block = r.sub(len)?;
    let mut out = Vec::new();
    while block.remaining() >= 35 {
        let kind = block.read(5)?;
        let id = block.read(15)?;
        let data = block.read(15)? as usize;
        let extra = if block.read(1)? == 1 { format!(" +{:#x}", block.read(16)?) } else { String::new() };
        block.skip(data)?;
        out.push(format!("type {kind} id {}_{}{extra} {data}b", id & 0x7F, id >> 7));
    }
    Some(out.join(", "))
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("capture");
    let want_from: Option<u8> = args.next().map(|a| a.parse().unwrap());
    let want_to: Option<u8> = args.next().map(|a| a.parse().unwrap());
    let mut limit: usize = args.next().map_or(60, |a| a.parse().unwrap());
    let data = std::fs::read(&path).expect("read capture");
    let mut at = 0;
    while at + 12 <= data.len() && limit > 0 {
        let ms = u64::from_le_bytes(data[at..at + 8].try_into().unwrap());
        let (from, to) = (data[at + 8], data[at + 9]);
        let len = u16::from_le_bytes([data[at + 10], data[at + 11]]) as usize;
        let p = data[at + 12..at + 12 + len].to_vec();
        at += 12 + len;
        if want_from.is_some_and(|f| f != from) || want_to.is_some_and(|t| t != to) {
            continue;
        }
        let Some(p) = Packet::parse(&p) else { continue };
        for f in p.first.iter().chain(&p.second).filter(|f| !f.out_of_band) {
            for c in &f.chunks {
                let all = packet::messages(&c.data);
                let Some(m) = all.iter().find(|m| m.body.position() == 23) else { continue };
                let mut r = m.body.clone();
                let text = match m.id {
                    0x4F => events(&mut r.clone()).zip(block(&mut r)).map(|(e, b)| format!("{e} {b}")),
                    0x89 => (|| {
                        let mask = r.read(2)?;
                        let mut s = format!("mask {mask}");
                        for bit in [1, 2] {
                            if mask & bit != 0 {
                                s += &format!(" {}:{}", if bit == 1 { "events" } else { "replies" }, block(&mut r)?);
                            }
                        }
                        Some(s)
                    })(),
                    _ => continue,
                };
                println!(
                    "{ms:>8} {from:>3}->{to:<3} ch{} {} {:#x} {}",
                    f.channel,
                    if c.reliable { format!("R{}", c.seq) } else { "u".into() },
                    m.id,
                    text.unwrap_or("?".into())
                );
                limit -= 1;
            }
        }
    }
}
