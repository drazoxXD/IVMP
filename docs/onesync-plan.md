# Server-authoritative sync ("OneSync"-like)

Goal: the IVMP server, not the hosting player, is the authority for the game world. It knows
every networked entity (players, peds, vehicles, objects), decides who owns what, replicates the
world to anyone who connects, and can own entities itself.

## Where we are (2026-10-20)

* Every engine packet already passes through the server (`net.rs` tunnel, `Server` relay),
  but the server treats them as opaque bytes and just forwards them.
* The engine's own clone sync (rage `netObjectMgr`, `CNetObj*`) works peer to peer between the
  clients: the first client hosts the session, the others join it.
* Object ids: the host keeps its ids; a joiner gives itself a block (4000 + 1000 * client id)
  because the host's id grant never arrives (`hostsys.rs`).
* The game's own network log is available (`IVMP_Engine.log`).

## Phases

### 1. The server understands the traffic
Decode the engine's packet framing (`rage::netConnection` frames, then the messages inside):
`CloneCreate`, `CloneSync`, `CloneRemove`, their acks, object id messages, game events. The
server logs them per entity (type, id, owner, position). No behaviour change yet.

Work: reverse the frame/message layout from the send path (`netSocket` send 0x6F1A60 callers,
message ids registered with the connection manager 0x18E49D8) and confirm with captures from the
tunnel.

### 2. Server-assigned object ids and an entity registry
* The server hands out object id ranges to every client (replaces the self-assigned block and
  the missing host grant).
* The server keeps the registry: id -> type, owner, last create data, last sync data, position.
* Ownership changes (migration when a player leaves) are decided and broadcast by the server.

### 3. Connecting replicates the world
When a player connects, the server sends them the current world: a `CloneCreate` (from the
stored data) for every entity in range, then the latest sync. The newcomer's own player is
announced to everyone the same way. Scoping by distance so only relevant entities are sent.

### 4. Server-owned entities
The server creates and owns peds, vehicles and objects (from server scripts later). Clients get
them as clones of a "server" owner. Needs a session member the engine accepts as owner: either a
reserved player slot that represents the server, or remapping server-owned entities onto the
host client for simulation while the server keeps authority over their state.

### Later
Population control (server decides ambient peds/cars), server scripting hooks, anti-cheat
checks on incoming sync (speed, teleport, ownership).

## Open questions
* Is the packet payload encrypted (rage `netCrypto` keys from the peer address)? Our keys are
  deterministic (`peer.rs`), so the server can derive them if needed.
* How much of the sync data must the server parse (positions for scoping) vs. store as opaque
  blobs to replay.

## Phase 1 notes (2026-10-20)

* Packet = `[flags u8][nonce u32][payload][mac u32]` (0x6D2020 send / 0x6D2320 receive). The
  payload was RC4 (0x6F1530) with a key from the nonce (key generator 0x6F1590, deterministic in
  IVMP) XOR a title key (0x6EB3E0 / 0x6EB440). IVMP clients now skip the RC4 calls
  (`Networking/src/crypt.rs`), so captures are plain.
* `Server --capture file` records `u64 ms, u8 from, u8 to, u16 len, packet`.
* First capture (46k packets): payload starts with a big-endian u16 total length, then a u16
  (frame header / sequence), then bit-packed frame data. Recurring `c9 19 14`, `c9 19 16`,
  `c9 19 18` look like message headers. Next: reverse the netConnection frame layout (sequence,
  acks, channel, reliable/unreliable chunks) and the message id table, then the clone messages.

## Message ids (GTA IV 1.2.0.59)

Messages start with the 14-bit magic 0x3246, then 1 bit (0 = 8-bit id, 1 = 16-bit id) and the id
(writer 0x47F8A0, bits MSB first). Ids are fixed at init (AutoIdDescriptor, e.g. 0x4E6980).

