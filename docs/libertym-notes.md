# How FiveM's GTA IV code (LibertyM) runs native multiplayer

Source: the public `citizenfx/fivem` repository still contains LibertyM, Cfx.re's GTA IV framework (written for
GTAIV.exe **1.2.0.43**; ours is 1.2.0.59, so its hard-coded addresses don't apply, its patterns mostly should):

* `code/components/gta-net-ny/src/CoreNetworkingNY.cpp`: transport, peer addresses, session info
* `code/components/gta-net-ny/src/HostSystemNY.cpp`: host/join state machine (no lobby)
* `code/components/gta-game-ny/src/hooks/HookAlwaysNetwork.cpp`: boot straight into a network game
* `code/components/gta-core-ny/src/GameInit.cpp`: load/reload/kill-network helpers

https://github.com/citizenfx/fivem/tree/master/code/components

GTA Connected's "native multiplayer" for IV works the same way: Rockstar's own sync, relayed by the server, which
cannot read the sync packets (https://wiki.gtaconnected.com/NativeMultiplayer).

Below is a summary of what LibertyM does; no code is copied here.

## Transport

* IAT hooks on `ws2_32` `bind` / `sendto` / `recvfrom` (ordinals 2, 20, 17). The game socket is the one bound to
  port **6672**; with `-cl2` on the command line it binds **6673** instead (that is their second-client switch).
* Peers are addressed as fake LAN IPs: `192.168.x.y` where the low 16 bits are `netID ^ 0xFEED`. `sendto` to such an
  address becomes `RoutePacket(netID)` on their server connection; `recvfrom` returns queued packets as coming from
  `192.168.(netID ^ 0xFEED)`, port 6672. Broadcasts (255.255.255.255) are dropped.
* They also pump the engine's relay receive path every frame (feed packets as "relay packet received" events to the
  connection manager), and mark the relay as active.

Ours does the same at the `rage::netSocket` level (10.0.0.x) instead of ws2_32; either works.

## Identity and session info (no Rockstar services)

* `GetLocalPeerAddress` is replaced: local/relay/public address all = the fake LAN IP of our server net ID, port
  6672, `rockstarAccountId` = net ID, keys derived from a per-client "server base" value.
* The function that derives the session key from the session offer is replaced (deterministic `seed ^ 0xABCD`).
* Peer/session UUIDs are forced to 2 (host, joinee and migrate paths).
* **`ReadSession`** (what turns a matchmaking result into an `rlSessionInfo`) is replaced: it returns a session whose
  host peer address is the fake LAN IP of the host the server picked, token 2/2. So "find game" always finds the
  server's host, without Rockstar matchmaking.
* Host migration: when the session is copied on migrate, the client tells the server who the new host is.

## Boot into a network game

* The "next game is a network game" flag is forced to 1 on first launch, and the stored network game config
  (30 dwords) is set to `[0] = 16` (Free Mode), `[2] = 32` (slots), everything else 0.
* The `network_startup` lobby is not used at all.

## Host / join (their own state machine, via natives)

* Host: `NETWORK_HOST_GAME(16, false, 32, false, 0, 0)` (hash `0x5BEA05E2`), then wait while
  `0x391E4575` (operation pending) is true, then `0x1CA77E94` must be true (hosted).
* Join: `NETWORK_FIND_GAME(16, false, 0, 0)` (`0x5D4D0C86`), wait on `0x23D60810`, count with `0x10DF4CED`,
  `NETWORK_JOIN_GAME(0)` (`0x60806A0C`), wait on `0x76C53927`, success `0x59F24327`. If joining fails, host instead.
* In game, every frame: if `0x5C03585C` (local player ready to start playing) then `0x70FE415C` (launch local player
  in network game); for each player `i`, if `0x7D99343C(i)` (wants to join) then `0x465D424D(i, 0)` (tell them to
  start playing).
* The player is created by a script thread calling `CREATE_PLAYER` (`0x335E3951`).
* Other natives used: `0x18F43649` (filter menu on/off), `0x4E237943` (is network player active),
  `0x2E5E1600` (is host).

## Other patches worth taking

* Network population cycling allowed in MP; remove the "too slow / out of streaming memory" MP disconnect counters;
  don't unload streamed fonts (pause menu crash); `ExitProcess` -> `TerminateProcess` after telling the server.

## What this means for IVMP

* Free Mode = host mode **16** with **32** slots: confirmed (we found the same from the recorded lobby).
* Skip `network_startup` and drive host/join with these natives from our DLL (needs a small native invoker; we
  already map every handler to its hash).
* Replace "find game" / `ReadSession` so joiners join the host our server picked; that is the missing piece for a
  second client to join.
* `-cl2`: bind the game socket to 6673 for the second instance (plus the launcher-side single-instance block).

## Comparison with IVMP's port (2026-10-20)

| LibertyM | IVMP |
| --- | --- |
| Boots the first game as a network game (HookAlwaysNetwork) | Same (boot.rs network boot patches) |
| Fake Rockstar (ROS) server signs the player in and answers matchmaking | No ROS: hostsys fakes the sign-in fields, peer.rs answers "find game" locally |
| Peer address / keys / UUID / ReadSession hooks | Same (peer.rs) |
| ws2_32 bind/sendto/recvfrom + per-frame relay feed | Same idea (net.rs, relay.rs) |
| Host/join state machine with the same natives | Same (hostsys.rs) + NETWORK_START_SESSION |
| Creates the local player with CREATE_PLAYER(0, ...) from a script thread | Player_Create from the boot hook; slot per client |
| HookRemoveSpecNetChecks (no too-slow kicks) | Same (boot.rs) |
| Host-migration copy hook, server-base message 0xB3EA30DE | Not ported yet |
