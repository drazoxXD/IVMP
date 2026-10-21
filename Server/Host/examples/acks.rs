//! Shows clone syncs one client sent and the sync acks it got back (first ones):
//! `cargo run -p Host --example acks -- capture.bin <from> <to>`.

use ivmp_host::frame::Packet;
use ivmp_sync::bits::BitReader;
use ivmp_sync::packet;

fn bits(data: &[u8], start: usize, n: usize) -> String {
    let mut r = BitReader::new(data);
    r.seek(start);
    (0..n).map(|_| char::from(b'0' + r.read(1).unwrap_or(0) as u8)).collect()
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap();
    let a: u8 = args.next().unwrap().parse().unwrap();
    let b: u8 = args.next().unwrap().parse().unwrap();
    let data = std::fs::read(&path).unwrap();
    let (mut at, mut shown) = (0, 0);
    while at + 12 <= data.len() && shown < 40 {
        let ms = u64::from_le_bytes(data[at..at + 8].try_into().unwrap());
        let (from, to) = (data[at + 8], data[at + 9]);
        let len = u16::from_le_bytes([data[at + 10], data[at + 11]]) as usize;
        let p = data[at + 12..at + 12 + len].to_vec();
        at += 12 + len;
        if !((from == a && to == b) || (from == b && to == a)) {
            continue;
        }
        let Some(p) = Packet::parse(&p) else { continue };
        for f in p.first.iter().chain(&p.second) {
            for c in &f.chunks {
                for m in packet::messages(&c.data) {
                    let pos = m.body.position();
                    match m.id {
                        0x8A if from == a => {
                            let mut r = m.body.clone();
                            println!("{ms} {from}->{to} sync seq {} (ch{} chunk rel {})", r.read(16).unwrap_or(0), f.channel, c.reliable);
                            shown += 1;
                        }
                        0x8B if from == b => {
                            println!("{ms} {from}->{to} SYNC ACK (rel {}): {}", c.reliable, bits(&c.data, pos, (c.data.len() * 8 - pos).min(120)));
                            shown += 1;
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}
