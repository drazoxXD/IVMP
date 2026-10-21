//! Compares what the host sent during the join in two captures (same host id, different joiner
//! ids), message by message, to find the bytes that depend on the joiner:
//! `cargo run -p Host --example diff -- a.bin b.bin [host id]`.

use ivmp_host::frame::Packet;

/// Messages the host sent in the first seconds: (channel, out of band, reliable chunk index,
/// bytes). Packets that aren't frames (key exchange, pings) are included raw.
fn host_messages(path: &str, host: u8) -> Vec<(String, Vec<u8>)> {
    let data = std::fs::read(path).expect("read capture");
    let mut out = Vec::new();
    let (mut at, mut start) = (0, None);
    while at + 12 <= data.len() {
        let ms = u64::from_le_bytes(data[at..at + 8].try_into().unwrap());
        let from = data[at + 8];
        let len = u16::from_le_bytes([data[at + 10], data[at + 11]]) as usize;
        let packet = data[at + 12..at + 12 + len].to_vec();
        at += 12 + len;
        let start = *start.get_or_insert(ms);
        if ms - start > 1500 {
            break;
        }
        if from != host {
            continue;
        }
        match Packet::parse(&packet) {
            Some(p) => {
                for frame in p.first.iter().chain(&p.second) {
                    if frame.out_of_band {
                        out.push((format!("oob ch{}", frame.channel), frame.body.clone()));
                    }
                    for chunk in &frame.chunks {
                        if !chunk.data.is_empty() {
                            out.push((format!("ch{} chunk", frame.channel), chunk.data.clone()));
                        }
                    }
                }
            }
            None => out.push((format!("raw flags {:02x}", packet[0]), packet[5..packet.len() - 4].to_vec())),
        }
    }
    out
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (a, b) = (args.next().expect("a.bin"), args.next().expect("b.bin"));
    let host: u8 = args.next().and_then(|s| s.parse().ok()).unwrap_or(254);
    let (ma, mb) = (host_messages(&a, host), host_messages(&b, host));
    for (i, ((ka, da), (kb, db))) in ma.iter().zip(&mb).enumerate() {
        let same = da == db;
        println!("#{i} {ka} / {kb}: {} bytes / {} bytes {}", da.len(), db.len(), if same { "same" } else { "DIFFERENT" });
        if !same {
            let n = da.len().max(db.len());
            let diffs: Vec<String> = (0..n)
                .filter(|j| da.get(*j) != db.get(*j))
                .map(|j| format!("@{j}: {:02x?} {:02x?}", da.get(j), db.get(j)))
                .collect();
            println!("   {}", diffs.iter().take(24).cloned().collect::<Vec<_>>().join("  "));
        }
    }
}
