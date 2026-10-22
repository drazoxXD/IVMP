# IVMP handoff (2026-10-21)

Everything a fresh session needs to continue. Read this first, then `docs/onesync-plan.md`.

## Goal

IVMP is a GTA IV multiplayer mod in Rust for the Steam Complete Edition (GTAIV.exe 1.2.0.59, run
through the Rockstar Games Launcher). It uses the game's own network engine (rage netcode, clone
sync), with all game traffic tunnelled through the IVMP server. Targets: load straight into online
(no story mode), player / animation / weapon / vehicle / object sync, server-owned entities
("OneSync"-like), and the **server as the permanent session host** (no host migration; clients
are just players). Later: server scripting for game mode -2 (IVMP freeroam).

## Rules from Drazox (the user)

* **Never commit unless explicitly asked** for that commit. Commit style: `type(scopes): Title`,
  short body, optional "Current problems:" list, **no Co-Authored-By trailer**. **Never push.**
* Keep code in separate, editable crates and modules (networking is not baked into the hooks DLL).
* Builds go to `out/<profile>` (the launcher loads from `Client/out/debug`). Builds must work from
  cmd and PowerShell.
* Ghidra: rename functions you identify and add `IVMP:` comments.
* No client-only spawns: every ped/vehicle must be networked (the factories are patched).
* An unfocused (alt-tabbed) game must not throttle or slow down (desync); it keeps rendering and
  ignores input.
* Intros stay on (the intro-skip patch is disabled): with it enabled the multiplayer lobby / session
  doesn't start.
* `server.cfg` comments use Drazox's wording for `host` (see below).

## Layout

* `Client/` (Cargo workspace, 32-bit target `i686-pc-windows-msvc` for the DLL):
  * `Core` (`ivmp_core`): memory helpers (`from_ghidra`, patterns, patches), logger.
  * `NativeInvoker` (`ivmp_natives`): native calls by hash, native logger.
  * `Networking` (`ivmp_networking`): `net` (server connection, tunnel, game settings),
    `peer` (peer identity, find-game answer, session reader), `relay`, `hostsys` (host/join state
    machine, object id ring), `session`, `lobby`, `instance` (`-cl2`), `netobjects`, `crypt`
    (packets unencrypted, MAC check off), `spawner` (server-owned spawns/despawns), `jointrace`
    (diagnostic call traces incl. the AddGamer checks).
  * `Hooks` (cdylib **IVMP_Client.dll**): boot (network boot, player slot, MP model), focus,
    crash logging, mods folder (FusionFix `mods\dinput8.dll`, not for cl2), engine log, glue.
  * `Launcher` (`ivmp_launcher.exe`, 64-bit): starts the game via Steam, injects the DLL after
    the window opens; `-cl2` starts a second client (parent = Rockstar launcher). Waits for a
    retried start when the first start quits (exit 0xC0E90002; press Retry in the R* launcher).
  * `Shared`: config (`ivmp.json`, `ivmp_cl2.json`), constants.
* `Server/` (Cargo workspace): root package builds **Server.exe** (`src/`: config, log, lobby,
  relay, capture, world (server-owned entities), console, server loop); crates `NetProtocol`
  (wire protocol, shared with the client; protocol version 5), `Sync` (`ivmp_sync`: packet/frame
  decoding, entity registry, object id blocks, position nodes), `Host` (`ivmp_host`: the server as
  session host: frame codec, session, templates), `Auth`/`Natives`/`Scripting` (empty stubs).
* `.github/workflows/build.yml`: builds client + server, separate zips, pre-release only.

## Build and run

```
cd Client
cargo build --target i686-pc-windows-msvc -p Hooks     # IVMP_Client.dll
cargo build -p Launcher                                  # ivmp_launcher.exe
# copy target/i686-pc-windows-msvc/debug/IVMP_Client.dll (+ .pdb) and target/debug/ivmp_launcher.exe
# to Client/out/debug (if locked by a running game, rename the old file first)
cd ../Server
cargo build                                              # target/debug/Server.exe
cargo test --workspace
```
Run: start `Server/target/debug/Server.exe`, then `Client/out/debug/ivmp_launcher.exe`; second
client: `ivmp_launcher.exe -cl2` (or `cl2.cmd`). A running Server.exe locks its exe: rename it
(`Server.old<time>.exe`) before rebuilding, or close it.

`server.cfg` (next to Server.exe, written with defaults on first start; command line overrides
with `--host`, `--capture <file>`, `--ids a,b`):
```
host = 0        # 0 = peer to peer (first player hosts, the default), 1 = server is the host (experimental)
gamemode = -2   # -2 = IVMP freeroam (current), 16 = GTA IV's own online Free Mode
slots = 32
bind = 0.0.0.0:9999
capture =
```
Server console: `spawn car|ped <model> [x y z [heading]]`, `despawn <id>`, `entities`, `help`.

