//! Prints where one entity was over a capture (every ~5 s):
//! `cargo run -p Sync --example track -- capture.bin <object id>`.

use ivmp_sync::registry::Registry;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: track <capture file> <object id>");
    let object_id: u16 = args.next().and_then(|a| a.parse().ok()).expect("object id");
    let data = std::fs::read(&path).expect("read capture");
    let mut registry = Registry::default();
    let (mut at, mut last_ms, mut last) = (0, 0u64, None);
    while at + 12 <= data.len() {
        let ms = u64::from_le_bytes(data[at..at + 8].try_into().unwrap());
        let from = data[at + 8];
        let len = u16::from_le_bytes([data[at + 10], data[at + 11]]) as usize;
        let Some(packet) = data.get(at + 12..at + 12 + len) else { break };
        registry.observe(from, packet);
        let position = registry.entities.get(&object_id).and_then(|e| e.position);
        if position.is_some() && position != last && ms >= last_ms + 5000 {
            let [x, y, z] = position.unwrap();
            println!("{:6.1}s  {x:8.1} {y:8.1} {z:6.1}", ms as f64 / 1000.0);
            last_ms = ms;
            last = position;
        }
        at += 12 + len;
    }
}
