//! Writes the host's join messages from a recording whose host was id 254 to
//! `Host/templates/` (used by `ivmp_host::templates`):
//! `cargo run -p Host --example extract -- hostA.bin`.

use ivmp_host::frame::Packet;

/// The server's gamer handle and name in the session.
const SERVER_HANDLE: [u8; 8] = [0x49, 0x56, 0x4D, 0x50, 0x53, 0x52, 0x56, 0xFE];
const SERVER_NAME: &str = "IVMP_Server";

/// Replaces every occurrence of `from` (at any bit offset) with `to` (same length).
fn replace_bits(bytes: &mut [u8], from: &[u8], to: &[u8]) {
    let bit = |b: &[u8], i: usize| (b[i / 8] >> (7 - i % 8)) & 1;
    let n = from.len() * 8;
    let total = bytes.len() * 8;
    let mut i = 0;
    while i + n <= total {
        if (0..n).all(|k| bit(bytes, i + k) == bit(from, k)) {
            for k in 0..n {
                let mask = 0x80 >> ((i + k) % 8);
                if bit(to, k) == 1 { bytes[(i + k) / 8] |= mask } else { bytes[(i + k) / 8] &= !mask }
            }
            println!("replaced {} bytes at bit {i}", from.len());
            i += n;
        } else {
            i += 1;
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: extract <capture with host 254> <host handle hex> <host name>");
    // The recording host's gamer handle (8 bytes hex) and name, to swap for the server's.
    let args_handle: Option<Vec<u8>> = args.next().map(|h| (0..h.len() / 2).map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).unwrap()).collect());
    let host_name: Option<String> = args.next();
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("templates");
    std::fs::create_dir_all(&out).unwrap();
    let data = std::fs::read(&path).expect("read capture");
    let mut at = 0;
    let (mut key_answer, mut detail, mut ch13, mut ch7) = (None, None, Vec::new(), Vec::new());
    while at + 12 <= data.len() {
        let from = data[at + 8];
        let len = u16::from_le_bytes([data[at + 10], data[at + 11]]) as usize;
        let packet = data[at + 12..at + 12 + len].to_vec();
        at += 12 + len;
        if from != 254 {
            continue;
        }
        if packet[0] & 1 != 0 {
            key_answer.get_or_insert(packet);
            continue;
        }
        let Some(p) = Packet::parse(&packet) else { continue };
        for f in p.first.iter().chain(&p.second) {
            if f.out_of_band && f.channel == 13 {
                detail.get_or_insert(f.body.clone());
            }
            for c in f.chunks.iter().filter(|c| c.reliable && !c.data.is_empty()) {
                // The join: the first 4 reliable messages on each channel.
                let list = match f.channel {
                    13 => &mut ch13,
                    7 => &mut ch7,
                    _ => continue,
                };
                if list.len() < 4 && !list.iter().any(|(seq, _)| *seq == c.seq) {
                    list.push((c.seq, c.data.clone()));
                }
            }
        }
    }
    // The recording's host was a real player: give the server its own gamer handle and name,
    // or a player joining with that same account takes the host for itself and never connects.
    let identity = |bytes: &[u8]| -> Vec<u8> {
        let mut bytes = bytes.to_vec();
        if let (Some(handle), Some(server_handle)) = (args_handle.as_ref(), Some(SERVER_HANDLE)) {
            replace_bits(&mut bytes, handle, &server_handle);
        }
        if let Some(name) = host_name.as_ref() {
            let mut server_name = SERVER_NAME.as_bytes().to_vec();
            server_name.resize(name.len(), b'_');
            replace_bits(&mut bytes, name.as_bytes(), &server_name);
        }
        bytes
    };
    std::fs::write(out.join("key_answer.bin"), identity(&key_answer.expect("no key exchange answer"))).unwrap();
    std::fs::write(out.join("session_detail.bin"), identity(&detail.expect("no session detail"))).unwrap();
    for (name, list) in [("ch13", &ch13), ("ch7", &ch7)] {
        assert_eq!(list.len(), 4, "{name}: {} messages", list.len());
        for (i, (seq, bytes)) in list.iter().enumerate() {
            println!("{name} #{i}: seq {seq}, {} bytes", bytes.len());
            std::fs::write(out.join(format!("{name}_{i}.bin")), identity(bytes)).unwrap();
        }
    }
    println!("templates written to {}", out.display());
}
