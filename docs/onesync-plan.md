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

## Server settings (2026-10-21)

`server.cfg` next to `Server.exe` (written with defaults on first start; the command line still
overrides): `bind`, `host = 0|1` (1 = the server is the session host; 0 = peer, the default since 2026-10-21,
to peer with the first player hosting, the server only relays), `gamemode` (16 = Free Mode; negative = IVMP's own modes, -2 = scripted
IVMP freeroam, the default; 16 = GTA IV's own online Free Mode. IVMP modes run on a Free Mode session underneath), `slots`, `capture`. The server sends `ServerMessage::GameSettings`
(protocol 5) after the welcome; clients host and find games with that mode and slots
(`net::game_settings`, hostsys NETWORK_HOST_GAME / NETWORK_FIND_GAME, the lobby host override
and the stored network game config). The server host itself only replays Free Mode templates.

Two players with the server host: the AddGamer's app data was wrong. The gamer block is session
part, gamer info, peer address, a field, then app data (10-bit byte length + bytes) and a 6-bit
gamer count; in a join request the app data is the CMsgJoinRequest, in AddGamer the player's
CMsgPeerData (18 bytes). host10's trace showed the read stopping right after the app data length.
The server now builds AddGamer as: prefix of the player's join block, length 18, its peer data,
count 0 (the host's own AddGamer has the same shape). Untested.

### host11 (2026-10-21): two players work through the server host

The AddGamer fix worked: both games connect to each other (about 10,000 packets between them),
see each other and sync cars and peds; the server registry saw 93 entities. Problems seen:
driving another player's car doesn't move it for others, and both games spawn ambient traffic and
peds in the same spots (stacked cars, explosions).

* The games treat the server as a player and send it clone creates (330), syncs (459) and wait
  for confirmation; entities only change owner once every player has them. The server now
  confirms: CMsgPackedReliables section 1 (create acks) and 3 (remove acks), items 14-bit object
  id + 3-bit result (0 = ok, 1 = refused; reader 0x497F40 / 0x498310, handler 0x498930), reliable
  on ch7; CMsgPackedCloneSyncAcks (0x8B) per clone sync: 16-bit sequence, 8 bits 0, 1 bit 1
  (reader 0x49A1A0), unreliable on ch7. Untested.
* The games also send the host netComplaintMsg (0x55, 81 times on ch13), complaints about a peer
  that a host would act on (kick). The server ignores them for now.
* Population: next, the server picks one game per area to spawn ambient traffic and peds.

### Players leaving (2026-10-21)

When a client leaves or times out, the server sends every other joined client
snMsgRemoveGamersFromSessionCmd (0x53, reliable on ch13, with the session envelope): session id
(2 x u32), 6-bit gamer count (<= 32), per gamer its 8-byte gamer handle (two u32), then a 16-bit
reason (sign bit + 15 bits; 0). Writer 0x627060, reader 0x6271A0, validation 0x623EE0 (state 2..3,
session id == +0x540/+0x544), applied by 0x622360 (sender must be the host; looks each gamer up
with 0x622D90, the local gamer -> snEventRemovedFromSession, others -> snDropGamersTask).
The handle is taken from the leaver's gamer block: session id (64 bits), one set bit, then the
gamer info, which starts with the handle (bits 65..129; on host11 client 1's is DrazoxXD_HUN's
`441a7b8ee505d6c6`). Untested in game.

### Complaints and chunk sequencing (2026-10-21)

netComplaintMsg (0x55, unreliable, ch13; writer 0x6D7D30, reader 0x6D7DE0): complainer peer id
(2 x u32), u32 count (<= 64), the peer ids complained about (2 x u32 each). The complaint manager
(globals 0x1980CA0..0x1981920): 0x6D6D80 adds a peer to the list (0x19814F0, count 0x19816F0)
and sends it to the host at once (0x6D73D0), 0x6D6E40 removes one, 0x6D6F20 updates (a member
resends its list to the host every interval via 0x6D7440; a host processes the list, 0x6D74C0,
and its handler 0x6D70F0 pairs complainer and complainee). The only caller of the add is the
connection event handler 0x47E2B0 on **netEventConnectionError** (event type 4; rage event ids:
0 Requested, 1 Established, 2 Reassigned, 3 RelayAddressChanged, 4 Error, 5 Closed,
6 FrameReceived, 7 AckReceived, 8 BandwidthExceeded, 9 OutOfMemory).

host11: all 81 complaints are client 1 about peer 57001221:b8244948, the server host itself, once a
second from 340 s to the end (`cargo run -p Host --example complaints -- host11.bin`). Cause: the
server's reliable ordering was wrong, so client 1's connection to it errored out.

How the engine orders chunks (0x6F60B0): reliable and unreliable chunks share one sequence
counter. A reliable chunk carries its sequence; an unreliable one doesn't, it counts on from the
frame header's `seq` (the games put 0 there in frames without unreliable chunks). Every chunk's
16-bit `previous` is the sender's **last reliable** sequence, not seq - 1. A chunk is taken in
order when `previous` == the last reliable taken and its sequence isn't behind the next expected;
a reliable chunk up to 22 ahead is buffered, anything else dropped. In host11, client 1 sent
reliable 62933, 459 unreliable clone syncs (62934..63392), then reliable 63393 (previous 62933).
The server took every unreliable as `previous + 1` = 62934, expected 62935, dropped 63393 forever;
the client resent it every 250 ms until the connection errored (340 s).

Fixed in `Server/Host/src/session.rs`, both ways: incoming chunks are numbered and ordered as
above; outgoing chunks carry our last reliable sequence as `previous` and unreliable ones their
sequence in the frame's `seq` (before, the sync acks the host12 build sends had frame seq 0 and
the next reliable chunk named an unreliable as `previous`, which a client would drop forever).
Replaying host11 (`--example simulate -- host11.bin 1 100000`) now acks past 63393. Tools:
`--example complaints`, `--example link -- capture <client> [from ms] [to ms]` (frames between a
client and the server host).

### host12 (2026-10-21): a bike frozen for the other player

Test: Test2 (client 2) rode a bike; for Test (client 1) it stood still. Bike = object 991
(`cargo run -p Sync --example entity_log -- host12.bin 991`): owner 2 -> 1 (395 s) -> 2 (415 s)
-> 1 (436 s), then client 1 sent client 2 a new clone create of it (442.8 s) while client 2 kept
syncing it: both games thought they owned it, and client 1 ignored client 2's syncs.

Cause (most likely): the host12 server's ch7 was broken both ways by the chunk ordering bugs
above. It took only the first unreliable clone sync after each reliable chunk (so it acked almost
no syncs), and its own reliable chunks named an unreliable chunk as `previous`, so the clients
dropped them (only one batch of create acks arrived, at 240 s). A game hands an entity over
cleanly only once every player, the server included, has confirmed it, so handovers misbehaved.
Replaying host12 through the fixed server (`--example simulate -- host12.bin <client> 100000`)
keeps ch7 acked to the end. Untested in game.

Still unanswered by the server (`cargo run -p Host --example syncs -- host12.bin 600`):
* CMsgPackedEvents (0x4F, 573 + 377): game events with the server in scope wait for its reply
  (CMsgPackedEventReliablesMsgs 0x89) and are resent forever, e.g. REQUEST_EXPLOSION_EVENT
  ("Peer IVMP_Server_ in scope of event" in IVMP_Engine.log, packed 1150 times).
* CMsgArrayElements (0x86, 53 from client 2): probably waits for CMsgArrayElementsAck (0x87).
The client complaints in host12 (about client 2's peer 1f00025e:00000002, from 525 s) are just
client 1 noticing that cl2 was closed.

### Game events and array elements (2026-10-21)

The server host now acks both (`Server/Host/src/session.rs`), like a player whose game took them.

* CMsgPackedEvents (0x4F, unreliable, ch7; writer 0x4AC010 via 0x4AC0A0, read by 0x4AC140 /
  0x4ABF90): 5-bit item count, 15-bit length in bits, the events. Each event (0x4AEFB0): 5-bit
  type, 15-bit id (low 7 bits the number, the rest a sequence: `NAME_<number>_<sequence>` in
  IVMP_Engine.log), 15-bit data length, a flag (16 more bits if set), the data. The dispatcher
  0x4AF610 drops repeats per sender and calls the handler for the type (0..26).
* CMsgPackedEventReliablesMsgs (0x89, reliable, ch7; writer 0x4B18C0, reader 0x4B1940): 2-bit mask,
  bit 0 = acks block (count 5, length 15, 15-bit event ids; read by 0x4AF330 -> 0x4AFBB0), bit 1 =
  replies block (count, length, per reply 15-bit id, 15-bit data length, data; 0x4AF3E0 -> 0x4AFCC0).
  Ack and reply both clear the answering player's bit in the event's pending mask; at 0 the event
  is destroyed. An ack calls the event's ack handler (vtable +0x38), a reply its reply handlers
  (+0x28, +0x30/+0x34/+0x50) with the data. The server only acks.
* host12: the only event sent to the server was type 14, REQUEST_EXPLOSION (client 1 event 13_1
  573 times, cl2 27_1 377 times); between the clients types 1, 3, 11, 18, 25 (answered).
* CMsgArrayElements (0x86, unreliable; 0x4B71B0 / 0x4B7250, body 0x4B8810 / 0x4B88B0): 13-bit
  length, data that starts with a 16-bit sequence (one counter over all recipients), a 3-bit array
  handler and a 6-bit index. CMsgArrayElementsAck (0x87, unreliable; writer 0x4B8940): handler 3,
  index 6, sequence 16. In host12 cl2 sent handler 6, index 0 once a second to the server.
* Tools: `cargo run -p Host --example events|arrays -- capture.bin ...`. `--example simulate --
  capture 0 1000000` now replays every client into one server host (needed with two players: the
  host hands out channel sequences in join order); it only feeds packets addressed to the server.
  Replaying host12 that way the server acks all of cl2's 377 events, 53 array updates and 651 syncs.

### host13 (2026-10-21): client 1 can't drive

Build with the chunk ordering fix (before the event/array acks). 12 minutes, both players
connected throughout; ch7 healthy both ways (the server acked all 794 / 838 clone syncs, no stuck
reliable chunks); no complaints about the server. Unanswered: game events and array elements (the
next build acks them). Stuck events were requests for the host: CREATE_PICKUP (type 19, cl2,
2368 resends), REQUEST_FIRE (12, client 1), REQUEST_EXPLOSION (14, cl2). Event types: 1
GIVE_CONTROL, 3 WEAPON_DAMAGE, 11 VEHICLE_COMPONENT_CONTROL, 12 REQUEST_FIRE, 14 REQUEST_EXPLOSION,
19 CREATE_PICKUP, 25 DOOR_BREAK.

Seen by Drazox: client 2 drives with good sync; client 1 can't drive. Capture: car 503 while client
1 owns it (378 s, 407 s; 47..77 syncs per 5 s) stays within ~10 m; once client 2 takes it (440 s)
it moves at ~20 m/s. Ownership flips 2 -> 1 -> 2 every ~15 s (`--example entity_log -- host13.bin
503`). Client 1 triggered 124 GIVE_CONTROL events, client 2 63: client 1 gives away even the car
its player drives. cl2 refused 39 of client 1's VEHICLE_COMPONENT_CONTROL requests (reply `0`),
client 1 none.

How the game passes control (all on the owner):
* Vehicle update 0x4C9BF0: a vehicle whose driver (+0xF50) belongs to another player logs
  ">> Driver / >> Vehicle ... peer owner" (when the driver isn't a player, ped +0x219 = 0, set
  from the ped constructor's control info, 0x9E26A0); a player driver gets the vehicle (GIVE_CONTROL
  via 0x4AFEA0 at 0x4C9EB4) if CanPassControl allows; otherwise the generic path (vtable +0xFC).
* Generic proximity handover 0x48D1F0 (every 500 ms per owned object): if the object is farther
  than a threshold from the local player's ped (CPlayerInfo 0x11A8808[local index] +0x598), it goes
  to the nearest player that may take it (GIVE_CONTROL at 0x48D533).
* CanPassControl: vehicle 0x4CA050 (driver and passengers too) -> net object 0x48B4B0 -> 0x48E2D0:
  cloned on the target (per-player state at +0x28 + 3 * index: cloned, +0x29 create pending,
  +0x2A remove pending; 0x48D970 makes the masks), locally owned, no pending owner (+0x10 = 0xFF);
  with flag 2 (+0x12) every non-local player must have the clone; no creates or removes pending.
* VEHICLE_COMPONENT_CONTROL decision 0x4A8230: refused (reply bit 0) when the vehicle isn't
  locally owned or already has a pending owner, or the ped's player isn't the sender.

Next: the client DLL now logs every GIVE_CONTROL this game triggers (`controltrace.rs`: site,
object, target, local player index / ped / position, entity position, driver), to see why client 1
hands away the car it drives. Suspects: the local player's ped/position seen by the proximity check
(client 1 is player slot 31), or CanPassControl failing for client 1 as target on cl2.

### host14 (2026-10-21): handover trace

Build with the event/array acks and the DLL's give-control trace (`controltrace.rs`). Drazox:
vehicle sync still wrong (client 1 showed Test2 standing on a bike while Test2 drove a car on
cl2), and NPC vehicles/peds still spawn badly (population, open issue 3).

* Client 1 could drive: it owned car 518 from 190 to 290 s and drove it ~500 m east and back at
  20..25 m/s (`--example track -- host14.bin 518`). Car 556 was handed to client 1 by cl2's
  driver path (558 s), synced by it at driving rate, but barely moved.
* Trace: client 1 made 74 handovers, all proximity, none of the car its own ped drove (ruled
  out). cl2 made 43 proximity and 8 driver-path handovers (cars 518 and 556 to player 31); the
  pending owner stayed 0x1F over repeated attempts until the handover went through.
* Clone create refusals: result 1 comes from the net object's create (vtable +0x94, checked at
  0x498744 -> 0x498882), i.e. the game couldn't build the entity; 2 and 4 are other checks. cl2
  refused 87 of client 1's objects, every one accepted on a later resend (model streaming, normal).
  Client 1 refused cl2's object 993 (an OBJECT) all 360 times cl2 sent it (237..349 s) and its
  result-1 counter passed 1025; those refusals never show as acks in the capture (the handler
  only acks when it finds the sender's player, 0x498834). `--example creates -- capture`,
  `cargo run -p Sync --example create_types -- capture`.
* Not found yet: why client 1 showed Test2 on the bike. cl2 synced no vehicle at driving rate to
  client 1 in the last minute of the capture, and its player syncs to client 1 were sparse.

### Receiving side (2026-10-21, after host14)

* Clone sync results (per entry 0x493440, called at 0x492A2F, args sender, type, object id):
  5 applied, 3 no such object, 2 sender may not sync it (0x48E000 / object being removed), 1 the
  sync didn't apply (node read, vtable +0xB4) or a ped <-> dummy ped switch failed (0x495FE0), or
  the thread flags lack bit 1. CMsgPackedCloneSyncAcks (writer 0x4EDDC0): 16-bit sequence, 8-bit
  count, per failed entry 14-bit object id + 3-bit result (applied entries have no item).
  `cargo run -p Host --example sync_acks -- capture <from> <to>`. host14: client 1 failed cl2's
  syncs with result 1 for many peds (object 516 172 times, 1003 79), cl2 failed almost nothing.
* Clone create result 1 = the net object couldn't build its entity (vtable +0x94, 0x498744 ->
  0x498882); 4 = no room; 2 = existing object check.
* NPCs that "despawn and respawn": the registry now logs removes too. host14 object 491: created
  as DUMMYPED, synced into a PED, removed by its owner 1..3 s later, created again as a DUMMYPED
  (every few seconds for minutes). Ped <-> dummy ped switching done as remove + create.
* The DLL now logs (IVMP_Client.log): `sync in:` every failed received sync with the local
  object's type/owner and a 30 s summary per type and result (`synctrace.rs`); `clone create:`
  refusals per object and objects created again within 60 s (`jointrace.rs`); `give control`
  handovers (`controltrace.rs`).

How others do it: FiveM's GTA IV code (LibertyM) keeps a player as session host with migration
and relays the game's traffic without reading it (as GTA Connected); FiveM's GTA V OneSync
replaces the game's clone handling so the server decides ownership. IVMP: the server is the
session host and confirms like a player; the games still decide ownership among themselves.
