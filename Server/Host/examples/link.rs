//! Frames between one client and the server host in a time window: channel, ack, chunks.
//! `cargo run -p Host --example link -- capture.bin <client> <from ms> <to ms>`.

use ivmp_host::frame::Packet;
use ivmp_sync::packet;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("capture");
    let client: u8 = args.next().expect("client").parse().unwrap();
    let from_ms: u64 = args.next().map_or(0, |a| a.parse().unwrap());
    let to_ms: u64 = args.next().map_or(u64::MAX, |a| a.parse().unwrap());
    let data = std::fs::read(&path).expect("read capture");
    let mut at = 0;
    while at + 12 <= data.len() {
        let ms = u64::from_le_bytes(data[at..at + 8].try_into().unwrap());
        let (from, to) = (data[at + 8], data[at + 9]);
        let len = u16::from_le_bytes([data[at + 10], data[at + 11]]) as usize;
        let p = data[at + 12..at + 12 + len].to_vec();
        at += 12 + len;
        if ms < from_ms || ms > to_ms || !((from, to) == (client, 254) || (from, to) == (254, client)) {
            continue;
        }
        let dir = if from == 254 { "srv->cl" } else { "cl->srv" };
        let Some(p) = Packet::parse(&p) else {
            println!("{ms:>8} {dir} raw {} bytes", len);
            continue;
        };
        for f in p.first.iter().chain(&p.second) {
            if f.out_of_band {
                let id = packet::messages(&f.body).first().map_or(0, |m| m.id);
                println!("{ms:>8} {dir} ch{:<2} oob msg {id:#x}", f.channel);
                continue;
            }
            let chunks: Vec<String> = f
                .chunks
                .iter()
                .map(|c| {
                    let id = packet::messages(&c.data).first().map_or(String::from("-"), |m| format!("{:#x}", m.id));
                    if c.reliable { format!("R{}(p{}):{}", c.seq, c.previous, id) } else { format!("u(p{}):{}", c.previous, id) }
                })
                .collect();
            println!(
                "{ms:>8} {dir} ch{:<2} {}s{} ack {:?} [{}]",
                f.channel,
                if f.has_seq { "syn " } else { "" },
                f.seq,
                f.ack,
                chunks.join(" ")
            );
        }
    }
}
