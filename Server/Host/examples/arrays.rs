//! Dumps CMsgArrayElements (0x86: 13-bit data length, data) and CMsgArrayElementsAck (0x87:
//! 3 bits, 6 bits, 16 bits; writer 0x4B8940) in a capture.
//! `cargo run -p Host --example arrays -- capture.bin [limit]`.

use ivmp_host::frame::Packet;
use ivmp_sync::packet;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("capture");
    let mut limit: usize = args.next().map_or(60, |a| a.parse().unwrap());
    let data = std::fs::read(&path).expect("read capture");
    let mut at = 0;
    while at + 12 <= data.len() && limit > 0 {
        let ms = u64::from_le_bytes(data[at..at + 8].try_into().unwrap());
        let (from, to) = (data[at + 8], data[at + 9]);
        let len = u16::from_le_bytes([data[at + 10], data[at + 11]]) as usize;
        let p = data[at + 12..at + 12 + len].to_vec();
        at += 12 + len;
        let Some(p) = Packet::parse(&p) else { continue };
        for f in p.first.iter().chain(&p.second).filter(|f| !f.out_of_band) {
            for c in &f.chunks {
                let all = packet::messages(&c.data);
                let Some(m) = all.iter().find(|m| m.body.position() == 23) else { continue };
                let mut r = m.body.clone();
                let text = match m.id {
                    0x86 => (|| {
                        let n = r.read(13)? as usize;
                        let mut bits = String::new();
                        for _ in 0..n.min(120) {
                            bits.push(if r.read(1)? == 1 { '1' } else { '0' });
                        }
                        Some(format!("{n}b {bits}"))
                    })(),
                    0x87 => (|| Some(format!("{} {} {}", r.read(3)?, r.read(6)?, r.read(16)?)))(),
                    _ => continue,
                };
                let kind = if c.reliable { format!("R{}", c.seq) } else { "u".into() };
                println!("{ms:>8} {from:>3}->{to:<3} ch{} {kind} {:#x} {}", f.channel, m.id, text.unwrap_or("?".into()));
                limit -= 1;
            }
        }
    }
}
