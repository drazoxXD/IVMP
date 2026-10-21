//! Counts the clone creates (CMsgPackedReliables section 0) each client sent each other, per
//! object: `cargo run -p Host --example creates -- capture.bin [from] [to]`.

use std::collections::BTreeMap;

use ivmp_host::frame::Packet;
use ivmp_sync::packet;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("capture");
    let want_from: Option<u8> = args.next().map(|a| a.parse().unwrap());
    let want_to: Option<u8> = args.next().map(|a| a.parse().unwrap());
    let data = std::fs::read(&path).expect("read capture");
    // (from, to, object id, type) -> (count, first ms, last ms)
    let mut creates: BTreeMap<(u8, u8, u16, u8), (u32, u64, u64)> = BTreeMap::new();
    let mut at = 0;
    while at + 12 <= data.len() {
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
                let Some(m) = all.iter().find(|m| m.body.position() == 23 && m.id == 0x8C) else { continue };
                let Some(reliables) = packet::packed_reliables(m.body.clone()) else { continue };
                for create in &reliables.creates {
                    let e = creates.entry((from, to, create.object_id, create.object_type)).or_insert((0, ms, ms));
                    e.0 += 1;
                    e.2 = ms;
                }
            }
        }
    }
    let mut rows: Vec<_> = creates.into_iter().collect();
    rows.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
    for ((from, to, id, kind), (n, first, last)) in rows.iter().take(25) {
        println!(
            "{from} -> {to}: object {id} {} x{n} ({:.1}s .. {:.1}s)",
            packet::OBJECT_TYPES.get(*kind as usize).unwrap_or(&"?"),
            *first as f64 / 1000.0,
            *last as f64 / 1000.0
        );
    }
}
