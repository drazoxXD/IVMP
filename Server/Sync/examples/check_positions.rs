//! Checks decoded positions in a capture: for every object type, how many creates and syncs
//! decode to a position on the map. `cargo run --example check_positions -- capture.bin`.

use std::collections::BTreeMap;

use ivmp_sync::nodes;
use ivmp_sync::packet::{self, OBJECT_TYPES, id};
use ivmp_sync::registry::Registry;

fn on_map(p: [f32; 3]) -> bool {
    p[0].abs() < 3000.0 && p[1].abs() < 3000.0 && (-100.0..1500.0).contains(&p[2])
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: check_positions <capture file>");
    let data = std::fs::read(&path).expect("read capture");
    let mut registry = Registry::default();
    // type -> (creates, with position, on map)
    let mut creates: BTreeMap<u8, [u32; 3]> = BTreeMap::new();
    let mut syncs: BTreeMap<u8, [u32; 3]> = BTreeMap::new();
    let mut at = 0;
    while at + 12 <= data.len() {
        let from = data[at + 8];
        let len = u16::from_le_bytes([data[at + 10], data[at + 11]]) as usize;
        let Some(p) = data.get(at + 12..at + 12 + len) else { break };
        for frame in packet::frames(p) {
            for m in packet::messages(&frame) {
                match m.id {
                    id::PACKED_RELIABLES => {
                        let Some(r) = packet::packed_reliables(m.body) else { continue };
                        for c in &r.creates {
                            let n = creates.entry(c.object_type).or_default();
                            n[0] += 1;
                            if let Some(pl) = nodes::create_placement(c) {
                                if let Some(pos) = pl.world.or(nodes::world_position(pl.sector, pl.local)) {
                                    n[1] += 1;
                                    n[2] += on_map(pos) as u32;
                                }
                            }
                        }
                    }
                    id::CLONE_SYNC => {
                        let Some(mut s) = packet::clone_sync(m.body.clone()) else { continue };
                        let Some(e) = registry.entities.get(&s.object_id) else { continue };
                        let script = e.create.flags & nodes::SCRIPT_FLAG != 0;
                        if s.mask & 0b1100 != 0b1100 {
                            continue;
                        }
                        let n = syncs.entry(s.object_type).or_default();
                        n[0] += 1;
                        if let Some(pl) = nodes::read_nodes(&mut s.nodes, s.object_type, script, s.mask & 0b1111) {
                            if let Some(pos) = nodes::world_position(pl.sector, pl.local) {
                                n[1] += 1;
                                n[2] += on_map(pos) as u32;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        registry.observe(from, p);
        at += 12 + len;
    }
    for (kind, [n, placed, ok]) in creates {
        println!("create {:10} {n:6} decoded {placed:6} on map {ok:6}", OBJECT_TYPES[kind as usize]);
    }
    for (kind, [n, placed, ok]) in syncs {
        println!("sync   {:10} {n:6} decoded {placed:6} on map {ok:6} (with sector and position)", OBJECT_TYPES[kind as usize]);
    }
    println!("{}", registry.summary());
}