| id | message | id global |
| --- | --- | --- |
| 0x03 | CMsgVoiceStatus | 0x19d2c08 |
| 0x05 | rlMsgSearchRequest | 0x19f396c |
| 0x0F | netKeyExchangeOffer | 0x1a0585c |
| 0x11 | netIceSessionPong | 0x1b432e0 |
| 0x12 | snMsgMigrateHostResponse | 0x19f0a5c |
| 0x13 | rlSessionDetailResponse | 0x19f38a4 |
| 0x15 | netIceSessionPing | 0x1b48468 |
| 0x1D | rlMsgQosProbeRequest | 0x19f39ac |
| 0x1F | snMsgSetInvitableCmd | 0x19f093c |
| 0x22 | rlSessionDetailRequest | 0x19f3868 |
| 0x23 | snMsgSessionMemberIds | 0x19f097c |
| 0x24 | CMsgTextMessage | 0x19f0828 |
| 0x28 | netIceSessionDebugInfo | 0x1b43320 |
| 0x2A | snMsgJoinResponse | 0x19f09dc |
| 0x2C | rlMsgQosProbeResponse | 0x19f39cc |
| 0x2D | snMsgStartMatchCmd | 0x19f08bc |
| 0x2F | cxnRequestRemoteTimeoutMsg | 0x19f38c4 |
| 0x35 | snMsgNotMigrating | 0x19f095c |
| 0x38 | netTimeSyncMsg | 0x19f24d0 |
| 0x41 | snMsgJoinRequest | 0x19f0a1c |
| 0x44 | netKeyExchangeAnswer | 0x1a0b088 |
| 0x45 | netIceSessionPortOpener | 0x1b484c8 |
| 0x48 | snMsgConfigRequest | 0x19f09bc |
| 0x49 | cxnRelayAddrChangedMsg | 0x19f38e4 |
| 0x4F | CMsgPackedEvents | 0x19d2da8 |
| 0x53 | snMsgRemoveGamersFromSessionCmd | 0x19f099c |
| 0x54 | snMsgRequestGamerInfo | 0x19f087c |
| 0x55 | netComplaintMsg | 0x19f3924 |
| 0x58 | snMsgHostLeftWhilstJoiningCmd | 0x19f089c |
| 0x5A | snMsgChangeSessionAttributesCmd | 0x19f09fc |
| 0x5F | snMsgConfigResponse | 0x19f0a3c |
| 0x61 | CMsgPlayerIsTyping | 0x19f0848 |
| 0x64 | snMsgAddGamerToSessionCmd | 0x19f08fc |
| 0x66 | snMsgMigrateHostRequest | 0x19f091c |
| 0x68 | CMsgJoinRequest | 0x19d2c88 |
| 0x69 | snMsgBase | 0x19f08dc |
| 0x6B | rlMsgSearchResponse | 0x19f394c |
| 0x7F | netIceSessionOffer | 0x1b432c0 |
| 0x80 | netIceSessionAnswer | 0x1b48488 |
| 0x86 | CMsgArrayElements | 0x19d277c |
| 0x87 | CMsgArrayElementsAck | 0x19d279c |
| 0x89 | CMsgPackedEventReliablesMsgs | 0x19d2d88 |
| 0x8A | CMsgCloneSync | 0x19d2d48 |
| 0x8B | CMsgPackedCloneSyncAcks | 0x19d2d68 |
| 0x8C | CMsgPackedReliables | 0x19d2ce8 |
| 0x8D | CMsgReassignNegotiate | 0x19d2cc8 |
| 0x8E | CMsgReassignConfirm | 0x19d2d28 |
| 0x8F | CMsgReassignResponse | 0x19d2d08 |
| 0x90 | CMsgPeerData | 0x19d2c68 |
| 0x92 | CMsgHostData | 0x19d2c28 |
| 0x93 | CMsgGetReadyToStartPlaying | 0x19d2bc8 |
| 0x94 | CMsgStartedLocalPlayerPlaying | 0x19d2c48 |
| 0x95 | CMsgInformObjectIDs | 0x19d2be8 |
| 0x97 | CMsgTextStatus | 0x19d2ca8 |
| 0x98 | MsgTvtSummons | 0x19d285c |
| 0x99 | MsgTvtReservationRequest | 0x19d287c |
| 0x9A | MsgBlacklist | 0x19d283c |
| 0x9B | CMsgDenyJoin | 0x19d281c |
| 0x9C | MsgGamerRank | 0x19d27fc |
| 0x9D | MsgGamerRankBroadcast | 0x19d27dc |
| 0x9E | MsgGamerRankRequest | 0x19d27bc |
| 0xA0 | MsgKickPlayer | 0x1980c88 |
| 0xA1 | MsgRejoin | 0x1981928 |
| 0xA4 | MsgViralKillMe | 0x19d59f8 |

