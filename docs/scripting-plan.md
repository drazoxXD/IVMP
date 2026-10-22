# Scripting, FiveM style

Goal: servers run **resources** (folders with scripts and UI) written in Lua, like FiveM. A resource
has server scripts, client scripts (sent to the players) and a UI page, talks to the other side with
events, and calls the game's natives on the client. Official game modes (`gamemode >= 0`) stay pure
peer to peer; scripting is for IVMP's own modes (`-2` freeroam and later others).

## Resource layout

```
resources/
  freeroam/
    ivmp.lua           the manifest (Lua, evaluated in a sandbox)
    server.lua
    client.lua
    ui/index.html
```

`ivmp.lua`:

```lua
author 'Name'
description 'What it does'
version '1.0.0'
server_scripts { 'server.lua' }       -- also: server_script 'a.lua'
client_scripts { 'client.lua' }
files { 'ui/index.html', 'ui/app.js' }
ui_page 'ui/index.html'
dependencies { 'other_resource' }
```

`server.cfg`: `start = freeroam` (one line per resource, in start order), console: `start`, `stop`,
`restart`, `ensure <resource>`, `resources`.

## The Lua environment

Each resource runs in its own Lua state (Lua 5.4, `mlua`), sandboxed: no `io`, `os` (except
`os.time/date/clock`), `debug`, `package`, `dofile`, `loadfile`; `require` only loads files of the
resource. Values cross between resources and over the network as a neutral variant (nil, boolean,
number, string, table), serialised as JSON on the wire.

Common (both sides): `print`, `GetCurrentResourceName`, `AddEventHandler`, `RemoveEventHandler`,
`RegisterNetEvent`, `TriggerEvent`, `CreateThread`, `Wait`, `SetTimeout`, `GetGameTimer`,
`LoadResourceFile`, `exports`, `json.encode/decode`.

Server: `RegisterCommand(name, handler, restricted)`, `GetPlayers`, `GetPlayerName`,
`GetNumPlayerIndices`, `DropPlayer`, `TriggerClientEvent(name, target, ...)` (target -1 = everyone),
`source` inside net event handlers, events `playerConnecting`, `playerJoining`, `playerDropped`,
`onResourceStart`, `onResourceStop`.

Client: `TriggerServerEvent(name, ...)`, natives (`Citizen.InvokeNative`-style plus named wrappers),
`SendNUIMessage`, `RegisterNUICallback`.

## Stages

1. **Server runtime.** Resource loader and manifest, Lua states, events, threads and timers,
   commands, `exports`, player connect/drop events, `start/stop/restart` from `server.cfg` and the
   console. Sample resource. (Server only, no client scripts yet.)
2. **Network events.** `TriggerServerEvent` / `TriggerClientEvent` as reliable messages
   (protocol 10), `RegisterNetEvent` gate, `source`.
   Done: arguments travel as a JSON array (at most 900 bytes, 100 events per second per player),
   `TriggerClientEvent(name, player|-1, ...)` reaches the client queue (`net::take_script_events`),
   `playerConnecting` with `SetKickReason` + `CancelEvent` refuses a join. The client console has
   `triggerserver <event> [json]` for testing until the client runtime (stage 3) exists.
3. **Resource distribution and the client runtime.** (Done, protocol 11: the server announces
   `ivmp.lua`, client scripts, `files` and the `ui_page` with SHA-256 hashes, the client downloads
   what its `cache` folder lacks in 800 byte chunks over the reliable stream, verifies and starts the
   resource; start/stop/restart reach clients live. Client Lua has a 30M instruction budget per call.) The server sends client scripts and files
   (hash, chunks over the reliable channel, local cache); the client DLL runs them in Lua on the
   game thread (`game_process`), with the same event API.
4. **Natives.** (Done: about 3000 natives with names and typed signatures. The table comes from the
   GTA IV script tooling's native lists (`natives.cfg` / `natives.h`, found in the ScriptHookDotNet
   sources, MIT; originally Alexander Blade's scocl): every name of it is registered in this game
   build with the same hash, and `Client/tools/gen_natives.py` turns it into
   `Client/Hooks/natives.json`, which is built into the DLL. Client Lua has
   `InvokeNative(name|hash, ...)`, `Natives.NAME(...)`, `DeclareNative(name, hash, args, ret)` and
   `NativeExists(...)`, e.g. `local x, y, z = Natives.GET_CHAR_COORDINATES(Natives.GET_PLAYER_CHAR(0))`:
   out parameters come back as return values. A `natives.json` next to the DLL adds or overrides
   entries (`{"NAME": {"hash": "0x...", "args": "iif", "ret": "b"}}`, signature letters in
   Hooks/src/luanatives.rs). F1 console: `lua <code>` runs Lua on the game thread, `nativetrace
   [seconds]` logs every native the scripts call, with name and arguments.)
   Original plan: Name to hash table for GTA IV's natives (generated), typed wrappers, a safe
   `InvokeNative` on the game thread, a script debug panel in the F1 console (the WIP entry).
5. **UI.** A resource's `ui_page` is served to the web layer as `ivmp://<resource>/...`;
   `SendNUIMessage` / `RegisterNUICallback` use the existing `ivmp.send` / `ivmp.on` bridge.
6. **Server list and extras.** Server list in the F1 menu, master list later, name tags and
   blips/checkpoints/labels for scripts, `GetPlayers` details, a resource manager command set.

Each stage ends in something that runs and is committed on its own.
