# Networking plan: the engine's own multiplayer (Option B) through the IVMP server

Goal: real multiplayer where the engine's own network objects (`CNetObjPlayer`, `CNetObjPed`,
`CNetObjVehicle`, `CNetObjObject`, ...) do the syncing, and every packet goes through our Rust server.
This is the approach FiveM took for GTA V: keep Rockstar's netcode, replace the transport and the matchmaking.

## What is in GTAIV.exe 1.2.0.59

* **Network object classes:** all of them are still in the exe (RTTI): `CNetObjEntity`, `CNetObjDynamicEntity`, `CNetObjPhysical`,
  `CNetObjPed`, `CNetObjPlayer`, `CNetObjDummyPed`, `CNetObjVehicle` (+ Automobile, Bike, Boat, Heli, Plane, Train),
  `CNetObjObject`, each with its `...SyncData` class.
* **Sessions:** the newer rage stack. `snSession` / `rlSession` (host, join, migrate, `snEvent*`), presence and
  leaderboards from Rockstar Online Services, `netIce*` NAT traversal. No Games for Windows Live.
* **Transport:** plain UDP through one wrapper class, `rage::netSocket` (renamed in Ghidra):

| Address | Function | Notes |
|---|---|---|
| `0x006F18F0` | `rage_netSocket_Create` | fastcall `ECX = this`. `socket(AF_INET, SOCK_DGRAM, UDP)`, binds port `this+0x94`, local address read back into `this+0x88` (ip) / `+0x8C` (port), non-blocking. |
| `0x006F1A60` | `rage_netSocket_Send` | `bool __thiscall (netSocket*, const netSocketAddress* to, const void* buf, int len, int* err)` |
| `0x006F1AE0` | `rage_netSocket_Receive` | `int __thiscall (netSocket*, netSocketAddress* from, void* buf, int maxLen, int* err)`, returns bytes (0 = nothing) |

`netSocketAddress` is `{ u32 ip; u16 port; }`. These are the only `sendto` / `recvfrom` users, apart from a second copy at `0x6FAF2C` / `0x6FB03F`
(still to check: probably the Social Club / HTTP side).

* **Game mode switch:** already works from our DLL (`GameMode_Request(1)`, verified in game). The session reloads in network mode,
  our hooks create the player and fade in. No session exists yet, and `network_startup` is the script that would set up the MP ped and menus.

## Architecture

```
 client A (GTAIV + ivmp_hooks)            IVMP server (Rust)              client B
 engine netcode                           ─ lobby: players, host,        engine netcode
   │ netSocket::Send(to = 10.0.0.2)          virtual addresses             ▲
   ▼                                      ─ relay: forwards packets        │ netSocket::Receive(from = 10.0.0.1)
 hook → our UDP socket ──────────────────►  by virtual address  ─────────► hook
```

* **Virtual addresses:** the server gives each client a fake IP (`10.0.0.N`). The engine only ever sees those, so it treats the
  other players as normal LAN peers and its NAT traversal has nothing to do.
* **Tunnel:** the DLL redirects every call to `netSocket::Send` / `Receive`. Send wraps the engine's packet with the destination
  virtual address and sends it to the server. Receive hands back packets the server forwarded, with the sender's virtual address.
  The engine's own socket stays bound but unused.
* **Session:** instead of Rockstar matchmaking, the server picks the host. The host's DLL tells the engine to host a session; joiners'
  DLLs tell it to join the host's session (session info + host virtual address from the server). Once peers are in the session, the
  engine creates `CNetObjPlayer` for each and starts syncing on its own.
* **Server:** starts as lobby + relay only. It can later read the sync messages (it sees every packet) to become authoritative, keep
  state when the host leaves, and run server scripts.

## Milestones

1. **Session host/join without Rockstar services (research, gating).** Find `snSession` host and join, the `rlSessionInfo`
   layout, and what they call into Rockstar Online Services. Goal: host a session from the DLL with no ROS calls failing hard.
   Needs the decrypted dump (`out/debug/GTAIV_dump.exe`) for the parts that are encrypted on disk.
2. **Server lobby + relay.** Rust UDP server in `Server/NetProtocol`: join handshake (nickname, version), virtual address assignment,
   host election, packet forwarding. Small, testable on its own with two fake clients.
3. **Client tunnel.** Hooks on `netSocket::Send` / `Receive` / `Create` (local address = virtual address), connection to the server
   from `ivmp.json`'s `server`.
4. **Two clients, one session.** Host + join through the server; check that `CNetObjPlayer` objects appear on both sides.
   Needs the second-client switch (`-cl2` style) to test on one PC.
5. **Ped / vehicle / object sync.** Mostly the engine's work once 4 works; we fix whatever single-player assumptions break.
6. **MP ped and spawn.** Use the multiplayer player models and our own spawn logic instead of `network_startup`'s menus.

## Open questions

* Does `snSession` refuse to host without a signed-in Social Club profile? If so we fake the signed-in state (rlPresence / gamer info).
* Packet size: `Receive` reads up to 0x400 bytes; our wrapper header must fit, or we split.
* Where the engine stores its own peer address for the session (likely from `netSocket` `+0x88`), so it advertises the virtual one.