Capture counts: CMsgCloneSync and CMsgPackedCloneSyncAcks dominate (about 40k each),
CMsgPackedReliables (clone create / remove) about 22k. CMsgInformObjectIDs (0x95), the host's
object id grant, was never sent, which is why joiners had no ids.

## Phase 1 result (2026-10-20)

`Server/Sync` (`ivmp_sync`) decodes the traffic and `Server` keeps a live entity registry:

* Payload: big-endian u16 split, flags byte (bit 1 = first area compressed with the engine's
  LZSS, 0x670EB0, implemented in `packet::decompress`), two frame areas; frames have a 9-byte
  header with a 10-bit size.
* CMsgPackedReliables: 4-bit section mask, per section 5-bit count, 10-bit length, data; the
  first section holds clone creates: 4-bit object type, 14-bit object id, 4-bit flags, 10-bit
  data length, data (reader 0x497C50). Object types (0x48EB30): PLAYER, PED, DUMMYPED,
  AUTOMOBILE, OBJECT, BIKE, TRAIN, BOAT, HELI, PLANE.
* CMsgCloneSync: 16-bit sequence, 32-bit time, 13-bit length, sync data (0x49A050).
* The first capture decodes to 264 entities, both players among them (host object 1, second
  client object 6201), and 45k clone syncs. `cargo run --example decode_capture -- file`.

* Removes (third reliable section, 0x4981F0): 14-bit object id, 5-bit token. The registry drops
  them, so the entity count now follows the live world (about 90 instead of an ever-growing list).
* Clone sync entries (0x492820): 4-bit type, 14-bit object id, 1 bit, a node mask whose width
  depends on the type (0x494A20: PLAYER 16, PED 15, DUMMYPED 6, AUTOMOBILE/HELI/PLANE 14,
  OBJECT 9, BIKE/TRAIN/BOAT 13), then the sync tree nodes. Only the owner syncs, so a sync from
  another client is an ownership change (logged as "now owned by"). DUMMYPED and PED switch type
  on the same id.

* Positions (`Server/Sync/src/nodes.rs`): entity nodes (CNetObjEntity, 0x48B970) 0 = 9 bits,
  1 = type state, 2 = map sector 3 x 7 bits (0x48C680), 3 = position in the sector 3 x 12 bits
  over 54/54/50 units (0x48C9B0); world = (sector - 64) * size + local, x + 162, z - 975.
  Node 1 per type (vtable +0x10C, bigger for script entities, create flag 8): physical base
  42 bits (+4), ped 0x49F6F0, player 0x4B9A50, vehicle 0x4CCB20 (+ auto/heli/plane 0x4E5350,
  bike 0x4E58A0, boat 0x4E5B70), object 0x4D7710, dummy ped 0x4EE740. Creates: type header
  (vtable +0x94) then nodes 0..3; world objects carry a 3 x 19-bit world position instead
  (0x4BB810). On the capture every create and sync with a position decodes onto the map.

Still open in phase 1: the two ack sections; sync entries after the first (they need every
node's size).

## Phase 2 (2026-10-20)

The server gives every client an object id block (`ivmp_sync::ids`: client c gets
`c * 490 .. c * 490 + 490`, ids below 490 stay with the engine) in `ServerMessage::ObjectIds`
(protocol version 2). The client empties the engine's id ring and fills it with the block on
its first in-session frame, host and joiner alike (`hostsys::use_object_ids`). The registry
logs creates whose id lies in another client's block.

## Phase 4 (2026-10-21)

Server-owned entities go through a client: the server (`Server/src/world.rs`) picks the client
nearest to the entity (else the first one) and sends `ServerMessage::Spawn` (protocol 3); the
client (`Client/Networking/src/spawner.rs`) streams the model and creates a networked mission
entity with the engine's factories (vehicle 0x5DA990, ped 0x5C3770, pop type 2), then answers
with its object id. The server follows it in the registry and has it created again if it is
gone for 10 s. Console: `spawn car|ped <model> [x y z [heading]]`, `entities`, `despawn <id>`.
Native hashes in 1.2.0.59 don't match the public lists (old ones are joaat of the lowercase
name), so the factories are called directly.

