//! What the games sent each other and the server: message counts per direction, and clone syncs
//! per object, in time buckets.
//! `cargo run -p Host --example syncs -- capture.bin [bucket seconds, default 20] [object id]`.

use std::collections::BTreeMap;

use ivmp_host::frame::Packet;
use ivmp_sync::packet;

const MSG_CLONE_SYNC: u32 = 0x8A;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("capture");
    let bucket: u64 = args.next().map_or(20, |a| a.parse().unwrap()) * 1000;
    let only: Option<u16> = args.next().map(|a| a.parse().unwrap());
    let data = std::fs::read(&path).expect("read capture");
    // (bucket, from, to) -> message id -> count; (bucket, from, to) -> object -> syncs
    let mut messages: BTreeMap<(u64, u8, u8), BTreeMap<u32, u32>> = BTreeMap::new();
    let mut syncs: BTreeMap<(u64, u8, u8), BTreeMap<(u16, &str), u32>> = BTreeMap::new();
    let mut at = 0;
    while at + 12 <= data.len() {
        let ms = u64::from_le_bytes(data[at..at + 8].try_into().unwrap());
        let (from, to) = (data[at + 8], data[at + 9]);
        let len = u16::from_le_bytes([data[at + 10], data[at + 11]]) as usize;
        let p = data[at + 12..at + 12 + len].to_vec();
        at += 12 + len;
        let Some(p) = Packet::parse(&p) else { continue };
        let key = (ms / bucket, from, to);
        for f in p.first.iter().chain(&p.second).filter(|f| !f.out_of_band) {
            for c in &f.chunks {
                // Messages start at the chunk's first bit; later finds are part of their data.
                let all = packet::messages(&c.data);
                let Some(m) = all.iter().find(|m| m.body.position() == 23) else { continue };
                *messages.entry(key).or_default().entry(m.id).or_default() += 1;
                if m.id == MSG_CLONE_SYNC {
                    if let Some(s) = packet::clone_sync(m.body.clone()) {
                        if only.is_none_or(|o| o == s.object_id) {
                            let name = packet::OBJECT_TYPES[s.object_type as usize];
                            *syncs.entry(key).or_default().entry((s.object_id, name)).or_default() += 1;
                        }
                    }
                }
            }
        }
    }
    for (key, ids) in &messages {
        let ids: Vec<String> = ids.iter().map(|(id, n)| format!("{id:#x}x{n}")).collect();
        println!("{:>4}s {:>3} -> {:<3} {}", key.0 * bucket / 1000, key.1, key.2, ids.join(" "));
        if let Some(objects) = syncs.get(key) {
            let objects: Vec<String> = objects.iter().map(|((id, name), n)| format!("{id} {name} x{n}")).collect();
            println!("        syncs: {}", objects.join(", "));
        }
    }
}
