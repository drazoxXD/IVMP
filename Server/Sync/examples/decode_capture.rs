//! Runs a server capture (`Server --capture file`) through the decoder:
//! `cargo run --example decode_capture -- capture.bin`.

use ivmp_sync::registry::Registry;

fn main() {
    let path = std::env::args().nth(1).expect("usage: decode_capture <capture file>");
    let data = std::fs::read(&path).expect("read capture");
    let mut registry = Registry::default();
    let mut at = 0;
    let mut events = 0;
    while at + 12 <= data.len() {
        let from = data[at + 8];
        let len = u16::from_le_bytes([data[at + 10], data[at + 11]]) as usize;
        let Some(packet) = data.get(at + 12..at + 12 + len) else { break };
        for line in registry.observe(from, packet) {
            if events < 40 {
                println!("{line}");
            }
            events += 1;
        }
        at += 12 + len;
    }
    println!("{events} events");
    println!("{}", registry.summary());
}
