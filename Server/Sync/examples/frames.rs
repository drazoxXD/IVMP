//! Dumps the connection-layer fields of every frame and chunk in a capture, to check the frame
//! layout (docs/onesync-plan.md): `cargo run -p Sync --example frames -- capture.bin [count]`.

use ivmp_sync::bits::BitReader;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: frames <capture file> [count]");
    let count: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(40);
    let data = std::fs::read(&path).expect("read capture");
    let (mut at, mut n) = (0, 0);
    while at + 12 <= data.len() && n < count {
        let ms = u64::from_le_bytes(data[at..at + 8].try_into().unwrap());
        let (from, to) = (data[at + 8], data[at + 9]);
        let len = u16::from_le_bytes([data[at + 10], data[at + 11]]) as usize;
        let packet = &data[at + 12..at + 12 + len];
        at += 12 + len;
        n += 1;
        let payload = &packet[5..packet.len() - 4];
        println!("#{n} {:.3}s {from}->{to} flags {:02x} len {len}", ms as f64 / 1000.0, packet[0]);
        if packet[0] & 1 != 0 || payload.len() < 3 {
            println!("   (not a frame packet) {}", hex(payload));
            continue;
        }
        let split = (u16::from_be_bytes([payload[0], payload[1]]) as usize).min(payload.len());
        let first = if payload[2] & 2 != 0 {
            ivmp_sync::packet::decompress(&payload[3..split]).unwrap_or_default()
        } else {
            payload[3..split].to_vec()
        };
        let areas = [&first[..], &payload[split..]];
        for (i, area) in areas.iter().enumerate() {
            let mut a = 0;
            while area.len() >= a + 9 {
                let mut r = BitReader::new(&area[a..]);
                let size = r.read(10).unwrap() as usize;
                let oob = r.read(1).unwrap();
                let direct = r.read(1).unwrap();
                let has_ack = r.read(1).unwrap();
                let has_seq = r.read(1).unwrap();
                let channel = r.read(4).unwrap();
                let seq = r.read(16).unwrap();
                let ack = r.read(16).unwrap();
                let mask = r.read(22).unwrap();
                if size < 9 || a + size > area.len() {
                    break;
                }
                println!(
                    "   area{i} frame size {size} oob {oob} direct {direct} ack? {has_ack} seq? {has_seq} ch {channel} seq {seq} ack {ack} mask {mask:#08x}"
                );
                let body = &area[a + 9..a + size];
                let mut c = 0;
                while oob == 0 && body.len() >= c + 4 {
                    let mut r = BitReader::new(&body[c..]);
                    let csize = r.read(10).unwrap() as usize;
                    let reliable = r.read(1).unwrap();
                    let group = r.read(16).unwrap();
                    let cseq = if reliable == 1 { r.read(16).unwrap() } else { 0 };
                    let header = if reliable == 1 { 6 } else { 4 };
                    if csize < header || c + csize > body.len() {
                        println!("      bad chunk at {c}: {}", hex(&body[c..]));
                        break;
                    }
                    println!(
                        "      chunk size {csize} reliable {reliable} group {group} seq {cseq}: {}",
                        hex(&body[c + header..c + csize])
                    );
                    c += csize;
                }
                if oob == 1 {
                    println!("      oob body {}", hex(body));
                }
                a += size;
            }
        }
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().take(400).map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")
}