Logs: `Client/out/debug/IVMP_Client.log` / `IVMP_Client_cl2.log` (DLL), `IVMP_Engine.log` /
`IVMP_Engine_cl2.log` (the game's own logger), `Server/target/debug/ivmp_server.log`, captures
(`--capture`) in `Server/target/debug/*.bin`. Rockstar launcher log:
`Documents/Rockstar Games/Launcher/launcher.log`.

Capture tools (from `Server/`): `cargo run -p Host --example chunks|simulate|roundtrip|diff|
to_host|acks|create_acks|extract|complaints|link|syncs|events|arrays -- <capture> ...`, `cargo run -p Sync --example
frames|handshake|decode_capture|check_positions|track -- <capture> ...`.

Reverse engineering: Ghidra (GTAIV_dump.exe, Ghidra addresses base 0x400000, runtime relocated;
use `memory::from_ghidra`). If the Ghidra MCP plugin drops, its HTTP API is at
`http://127.0.0.1:8080` (`decompile_function?address=`, `xrefs_to`, `read_memory`,
`disassemble_function`). For code Ghidra didn't analyse: `pip` capstone over
`Client/out/debug/GTAIV_dump.exe`. GTA IV 1.2.0.59 native hashes don't match public lists
(scrambled); the spawner calls the engine factories directly.

## What works (as of host11, 2026-10-21)

* Boot straight into a network Free Mode session; second client on the same PC (`-cl2`).
* Peer to peer (`host = 0`): first player hosts, others join; players, peds, cars sync.
* Unfocused games keep running without slowing down; input blocked when unfocused.
* Server: decodes all traffic, entity registry with owners and positions, object id blocks per
  client, server-owned cars/peds (`spawn`/`despawn`, re-created if they disappear).
* **Server as host (`host = 1`)**: with two clients, both join the server, connect to each other,
  see each other and sync cars and peds (host11). Getting in a car and steering show for the other
  player.

## Waiting on

* **host12 test** (current build, not yet tested): the server now confirms clone creates /
  removes (CMsgPackedReliables sections 1 and 3) and clone syncs (CMsgPackedCloneSyncAcks) like a
  player would. Expected to fix: a driven car not moving for the other player (cars only change
  owner to the driver once every player, including the server, has confirmed them). Test:
  `Server.exe --capture host12.bin`, two clients, one drives a car the other spawned or was near.

## Open issues / planned

1. Car ownership to the driver. host14: client 1 could drive (car 518), but client 1 still
   showed Test2 on an old bike while Test2 drove a car; cause not found (onesync-plan.md
   "host14"). host13: client 2 drives fine, client 1 can't (it keeps giving
   the car it drives away; see onesync-plan.md "host13"); the DLL now traces every GIVE_CONTROL
   (`controltrace.rs`), next test reads it from IVMP_Client.log. Earlier: host12 still failed (a bike frozen for the other player, both
   games owning it); caused by the server's broken ch7 sequencing, fixed after host12 (see
   onesync-plan.md "host12"). Test again. The server now also acks game events (0x4F -> 0x89) and
   array elements (0x86 -> 0x87) sent to it (after the host13 build; untested in game).
2. Complaints (netComplaintMsg 0x55): in host11 client 1 complained about the server host from
   340 s on, because the server's reliable chunk ordering was wrong and its connection errored
   (details in onesync-plan.md "Complaints and chunk sequencing"). Fixed in the server in both
   directions after the host12 build; untested in game. The server still ignores complaints.
3. NPCs that despawn and respawn: ped <-> dummy ped switching runs as remove + create (host14,
   object 491 every few seconds); the DLL now logs received syncs (`synctrace.rs`) and creates
   per object. Population: both games spawn ambient traffic and peds around their own player, so cars stack
   and explode when players are close. Plan: the server picks one game per area to spawn
   population and turns it off for the others; as a safety net refuse creates on top of an
   existing networked vehicle.
4. Players leaving a server-hosted session: the server now sends the others
   snMsgRemoveGamersFromSessionCmd (layout in onesync-plan.md). Untested in game: quit one client
   (or let it time out) and check the other drops its player.
5. Server-host game modes: the host replays Free Mode templates only.
6. Deferred: gamemode -2 server scripting (Scripting crate), FiveM-style.
7. Positions: only the first entity of each clone sync is decoded.

## Key facts and addresses (Ghidra space)

* Network state 0x17F5EA4 (8 lobby, 10 in game); boot patches in `boot.rs`; network game config
  {16,_,32} via pattern; player slot patch at Player_Create 0xBB53FE (server host: slot `32 - id`,
  server is slot 1/0 in lists).
