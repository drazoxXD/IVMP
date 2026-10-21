//! The object type of every clone create in a capture, per sender: `cargo run -p Sync --example
//! create_types -- capture.bin` prints "from id type" once per (from, id, type).

use std::collections::BTreeSet;

use ivmp_sync::registry::Registry;

fn main() {
    let path = std::env::args().nth(1).expect("usage: create_types <capture file>");
    let data = std::fs::read(&path).expect("read capture");
    let mut registry = Registry::default();
    let mut seen = BTreeSet::new();
    let mut at = 0;
    while at + 12 <= data.len() {
        let from = data[at + 8];
        let len = u16::from_le_bytes([data[at + 10], data[at + 11]]) as usize;
        let Some(packet) = data.get(at + 12..at + 12 + len) else { break };
        for line in registry.observe(from, packet) {
            // "entity 556 AUTOMOBILE created by client 1"
            let words: Vec<&str> = line.split_whitespace().collect();
            if words.len() >= 4 && words[0] == "entity" && words[3] == "created" {
                if seen.insert((from, words[1].to_string(), words[2].to_string())) {
                    println!("{from} {} {}", words[1], words[2]);
                }
            }
        }
        at += 12 + len;
    }
}
