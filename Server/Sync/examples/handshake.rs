//! Lists the game messages in a capture in order, with direction and frame bytes, to study the
//! session handshake: `cargo run -p Sync --example handshake -- capture.bin [seconds]`.

use ivmp_sync::packet;

/// Names of the session / connection messages (docs/onesync-plan.md).
fn name(id: u32) -> &'static str {
    match id {
        0x0F => "netKeyExchangeOffer",
        0x44 => "netKeyExchangeAnswer",
        0x12 => "snMsgMigrateHostResponse",
        0x13 => "rlSessionDetailResponse",
        0x22 => "rlSessionDetailRequest",
        0x1F => "snMsgSetInvitableCmd",
        0x23 => "snMsgSessionMemberIds",
        0x2A => "snMsgJoinResponse",
        0x2D => "snMsgStartMatchCmd",
        0x2F => "cxnRequestRemoteTimeoutMsg",
        0x35 => "snMsgNotMigrating",
        0x38 => "netTimeSyncMsg",
        0x41 => "snMsgJoinRequest",
        0x48 => "snMsgConfigRequest",
        0x49 => "cxnRelayAddrChangedMsg",
        0x53 => "snMsgRemoveGamersFromSessionCmd",
        0x54 => "snMsgRequestGamerInfo",
        0x58 => "snMsgHostLeftWhilstJoiningCmd",
        0x5A => "snMsgChangeSessionAttributesCmd",
        0x5F => "snMsgConfigResponse",
        0x64 => "snMsgAddGamerToSessionCmd",
        0x66 => "snMsgMigrateHostRequest",
        0x68 => "CMsgJoinRequest",
        0x69 => "snMsgBase",
        0x4F => "CMsgPackedEvents",
        0x86 => "CMsgArrayElements",
        0x87 => "CMsgArrayElementsAck",
        0x89 => "CMsgPackedEventReliablesMsgs",
        0x8A => "CMsgCloneSync",
        0x8B => "CMsgPackedCloneSyncAcks",
        0x8C => "CMsgPackedReliables",
        0x90 => "CMsgPeerData",
        0x92 => "CMsgHostData",
        0x93 => "CMsgGetReadyToStartPlaying",
        0x94 => "CMsgStartedLocalPlayerPlaying",
        0x95 => "CMsgInformObjectIDs",
        0x9B => "CMsgDenyJoin",
        _ => "?",
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: handshake <capture file> [seconds]");
    let limit_ms: u64 = args.next().and_then(|s| s.parse::<f64>().ok()).map_or(30_000, |s| (s * 1000.0) as u64);
    let data = std::fs::read(&path).expect("read capture");
    let mut at = 0;
    let mut first_ms = None;
    while at + 12 <= data.len() {
        let ms = u64::from_le_bytes(data[at..at + 8].try_into().unwrap());
        let (from, to) = (data[at + 8], data[at + 9]);
        let len = u16::from_le_bytes([data[at + 10], data[at + 11]]) as usize;
        let Some(p) = data.get(at + 12..at + 12 + len) else { break };
        at += 12 + len;
        let start = *first_ms.get_or_insert(ms);
        if ms - start > limit_ms {
            break;
        }
        let frames = packet::frames(p);
        let mut names = Vec::new();
        for frame in &frames {
            for m in packet::messages(frame) {
                if m.id == 0x8A || m.id == 0x8B {
                    continue;
                }
                names.push(format!("{}({:#x})", name(m.id), m.id));
            }
        }
        let head: String = p.iter().take(16).map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ");
        println!(
            "{:7.3}s {from}->{to} len {len:4} flags {:02x} frames {} {} | {head}",
            (ms - start) as f64 / 1000.0,
            p[0],
            frames.len(),
            names.join(" ")
        );
    }
}