* Object id ring 0x19360C8 / write 0x19392C8 / read 0x19392CC / count 0x19392D0; server blocks:
  client c gets `c*490 .. +490`.
* Packet `[flags][nonce u32][payload][mac u32]`; RC4 calls 0x6D2200/0x6D25B6 skipped; MAC check
  jnz at 0x6D258E nopped. Frame and chunk layout: `docs/onesync-plan.md`.
* Message ids (CMsgCloneSync 0x8A, PackedCloneSyncAcks 0x8B, PackedReliables 0x8C, PeerData
  0x90, HostData 0x92, snMsgJoinRequest 0x41, AddGamer 0x64, ...): table in onesync-plan.md.
* Factories: vehicle 0x166D9FC slot 1 create 0x5DA990 / slot 2 destroy 0xB59180; ped 0x166D9F4
  slot 5 create 0x5C3770 / slot 6 destroy 0xB58EA0; net object by id 0x4913C0 on 0x1908EF0.
* Clone create handler 0x498430; AddGamer handler 0x6245C0, validation 0x623D80, block reader
  0x626E70.

## Other docs

* `docs/onesync-plan.md`: everything about sync, the server as host (templates, findings,
  test history host1..host11), settings.
* `docs/networking-plan.md`: the tunnel/relay design.
* `docs/libertym-notes.md`: what FiveM's GTA IV code (LibertyM) does and what IVMP took from it.
* `docs/boot-flow-research.md`: boot-skip research.

## Decision (2026-10-21): peer to peer for now

