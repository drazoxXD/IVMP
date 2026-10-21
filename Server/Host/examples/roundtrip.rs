//! Parses every frame packet in a capture and encodes it again; reports packets whose frames
//! don't come out byte for byte the same (checks the frame layout):
//! `cargo run -p Host --example roundtrip -- capture.bin`.

use ivmp_host::frame::Packet;

fn main() {
    let path = std::env::args().nth(1).expect("usage: roundtrip <capture file>");
    let data = std::fs::read(&path).expect("read capture");
    let (mut at, mut ok, mut bad, mut skipped) = (0, 0, 0, 0);
    while at + 12 <= data.len() {
        let len = u16::from_le_bytes([data[at + 10], data[at + 11]]) as usize;
        let packet = &data[at + 12..at + 12 + len];
        at += 12 + len;
        let Some(parsed) = Packet::parse(packet) else {
            skipped += 1;
            continue;
        };
        // Compare frames, not bytes: originals may be compressed.
        let encoded = parsed.encode(0);
        if Packet::parse(&encoded) == Some(parsed.clone()) && (packet[5 + 2] & 2 != 0 || encoded[5..encoded.len() - 4] == packet[5..packet.len() - 4]) {
            ok += 1;
        } else {
            bad += 1;
            if bad < 5 {
                println!("mismatch: {:02x?}\n      vs: {:02x?}", &packet[5..packet.len() - 4], &encoded[5..encoded.len() - 4]);
            }
        }
    }
    println!("{ok} packets round-trip, {bad} don't, {skipped} not frame packets");
}
