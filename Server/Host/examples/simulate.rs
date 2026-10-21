//! Feeds one client's packets from a capture to the server host and prints what it answers:
//! `cargo run -p Host --example simulate -- hostA.bin 2`.

use ivmp_host::frame::Packet;
use ivmp_host::session::HostSession;

fn describe(packet: &[u8]) -> String {
    match Packet::parse(packet) {
        Some(p) => p
            .first
            .iter()
            .chain(&p.second)
            .map(|f| {
                if f.out_of_band {
                    format!("oob ch{} {}b", f.channel, f.body.len())
                } else {
                    let chunks: Vec<String> = f.chunks.iter().map(|c| format!("{}:{}b", c.seq, c.data.len())).collect();
                    format!("ch{} ack {:?} [{}]", f.channel, f.ack, chunks.join(" "))
                }
            })
            .collect::<Vec<_>>()
            .join(" | "),
        None => format!("raw {} bytes flags {:02x}", packet.len(), packet[0]),
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("capture");
    let client: u8 = args.next().and_then(|s| s.parse().ok()).unwrap_or(2);
    let data = std::fs::read(&path).expect("read capture");
    let mut host = HostSession::default();
    let (mut at, mut n) = (0, 0);
    while at + 12 <= data.len() && n < 25 {
        let from = data[at + 8];
        let len = u16::from_le_bytes([data[at + 10], data[at + 11]]) as usize;
        let packet = data[at + 12..at + 12 + len].to_vec();
        at += 12 + len;
        if from != client {
            continue;
        }
        n += 1;
        println!("client: {}", describe(&packet));
        for answer in host.receive(client, &packet) {
            println!("   host: {}", describe(&answer));
        }
        for line in host.log.drain(..) {
            println!("   {line}");
        }
    }
}
