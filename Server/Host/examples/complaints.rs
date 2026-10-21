//! Lists the netComplaintMsg (0x55) in a capture: who complains about which peers, and when.
//! Body (writer 0x6D7D30, reader 0x6D7DE0): complainer peer id (2 x u32), u32 count (<= 64),
//! then the peer ids complained about (2 x u32 each).
//! `cargo run -p Host --example complaints -- capture.bin`.

use std::collections::BTreeMap;

use ivmp_host::frame::Packet;
use ivmp_sync::packet;

const MSG_COMPLAINT: u32 = 0x55;

fn main() {
    let path = std::env::args().nth(1).expect("capture");
    let data = std::fs::read(&path).expect("read capture");
    let mut counts: BTreeMap<(u8, u8, String), u32> = BTreeMap::new();
    let mut at = 0;
    while at + 12 <= data.len() {
        let ms = u64::from_le_bytes(data[at..at + 8].try_into().unwrap());
        let (from, to) = (data[at + 8], data[at + 9]);
        let len = u16::from_le_bytes([data[at + 10], data[at + 11]]) as usize;
        let p = data[at + 12..at + 12 + len].to_vec();
        at += 12 + len;
        let Some(p) = Packet::parse(&p) else { continue };
        for f in p.first.iter().chain(&p.second) {
            let bodies: Vec<(bool, &[u8])> = if f.out_of_band {
                vec![(false, &f.body[..])]
            } else {
                f.chunks.iter().map(|c| (c.reliable, &c.data[..])).collect()
            };
            for (reliable, b) in bodies {
                for m in packet::messages(b).into_iter().filter(|m| m.id == MSG_COMPLAINT) {
                    let mut r = m.body.clone();
                    let mut peer = || Some(format!("{:08x}:{:08x}", r.read(32)?, r.read(32)?));
                    let Some(complainer) = peer() else { continue };
                    let mut r2 = m.body.clone();
                    r2.skip(64);
                    let Some(count) = r2.read(32) else { continue };
                    let mut about = Vec::new();
                    for _ in 0..count.min(64) {
                        let (Some(a), Some(b)) = (r2.read(32), r2.read(32)) else { break };
                        about.push(format!("{a:08x}:{b:08x}"));
                    }
                    let about = about.join(",");
                    println!(
                        "{ms:>8} ms {from:>3} -> {to:>3} ch{} {}: {complainer} complains about [{about}]",
                        f.channel,
                        if reliable { "reliable" } else { "unreliable" }
                    );
                    *counts.entry((from, to, about)).or_default() += 1;
                }
            }
        }
    }
    println!();
    for ((from, to, about), n) in counts {
        println!("{from} -> {to}: [{about}] x{n}");
    }
}
