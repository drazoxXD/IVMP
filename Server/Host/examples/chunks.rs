//! Lists what one side sent (reliable chunks, out-of-band frames, raw packets) in a capture's
//! first seconds: `cargo run -p Host --example chunks -- capture.bin <from id> [seconds]`.

use ivmp_host::frame::Packet;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("capture");
    let who: u8 = args.next().and_then(|s| s.parse().ok()).expect("from id");
    let limit: f64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(3.0);
    let data = std::fs::read(&path).expect("read capture");
    let (mut at, mut start) = (0, None);
    while at + 12 <= data.len() {
        let ms = u64::from_le_bytes(data[at..at + 8].try_into().unwrap());
        let (from, to) = (data[at + 8], data[at + 9]);
        let len = u16::from_le_bytes([data[at + 10], data[at + 11]]) as usize;
        let packet = data[at + 12..at + 12 + len].to_vec();
        at += 12 + len;
        let start = *start.get_or_insert(ms);
        let t = (ms - start) as f64 / 1000.0;
        if t > limit {
            break;
        }
        if from != who {
            continue;
        }
        match Packet::parse(&packet) {
            Some(p) => {
                for f in p.first.iter().chain(&p.second) {
                    if f.out_of_band {
                        println!("{t:7.3} ->{to} oob ch{} {} bytes: {}", f.channel, f.body.len(), hex(&f.body));
                    }
                    for c in &f.chunks {
                        if c.data.is_empty() {
                            println!("{t:7.3} ->{to} ch{} SYN seq {} (frame has_seq {})", f.channel, c.seq, f.has_seq);
                        } else {
                            println!("{t:7.3} ->{to} ch{} seq {} rel {} tag {} {} bytes: {}", f.channel, c.seq, c.reliable, c.tag, c.data.len(), hex(&c.data));
                        }
                    }
                }
            }
            None => println!("{t:7.3} ->{to} raw {}", hex(&packet)),
        }
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().take(40).map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")
}
