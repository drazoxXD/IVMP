//! The registry's lines (creates, removes, owner changes) about one object:
//! `cargo run -p Sync --example entity_log -- capture.bin <object id>`.

use ivmp_sync::registry::Registry;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: entity_log <capture file> <object id>");
    let id = args.next().expect("object id");
    let data = std::fs::read(&path).expect("read capture");
    let mut registry = Registry::default();
    let mut at = 0;
    let needle = format!("entity {id} ");
    while at + 12 <= data.len() {
        let ms = u64::from_le_bytes(data[at..at + 8].try_into().unwrap());
        let (from, to) = (data[at + 8], data[at + 9]);
        let len = u16::from_le_bytes([data[at + 10], data[at + 11]]) as usize;
        let Some(packet) = data.get(at + 12..at + 12 + len) else { break };
        for line in registry.observe(from, packet) {
            if line.contains(&needle) {
                println!("{:>8.1}s {from} -> {to}: {line}", ms as f64 / 1000.0);
            }
        }
        at += 12 + len;
    }
}
