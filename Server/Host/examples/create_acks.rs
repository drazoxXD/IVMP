//! Shows the create-ack sections of CMsgPackedReliables one client sent another:
//! `cargo run -p Host --example create_acks -- capture.bin <from> <to>`.

use ivmp_host::frame::Packet;
use ivmp_sync::packet;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap();
    let a: u8 = args.next().unwrap().parse().unwrap();
    let b: u8 = args.next().unwrap().parse().unwrap();
    let data = std::fs::read(&path).unwrap();
    let (mut at, mut shown) = (0, 0);
    while at + 12 <= data.len() && shown < 12 {
        let (from, to) = (data[at + 8], data[at + 9]);
        let len = u16::from_le_bytes([data[at + 10], data[at + 11]]) as usize;
        let p = data[at + 12..at + 12 + len].to_vec();
        at += 12 + len;
        if from != a || to != b {
            continue;
        }
        let Some(p) = Packet::parse(&p) else { continue };
        for f in p.first.iter().chain(&p.second) {
            for c in &f.chunks {
                for m in packet::messages(&c.data) {
                    if m.id != 0x8C {
                        continue;
                    }
                    let mut r = m.body.clone();
                    let mask = r.read(4).unwrap_or(0);
                    if mask & 0b1010 == 0 {
                        continue;
                    }
                    let mut out = format!("ch{} rel {} mask {mask:04b}:", f.channel, c.reliable);
                    for section in 0..4 {
                        if mask & (1 << section) == 0 {
                            continue;
                        }
                        let count = r.read(5).unwrap_or(0);
                        if count == 0 {
                            out += &format!(" s{section} empty");
                            continue;
                        }
                        let bits = r.read(10).unwrap_or(0) as usize;
                        let mut sub = r.sub(bits).unwrap();
                        let mut items = Vec::new();
                        if section == 1 || section == 3 {
                            while sub.remaining() >= 17 {
                                items.push(format!("{}:{}", sub.read(14).unwrap(), sub.read(3).unwrap()));
                            }
                        }
                        out += &format!(" s{section} count {count} bits {bits} [{}]", items.join(" "));
                    }
                    println!("{out}");
                    shown += 1;
                }
            }
        }
    }
}
