//! Dumps CMsgPackedCloneSyncAcks (0x8B) bodies between two clients, listing the entries that
//! failed (16-bit sequence, 8-bit count, per item 14-bit object id + 3-bit result; 0x4EDDC0): `cargo run -p Host --example sync_acks -- capture.bin <from> <to>`.

use std::collections::BTreeMap;

use ivmp_host::frame::Packet;
use ivmp_sync::packet;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("capture");
    let from_id: u8 = args.next().expect("from").parse().unwrap();
    let to_id: u8 = args.next().expect("to").parse().unwrap();
    let data = std::fs::read(&path).expect("read capture");
    let mut patterns: BTreeMap<String, u32> = BTreeMap::new();
    let mut at = 0;
    while at + 12 <= data.len() {
        let (from, to) = (data[at + 8], data[at + 9]);
        let len = u16::from_le_bytes([data[at + 10], data[at + 11]]) as usize;
        let p = data[at + 12..at + 12 + len].to_vec();
        at += 12 + len;
        if (from, to) != (from_id, to_id) {
            continue;
        }
        let Some(p) = Packet::parse(&p) else { continue };
        for f in p.first.iter().chain(&p.second).filter(|f| !f.out_of_band) {
            for c in &f.chunks {
                let all = packet::messages(&c.data);
                let Some(m) = all.iter().find(|m| m.body.position() == 23 && m.id == 0x8B) else { continue };
                // Writer 0x4EDDC0: items only for failed entries (result 5 = applied, no item).
                let mut r = m.body.clone();
                let (Some(_sequence), Some(count)) = (r.read(16), r.read(8)) else { continue };
                if count == 0 {
                    *patterns.entry("none".into()).or_default() += 1;
                }
                for _ in 0..count {
                    let (Some(id), Some(result)) = (r.read(14), r.read(3)) else { break };
                    *patterns.entry(format!("object {id} result {result}")).or_default() += 1;
                }
            }
        }
    }
    let mut rows: Vec<_> = patterns.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    for (bits, n) in rows.iter().take(20) {
        println!("{n:>6}  {bits}");
    }
}