Despawn: the server sends `ServerMessage::Despawn` to the entity's owner; the client looks the
network object up (0x4913C0 on the object manager at 0x1908EF0) and destroys the entity with
its factory (vehicles 0xB59180, peds 0xB58EA0), which unregisters it for everyone.


## Server as the session host (in progress, 2026-10-21)

Goal: the server is the permanent session host; every game joins it, none hosts, no migration.

Join, in order (capture of a join, joiner 2 -> host 1):
1. 2 bytes `11 10`, answered the same (connection check, not a frame packet).
2. netKeyExchangeOffer / Answer (packet flag 1: not frames).
3. rlSessionDetailRequest -> rlSessionDetailResponse (session attributes `MMATTR_*`), out of band.
4. Key exchange again, then snMsgJoinRequest + CMsgJoinRequest (reliable, channel 13).
5. Host: snMsgJoinResponse, CMsgPeerData, CMsgHostData, snMsgAddGamerToSessionCmd, CMsgPeerData,
   snMsgStartMatchCmd; then snMsgChangeSessionAttributesCmd, netTimeSyncMsg (out of band),
   CMsgInformObjectIDs, CMsgGetReadyToStartPlaying; joiner answers CMsgStartedLocalPlayerPlaying.
6. Keep-alive: every second, an ack-only frame per channel (13, 7, 15).

Frame header (9 bytes, bits MSB first; receive 0x6CF560, per connection 0x6F60B0):
size 10 | out-of-band 1 | direct 1 | has ack 1 | has seq 1 | channel 4 | seq 16 | ack 16 | 22 bits.
`ack` is the next reliable sequence expected. Out-of-band frames carry one message directly.
Chunks inside a connection frame (0x6F50E0): size 10 | reliable 1 | 16 bits (previous seq) |
seq 16 if reliable; header 6 bytes reliable, 4 otherwise; then the messages. Session messages
start with an 8-byte envelope (`8c 25 30 00 00 00 <len>`) before the message magic.
`cargo run -p Sync --example frames -- capture.bin` dumps all of this.

Implementation (`Server/Host`, `Server --host`): the server answers as client 254. `frame.rs` is the
packet/frame/chunk codec (every packet of a recording re-encodes byte for byte); `session.rs`
keeps one connection per client with reliable channels 13/7/15 (SYN chunk, acks = next expected,
unreliable chunks also use a sequence number, resend after 250 ms, SYN resend after 500 ms, ack
keep-alive every second); `templates/` holds the host's join messages from a recording hosted as
254. Time sync answers are live (netTimeSyncMsg 0x6BE7E0: type 2 bits, sequence, token, client
time, host time). Clients skip the packet MAC check (crypt.rs).

### How to use and test

* `Server.exe --host [--capture file]`: the server hosts, every client joins it (client
  `Welcome.host` = 254). Without `--host` the first player hosts as before.
* `Server.exe --ids 254,2 --capture hostA.bin`: hands out client ids in join order, used to
  record a session hosted by a game as id 254 (where the templates come from).
