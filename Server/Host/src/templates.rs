//! The host's join messages, taken from a session hosted by a game with client id 254 (the id
//! the server hosts as): `cargo run -p Host --example extract -- hostA.bin` writes them to
//! `Host/templates/`. They hold the host's identity (peer address 10.0.0.254, session ids that
//! IVMP makes deterministic) and the session's settings, not the joiner's.

/// Key exchange answer packet (flag 1): the host's peer address.
pub const KEY_ANSWER: &[u8] = include_bytes!("../templates/key_answer.bin");
/// rlSessionDetailResponse: the session's attributes (out of band, channel 13).
pub const SESSION_DETAIL: &[u8] = include_bytes!("../templates/session_detail.bin");
/// Channel 13, in order: snMsgJoinResponse + CMsgPeerData + CMsgHostData,
/// snMsgAddGamerToSessionCmd (the host gamer) + CMsgPeerData, snMsgStartMatchCmd,
/// snMsgChangeSessionAttributesCmd.
pub const JOIN_SESSION: [&[u8]; 4] = [
    include_bytes!("../templates/ch13_0.bin"),
    include_bytes!("../templates/ch13_1.bin"),
    include_bytes!("../templates/ch13_2.bin"),
    include_bytes!("../templates/ch13_3.bin"),
];
/// The session envelope in front of snSession messages (as in the recording's AddGamer).
pub const SESSION_ENVELOPE: [u8; 7] = [0x8C, 0x25, 0x30, 0x00, 0x00, 0x00, 0x1F];
/// The session id (two u32) at the start of every gamer block, as the recording's AddGamer
/// has it (bits 79..143 of `JOIN_SESSION[1]`; IVMP forces session ids to 2).
pub const SESSION_ID: [u8; 8] = [0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00];

/// Where the joiner's CMsgPeerData body starts inside `JOIN_SESSION[0]` (the message starts at
/// bit 112, its body after the 23-bit header).
pub const JOIN_PEER_DATA_BIT: usize = 112 + 23;

/// Channel 7, in order: CMsgInformObjectIDs (none; IVMP's server hands out ids), CMsgVoiceStatus,
/// CMsgTextStatus, CMsgGetReadyToStartPlaying.
pub const JOIN_GAME: [&[u8]; 4] = [
    include_bytes!("../templates/ch7_0.bin"),
    include_bytes!("../templates/ch7_1.bin"),
    include_bytes!("../templates/ch7_2.bin"),
    include_bytes!("../templates/ch7_3.bin"),
];