After host14/host15 (bike: steering synced, speed/movement didn't; NPCs despawning and respawning)
Drazox decided to stay with peer to peer networking for now, like FiveM's GTA IV code (LibertyM):
the first player hosts, the server relays. `host = 0` is the default (code, server.cfg template,
and the local server.cfg). The server-host work (Server/Host, `host = 1` / `--host`) and its
diagnostics stay in place for later; findings are in onesync-plan.md.

## Game modes (2026-10-21)

`gamemode` in server.cfg (Drazox's list in the template): -2 IVMP freeroam (Free Mode underneath,
no scripting yet), -1 the game's lobby, 0..16 the game's own modes (16 = Free Mode; the others are
passed to NETWORK_HOST_GAME / FIND_GAME, untested).

`-1` (first attempt, untested in game): when the server's settings arrive (before the session
loads) `boot.rs use_game_lobby` undoes "no startup script" and applies ONLY_SKIP_STORY_SCRIPT, so
`network_startup` runs and shows the game's lobby. The first player (the server's host) lets the
lobby host and pick the game (`lobby.rs` passes the script's own host arguments); everyone else
joins that session through hostsys as in peer to peer, with no lobby script of its own (in the
first test cl2's network_startup ran alongside IVMP's join, the join failed and the game thread
hung in the join code at 0x47CF9F), and doesn't start the session: the host's lobby does. The host
is forced unranked (the script passed ranked 44037632). Object ids and the spawner still run.
Second test: the joiner joined the host's session but stayed out of the lobby (1/32): the lobby's
player list needs the joiner's own lobby script. Now the joiner starts network_startup by hand
(`lobby.rs start_network_startup`, the game's own launch calls from 0x94CA43) once it is in the
host's session; its host call stays blocked and IVMP no longer launches it into the world.
Third test: the joiner's network_startup called CREATE_PLAYER although IVMP had made the player
at load; the second CreatePlayer crashed (null + 0xA60 at 0x9F19C8). The native's CreatePlayer call
(0xBB339D) now returns the existing player when there is one (boot.rs create_player_from_script).
Fourth test: network_main on the joiner called NETWORK_JOIN_GAME again; the join refused (already
in a session) and its failure path (0x47D9F1 -> 0x47CDF0) deadlocked. Script calls of
NETWORK_JOIN_GAME now report success while IVMP has joined (hostsys script_join_game, via
`nativelog::set_override`, which redirects a native for scripts only).
Fifth test: both games reach the Free Mode lobby (network state 8, freemode_cr on both, "Peer X
has joined" both ways) but neither lists the other: host 1/32, joiner 0/32 (not even itself). The
host's engine log dirties PLAYERINFO element 0 at start, the joiner never dirties its element
(31). Only the host's scripts called PLAYER_WANTS_TO_JOIN (0x7D99343C) and
TELL_NET_PLAYER_TO_START_PLAYING (0x465D424D). Next build logs both (args, answers) and the
players (`log_players`) in the lobby states.
Sixth test: the host's lobby did let the joiner in (PLAYER_WANTS_TO_JOIN(1) -> true,
TELL_NET_PLAYER_TO_START_PLAYING(31, 0)), but the joiner had no network object ids: they came at
state 6, entering the lobby (state 8) emptied the ring and hostsys only refilled at state 10, so
the joiner's player ped stayed queued for networking (no net object) and no lobby listed it.
`give_object_ids` now refills whenever the network state moved on and the ring is empty.
If the mode arrives after the session started loading the log says "no lobby this time".
Seventh test: the lobby works (both players listed, Free Mode played together). Changing the game
type in the lobby (Deathmatch, Team Deathmatch) left the host on a loading screen: the lobby stores
the new match settings (native 0x1DFF5B06 -> 0x47B990, pending config 0x18B5FE4, flag 0x17F5FCB),
ends the session and requests the network game again (native 0x1BC5050E -> 0xBDC960 ->
GameMode_Request(1)), which IVMP's "game mode stays network" patch ignored. GameMode_Request is now
filtered instead (boot.rs `game_mode_request`): requests for single player (0) are ignored, network
ones go through; hostsys starts over on the reload. The joiner stays in the old, ended session's
lobby (network state 8); it doesn't follow the host to the new session yet.
Eighth test: the host's reload works (new lobby in the new mode). The joiner's own lobby script
noticed only ~30 s later (session timeout), then left and reloaded by itself; its join to the
host's new session then failed at step 1 and the join stayed pending. Now (protocol 6): the host's
reload sends ClientMessage::SessionRestart, the server passes ServerMessage::SessionRestart { host }
to the others, and a joiner in the host's lobby leaves (NETWORK_LEAVE_GAME 0x55D66E24) and reloads
(GameMode_Request(1)) right away, then finds and joins again.
Ninth test: following worked up to the join (left, reloaded, found and joined within ~5 s), but the
join stayed pending at step 1: the host read the joiner's packets and answered nothing, and the
joiner received nothing more. The relay was only set up once (`relay::start` returned on later
loads); it now re-binds the socket and sets "relay active" on every load and logs both before. The
10 s join retry started a second join on top of the first, which was refused and deadlocked
(0x47CDF0); a stuck join now leaves first (NETWORK_LEAVE_GAME) and StartJoining waits while the
network state is 12/13 (leaving).
Tenth test (capture lobby1.bin): the relay was fine (still active after the reload). The rejoin's
connection check and key exchange were answered, but the host never answered rlSessionDetailRequest
(five tries, then the join fails at step 1). Its handler (0x6CACC0) stays silent while the flag
0x18B82CF is clear: rline's shutdown (0x6DB9B0 -> 0x6CA2C0) clears it when the network game is torn
down for the reload, and rline's init (0x6DB890, from network init 0x480DD0) only runs at game start.
After every session load hostsys now calls the service's init (0x6CA1B0) again if the flag is clear;
the network update (0x6CA3D0) then registers the handlers by itself.
Eleventh test: the flag was still set after the reload (no shutdown), and the capture shows the same:
connection check and key exchange answered, every session detail request ignored, even one byte for
byte like the answered first one. So the handler isn't reached: the teardown seems to drop the
connection manager's handler registrations while the service's slots stay marked as registered
(0x19F33A8 + 0x30n, manager at 0x19F36A0 + 0x1Cn), and 0x6CA3D0 never registers them again. On
every reload hostsys clears those markers (logging them and the manager at 0x18B82E4) so it does.
Drazox: the joiner tries to join before the host's new lobby exists. Now (protocol 7) the host
sends ClientMessage::SessionReady once its new session has been in the lobby (network state 8) for
2 s after a restart; the server passes ServerMessage::SessionReady { host } on, and a joiner that
followed waits for it (up to 60 s) before finding and joining.
Twelfth test: the follow works (re-registration + ready signal): cl2 left, reloaded, waited, found and
joined the host's new session (`join task finished, success 1`, host engine: "Peer Test2 has joined").
But cl2's lobby was its own: the host admits the joiner in a new slot each time (TELL_NET_PLAYER_TO_
START_PLAYING 31, 30, 29, 28) while cl2's game always creates its player in slot 31 (boot.rs
`player_slot`), so the host lists cl2 where no player exists. The host picks the slot in 0x93F3C0: it sorts the 32
slots by a "last used" stamp (`[0x11A8888 + 4 * slot] = ++[0x11A4FC4]`, written when a CPlayerInfo is
created in the slot by 0x93E8C0 / 0x93E960) and takes the first slot with no CPlayerInfo (or one marked
left, +0x4E8 == 6). So a slot that held a player counts as recently used and the next joiner gets the
next one down, although 31 is empty again (confirmed: the host's table held only slot 0 at each
admission; an earlier guess, the reservation table at 0x19392E0, was always empty). hostsys now
resets the stamps of slots without a CPlayerInfo whenever we host a freshly loaded session
(`host: player slot stamps [...] -> [...]`) and logs the stamps in the players line, so the first
joiner gets slot 31 again, which is what the joiner's player uses (boot.rs `player_slot`).
