//! Counts the message ids clients sent to the server host (254) in reliable chunks:
//! `cargo run -p Host --example to_host -- capture.bin`.

use std::collections::BTreeMap;

use ivmp_host::frame::Packet;
use ivmp_sync::packet;

fn main() {
    let path = std::env::args().nth(1).expect("capture");
    let data = std::fs::read(&path).expect("read capture");
    let mut counts: BTreeMap<(u8, u32), u32> = BTreeMap::new();
    let mut at = 0;
    while at + 12 <= data.len() {
        let (_from, to) = (data[at + 8], data[at + 9]);
        let len = u16::from_le_bytes([data[at + 10], data[at + 11]]) as usize;
        let p = data[at + 12..at + 12 + len].to_vec();
        at += 12 + len;
        if to != 254 {
            continue;
        }
        let Some(p) = Packet::parse(&p) else { continue };
        for f in p.first.iter().chain(&p.second) {
            let bodies: Vec<&[u8]> = if f.out_of_band { vec![&f.body] } else { f.chunks.iter().map(|c| &c.data[..]).collect() };
            for b in bodies {
                if let Some(m) = packet::messages(b).first() {
                    *counts.entry((f.channel, m.id)).or_default() += 1;
                }
            }
        }
    }
    for ((ch, id), n) in counts {
        println!("ch{ch} id {id:#x}: {n}");
    }
}