* Tools (`Server/`): `cargo run -p Host --example extract -- hostA.bin 441a7b8ee505d6c6 DrazoxXD_HUN`
  (write templates, swapping the recording host's gamer handle and name for the server's
  `IVMPSRV\xFE` / `IVMP_Server`), `--example roundtrip` (codec check), `--example diff a.bin b.bin`
  (host join messages of two recordings), `--example chunks capture.bin <from id>` (what one side
  sent), `--example simulate capture.bin <client>` (feed a client's packets to the server host),
  `cargo run -p Sync --example frames|handshake -- capture.bin` (frame / message dumps).

### Templates (from recording A, host 254 and joiner 2)

* `key_answer.bin`: key exchange answer packet (flag 1); identical for any joiner.
* `session_detail.bin`: rlSessionDetailResponse (out of band, ch13); 9 bytes at 14..22 differ
  per session.
* ch13, in order: `ch13_0` snMsgJoinResponse + CMsgPeerData (the joiner's) + CMsgHostData;
  `ch13_1` snMsgAddGamerToSessionCmd (the host gamer) + CMsgPeerData (host's); `ch13_2`
  snMsgStartMatchCmd; `ch13_3` snMsgChangeSessionAttributesCmd.
* ch7, in order: CMsgInformObjectIDs (empty: ids come from the IVMP server), CMsgVoiceStatus,
  CMsgTextStatus, CMsgGetReadyToStartPlaying (203 bytes of game state). Sent once the client
  talks on ch7.
* Session messages on ch13 start with a 7-byte envelope `8c 25 30 00 00 00 XX` (XX = 1f, 20, 39
  in the recording); it is stripped before the message handlers see the message (+0x1C/+0x20 of
  the received message = data/length without it).

### Findings

* Identity: the recording's host was a real player (DrazoxXD_HUN, gamer handle
  `44 1a 7b 8e e5 05 d6 c6`). Replaying it unchanged made the server look like that player; a
  client logged into the same account ignored the host, never opened ch7, and later made itself
  host ("DrazoxXD_HUN has become the new host" in IVMP_Engine.log). With the server's own handle
  and name the client opens ch7, gets the game state, lists the server as player (slot 1 in its
  player list) and plays normally. Working since test host3.
* Player slots: CMsgPeerData's first byte is the player slot (host's 1, joiner's 31 in the
  recording). The server gives client `id` slot `32 - id` (in the joiner's own peer data in
  ch13_0) and the client creates its player there when the host is 254 (`boot.rs player_slot`).
* snMsgJoinRequest (0x41) and snMsgAddGamerToSessionCmd (0x64) have the same body (both read by
  0x626E70): session part 0x627640 (session id u32,u32 via 0x627780 = 2:0 in IVMP, handle 0x628030,
  0x628110, 0x48AC50, 10-bit length + bytes), 6-bit gamer count (< 33), per gamer 0x4E0800. A join
  request is `0x41 + body` then CMsgJoinRequest (0x68); the first 121 bits of 0x68's body look
  like the player's CMsgPeerData (slot 0, second field 0000001 instead of 1000001).
* CMsgPeerData (0x90), read by 0x48A980: slot 8 bits (0x48AB70), 0x48ABE0, 5 bits, 32 bits,
  32+32, 0x48AB00, flag (0x48A940) and optional fields; 121 bits for a joiner.
* AddGamer handling on a member (0x6245C0): validation 0x623D80 = session state (+0x50) 2..3,
  message connection (+0x18) == host's (peer id +0xC30/+0xC34 looked up by 0x622EA0), parse
  (0x61D300 -> 0x626E70), session id == +0x540/+0x544; then join-to-rline task (0x623490),
  gamers added (0x660B00), task start (0x626B70).

### Where it stands (2026-10-21)

* One player: joins the server host and plays (tests host3..host9).
* Two players: both join; the server sends each an AddGamer for the other ("introduced clients 2
  and 1"), built as envelope (message number counted up from 0x41) + 0x64 + the other player's
  join body (session id forced to 2:0) + CMsgPeerData (from the other player's 0x68, slot set,
  bit 8 set). Both clients ack it, but the client rejects it at the validation step (trace:
  "add gamer: message valid -> 0"). Logged values: state 2, message connection == host
  connection (0x10000), session id 2:0 on both sides. So the parse fails (0x61D300), or the
  parsed session id differs. The DLL now parses the message itself and logs the result and the
  bytes (`jointrace.rs install_add_gamer_trace`) to find out which.
* No traffic between the two clients yet, so no player, NPC or vehicle sync in --host mode.
* Not done: players leaving (snMsgRemoveGamersFromSessionCmd), more than one recorded template
  session, object id grants from the host (clients use the IVMP server's ids).

### Notes on recordings

* Recording B (`--ids 254,3`): the joiner with id 3 didn't see the host (host was slot 1 for it),
  a client-hosting side issue with ids above 2; irrelevant once the server hosts.
* The diff of A and B shows the host's join messages depend on the session, not on the joiner.

### Next (2026-10-21, after host9)

* host9 showed the client's own parse of the intro AddGamer fails (0x61D300 -> 0), while the
  server's own AddGamer parses. The intro's gamer block is bit for bit the other player's join
  request block (748 bits); the parse must depend on content (gamer info import 0x6BDB70 via
  0x628030, peer address import 0x6C70C0 via 0x628110, 0x48AC50, 6-bit count, 0x4E0800 per gamer).
* The DLL (14:52 build) traces each field read of the block (`jointrace.rs`, "gamer block: ..."
  lines). Next test: `Server.exe --host --capture host10.bin`, two clients, then read which field
  returns 0 for the intro.
