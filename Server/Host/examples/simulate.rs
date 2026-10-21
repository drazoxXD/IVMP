//! Feeds a client's packets to the server host (254) from a capture to a new server host and
//! prints what it answers: `cargo run -p Host --example simulate -- hostA.bin 2 [packets,
//! default 25]`. Client 0 feeds every client, in capture order: the host hands out channel
//! sequences in join order, so with more than one player only that matches the recording.

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
                    let chunks: Vec<String> = f
                        .chunks
                        .iter()
                        .map(|c| {
                            let id = ivmp_sync::packet::messages(&c.data).first().map_or(String::from("-"), |m| format!("{:#x}", m.id));
                            format!("{}:{}b:{id}", c.seq, c.data.len())
                        })
                        .collect();
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
    let limit: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(25);
    let data = std::fs::read(&path).expect("read capture");
    let mut host = HostSession::default();
    let (mut at, mut n) = (0, 0);
    while at + 12 <= data.len() && n < limit {
        let (from, to) = (data[at + 8], data[at + 9]);
        let len = u16::from_le_bytes([data[at + 10], data[at + 11]]) as usize;
        let packet = data[at + 12..at + 12 + len].to_vec();
        at += 12 + len;
        if (client != 0 && from != client) || to != 254 {
            continue;
        }
        n += 1;
        println!("client {from}: {}", describe(&packet));
        for answer in host.receive(from, &packet) {
            println!("   host -> {from}: {}", describe(&answer));
        }
        for line in host.log.drain(..) {
            println!("   {line}");
        }
    }
}
