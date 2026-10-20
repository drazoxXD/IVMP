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
