# GTA IV boot flow: research for direct-to-online boot

Target: `GTAIV.exe` 1.2.0.59 (Steam Complete Edition), x86, image base `0x00400000`, no ASLR.
SHA-256 `08759a5516f9837920ea504436236bbab89d0826a8e4d04ff106345177b5345d`.

All names below are already applied in the Ghidra project, with `IVMP:` comments on each function and on the patch sites.

## TL;DR

The game has no real "main menu" at boot. After the legal/profile/storage steps it either
**starts a new game** (runs the `startup` script, which is the story), **continues the autoload save**
(no startup script, player placed at a save house), or, on a special network boot, **switches to network mode**
and runs `network_startup`.

The cheapest way to land in a world we control is the "continue" path **without a save**:
`Game_LoadSession(episode 0, fromSave = true)` loads the full map and every engine subsystem but never launches a script,
then `Game_SpawnPlayerAfterLoad` puts the player down and fades in. Two byte patches get us there (see the hook plan).

## Code protection to know about

* Entry point `0x01EEB000` is in a small `.rkstr` stub; `.tbm` (1 MB) holds the protection payload.
* Some `.text` ranges are **encrypted on disk** and decrypted at runtime: at least `0x401680..0x401770`
  (tiny helpers such as `FUN_00401210`, which is used like `operator new`) and `0x481B20..0x482740`
  (profile / network helpers: `FUN_00481b20`, `FUN_00482150`, `FUN_00482720`, `FUN_00482730`, `FUN_004821b0`).
  Ghidra shows these as garbage. To read them we need a dump taken after the game has started.
* Ghidra also left large regions undefined (no function created), including the app-state code that calls
  `Boot_InitialLoadFlow` (`0x59d65f`, `0x59d679`) and the per-frame code that calls `GameMode_ProcessPendingChange` (`0x5c26b2`).
* Everything this plan patches sits in plain, unencrypted code. Our DLL is injected with `CreateRemoteThread` after the
  process is running, so decryption has already happened by then. Whether the stub checksums plain code is unknown; test it.

## Key globals

| Address | Name (Ghidra) | Meaning |
|---|---|---|
| `0x011D6FC4` | `g_GameModeCurrent` | 0 = single player, 1 = network |
| `0x011D6FC8` | `g_GameModeRequested` | mode applied on the next `CGame_InitSession` |
| `0x011D6FCC` | `g_GameModeChangePending` | byte, set by `GameMode_Request` |
| `0x011D6FD0` | `g_EpisodeNext` | episode being loaded (0 = IV, 1 = TLAD, 2 = TBoGT) |
| `0x011D6FD4` | `g_EpisodeCurrent` | episode in use |
| `0x011D6FDC` | `g_EpisodeSinglePlayerSaved` | SP episode to return to after MP |
| `0x011D6FE0` | `g_EpisodeNetwork` | episode used for a network session |
| `0x01032110` | `g_PlayerSpawnPointIndex` | spawn table index used after load, -1 = origin |
| `0x012DDB00` | spawn table | stride 0x40: x, y, z, w, heading (float), zone name |
| `0x011730F8` | `g_SavegameReadStatus` | 2 = a save was read into the buffer |
| `0x018B6148` | `g_BootNetworkKind` | 2 = boot straight into network mode (no static writer found) |
| `0x01030B7C` | `g_FrontendActive` | 1 while the pause/frontend menu is up |
| `0x01160C24` | `g_FrontendPendingAction` | frontend action id |
| `0x01160C40` | `g_FrontendCurrentScreen` | `SCR_*` screen id (name table at `0x00F8D828`) |

`MENUOPT_*` names are at `0x00F8DAD0` as `{id, char*}` pairs: `JUMP_NEWGAME = 0x10`, `START_EPISODE = 0x12`,
`HOST_GAME = 0x16`, `JOIN_GAME = 0x17`, `TITLE_START = 0x24`.

## Functions

Signatures are unique in the 1.2.0.59 file and were checked with a scan of the on-disk image.
Some of the short ones run into the padding after the function, so re-check them if you port to another version.

| Address | Name | Signature |
|---|---|---|
| `0x0059D550` | `GameThread_Main` | `A1 ?? ?? ?? ?? 83 EC 0C 53 55 56 57 8B F9 3B C7` |
| `0x005C1B70` | `Boot_TitleScreenWait` | `51 53 55 56 57 33 FF E8 ?? ?? ?? ?? 8A F8 E8 ?? ?? ?? ?? 6A` |
| `0x005C2670` | `CGame_Process` (per frame, game thread) | `55 8B EC 83 E4 F0 83 EC 18 B9 ?? ?? ?? ?? 53 56` |
| `0x005C1D20` | `Boot_InitialLoadFlow` | `83 EC 08 53 56 57 E8 ?? ?? ?? ?? 8B 1D ?? ?? ?? ?? ?? ?? ?? ?? FF` |
| `0x005C2060` | `Boot_TickFrame` | `56 E8 ?? ?? ?? ?? 8B 35 ?? ?? ?? ?? ?? ?? ?? 7C` |
| `0x008CB230` | `Savegame_InitBootState` | `B9 ?? ?? ?? ?? E8 ?? ?? ?? ?? 8B 0D ?? ?? ?? ?? C7 05 ?? ?? ?? ?? FF` |
| `0x008C7010` | `Boot_ProfileSignInStep` | `51 56 33 F6 83 3D ?? ?? ?? ?? FF` |
| `0x008C7540` | `Boot_StorageDeviceStep` | |
| `0x008C6F90` | `Boot_CheckNetworkBootStep` | |
| `0x008C71F0` | `Boot_FindAutoloadSaveStep` | |
| `0x008C7670` | `Savegame_RequestLoadSlot` | |
| `0x008C8F90` | `Savegame_ProcessLoad` | returns 0 = loaded, 1 = busy, 2 = failed |
| `0x005C1980` | `Game_LoadSession` | `83 EC 08 53 56 57 8B 3D ?? ?? ?? ?? 8A FA 8B F1` |
| `0x005C1360` | `CGame_InitSession` | `51 53 8B D9 8B 0D ?? ?? ?? ?? 56 E8 ?? ?? ?? ?? 8B` |
| `0x0094C470` | `CTheScripts_InitAndLaunchStartup` | `55 8B EC 83 E4 F0 83 EC 18 ?? ?? ?? ?? 68 ?? ?? ?? ?? ?? ?? ?? AB` |
| `0x0094B5A0` | `CTheScripts_FindScriptIndexByName` | |
| `0x00BDE840` | `CTheScripts_GetProgramByIndex` | `8B 0D ?? ?? ?? ?? 8B 54 24 04 8B 41 04 F6 04 02 80 74 08 ?? ?? ?? ?? 8B` |
| `0x0086EE60` | `CTheScripts_StartNewThread` | `53 55 8B 6C 24 0C 56 85 ED 74 63 8B 5C 24 1C 8B` |
| `0x008C7E30` | `Savegame_ApplyLoadedData` | `83 3D ?? ?? ?? ?? 02 ?? ?? ?? ?? 68 ?? ?? ?? ?? E8 ?? ?? ?? ?? 83 C4 08 84` |
| `0x005C1660` | `Game_CommitEpisodeSwitch` | `A1 ?? ?? ?? ?? 56 57 8B 3D ?? ?? ?? ?? A3 ?? ?? ?? ?? A3` |
| `0x008C7FC0` | `Game_SpawnPlayerAfterLoad` | `55 8B EC 83 E4 F0 83 EC 18 A1 ?? ?? ?? ?? 56 57 0F` |
| `0x00942B20` | `GameMode_Request` | `8B 44 24 04 A3 ?? ?? ?? ?? C6 05 ?? ?? ?? ?? ?? C3` |
| `0x009431D0` | `GameMode_ForceNetworkPending` | |
| `0x00942720` | `GameMode_GetCurrent` | |
| `0x00942730` | `GameMode_GetRequested` | |
| `0x00942840` | `GameMode_IsNetworkGame` | `33 C0 83 3D ?? ?? ?? ?? ?? 0F 94 C0 C3` |
| `0x009428A0` | `GameMode_IsChangePending` | |
| `0x009427D0` | `GameMode_ApplyRequested` | `A1 ?? ?? ?? ?? C6 05 ?? ?? ?? ?? 00 C7 05 ?? ?? ?? ?? 00 00 00 00 C7` |
| `0x005C2380` | `GameMode_ProcessPendingChange` | `83 EC 08 56 E8 ?? ?? ?? ?? 84 C0 ?? ?? ?? ?? 00` |
| `0x005C22F0` | `Game_StartNetworkSession` | `56 6A 01 8B F1 E8 ?? ?? ?? ?? 8B 0D ?? ?? ?? ?? 83` |
| `0x005C21E0` | `Game_ReturnToSinglePlayerFromNetwork` | `51 8B 0D ?? ?? ?? ?? 53 56 83 F9 FF 74 15` |
| `0x005C2340` | `Game_RestartSinglePlayer` | `51 8B 0D ?? ?? ?? ?? 83 F9 FF 74 15` |
| `0x005A6D90` | `CMenuManager_Process` | `55 8B EC 83 E4 F0 83 EC 68 A1 ?? ?? ?? ?? 33 C4 89 44 24 64 80` |
| `0x005A3980` | `CMenuManager_ProcessPendingAction` | |
| `0x008BE0E0` | `CMenuManager_SetPendingAction` | |
| `0x00934B20` | `CEpisodes_IsEpisodeAvailable` | thiscall on `*(0x01BB5624)` |

### `Game_LoadSession` calling convention

```
bool Game_LoadSession(int episode      /* ECX */,
                      bool fromSave    /* DL  */,
                      bool loadingUp   /* [esp+4] */,
                      bool forceReload /* [esp+8] */);   // returns AL, caller pops 8 bytes
```

This is not standard `__fastcall` (the callee does a plain `RET`), so call it from Rust through a small naked asm thunk.
`fromSave` is passed on to `CGame_InitSession`, and when it is true the startup script is not launched.

## Boot flow

```
GameThread_Main 0x59D550   (ECX = watchdog deadline; past it the game TerminateProcess'es itself)
 ├─ wait for early init (loading thread)
 ├─ benchmark boot (DAT_0110e6a0 != 0): pick episode, Game_LoadSession directly, no title/boot flow
 ├─ Boot_TitleScreenWait 0x5C1B70   (frontend action 0xE, ticks until action 5/6 = title confirmed)
 ├─ Boot_InitialLoadFlow 0x5C1D20   (below)
 └─ main loop: CGame_Process 0x5C2670 + render/streaming, until quit
```

```
Boot_InitialLoadFlow 0x5C1D20
     ├─ Savegame_InitBootState
     ├─ loop: Sleep(1); Boot_TickFrame; switch(ESI)
     │    0 Boot_ProfileSignInStep     → 1 or 4
     │    1 Boot_StorageDeviceStep     → 2
     │    2 Boot_CheckNetworkBootStep  → 4, or 8 (network boot)
     │    4 Boot_FindAutoloadSaveStep  → 7 (no save) or 5
     │    5 Savegame_RequestLoadSlot   → 6
     │    6 Savegame_ProcessLoad       → 9 (loaded) or 7 (failed)
     ├─ common block @0x5C1DEB (frontend/fade reset)
     ├─ ESI 8 & g_BootNetworkKind==2 → GameMode_ForceNetworkPending; Game_StartNetworkSession(1)
     ├─ ESI 7 → Game_LoadSession(ep, fromSave=0, 1, 0)          // NEW GAME, story "startup" script
     └─ ESI 9 → Game_LoadSession(ep, fromSave=1, 1, 0)
                 Savegame_ApplyLoadedData() != 2 →
                     Game_CommitEpisodeSwitch; Game_SpawnPlayerAfterLoad(1); fade in
                 else → Game_LoadSession(ep, 0, 0, 1)            // save broken, new game
```

`Game_LoadSession` → `CGame_InitSession(fromSave)` re-inits every subsystem (world, streaming, peds, vehicles, audio, etc.),
calls `GameMode_ApplyRequested` (current mode = requested), then `CTheScripts_InitAndLaunchStartup(fromSave)`, which
launches `network_startup` if `GameMode_IsNetworkGame()` and `startup` otherwise, but only when `fromSave == 0`.

## Frontend and the SP / MP switch

* `CMenuManager_Process` runs each frame (also during boot) and calls `CMenuManager_ProcessPendingAction`,
  a switch on `g_FrontendPendingAction`.
* Actions 4 and 5 are "go to multiplayer": `g_EpisodeNetwork = FUN_00482150()` (encrypted) then `GameMode_Request(1)`.
* `GameMode_ProcessPendingChange` (per frame, after the fade) sees the pending change and calls
  `Game_StartNetworkSession` for mode 1, or `Game_ReturnToSinglePlayerFromNetwork` / `Game_RestartSinglePlayer` for mode 0.
* `Game_StartNetworkSession` loads `g_EpisodeNetwork` with `fromSave = 0`, so `network_startup` runs. That script expects
  a live Rockstar network session (lobby, game config); the session code lives partly in the encrypted range and is not mapped yet.

## Proposed hook plan

### Option A (recommended): single-player engine, empty world

Run the engine in single-player mode and do all networking ourselves, as the original IV:MP did.
The engine never knows it is "online", so nothing depends on Rockstar's session code.

1. **Skip profile, storage and save selection.** Patch the boot loop head so it goes straight to state 9.
   * Site `0x005C1D33`, sig `6A 01 FF D3 E8 ?? ?? ?? ?? 83 FE 06 0F 87`.
   * Write `E9 AE 00 00 00` (`jmp 0x5C1DE6`, which is `mov esi, 9` followed by the common block).
   * Result: `Game_LoadSession(ep, fromSave=1, 1, 0)` with no save read, so `Savegame_ApplyLoadedData` returns 0 and the
     game commits the episode, spawns the player and fades in. No story script runs.
2. **Belt and braces: never launch `startup` / `network_startup`.**
   * Site `0x0094CA41`, sig `80 7D 08 00 C6 05 ?? ?? ?? ?? 00 75 64 E8`; change `75 64` to `EB 64`.
   * Needed anyway for any later reload that passes `fromSave = 0` (frontend "new game", death/arrest restarts).
3. **Force episode 0.** The state-9 exit picks the episode from `0x01173240` / `0x01031F28` (both rewritten by
   `Savegame_InitBootState`, so writing them early doesn't stick). Patch the 20 bytes at `0x005C1EFE`
   (sig `80 3D ?? ?? ?? ?? 00 8B 0D ?? ?? ?? ?? 0F 45 0D ?? ?? ?? ?? 6A 00 6A 01 B2 01`) to `33 C9` + 18 NOPs (`xor ecx, ecx`).
4. **Create the player ourselves** (required, verified in game). Without the startup script nothing creates the
   player ped and the first world update crashes on `FindPlayerPed() == null` (`0xB00065`). The DLL redirects the
   `call Game_SpawnPlayerAfterLoad` at `0x5C1F84` and the `call CGame_Process` at `0x59D7D8`; both call
   `Player_Create(float* xyz)` (`0xBB53B0`, what the `CREATE_PLAYER` native uses) if there is no player, and the
   spawn hook also fills spawn point 0 so the game's own placement and fade-in work. Signature scanning is limited to
   readable `.text` pages; scanning the protection sections crashed the game once.
5. **Control the spawn.** Set `g_PlayerSpawnPointIndex` to -1 and teleport the player from our code after the fade, or hook
   `Game_SpawnPlayerAfterLoad` and fire the event bus "world ready" event when it returns.
6. **Skip the legal / Rockstar logo intro (not applied: suspected of crashing; with it enabled the multiplayer lobby / session doesn't start, seen 2026-10-22).** These are entries in `common:/DATA/LOADINGSCREENS_PC.DAT`
   (parsed by `LoadingScreens_LoadDat` `0x5CE120`, ticked by `LoadingScreens_Update` `0x5CC760`). `GameThread_Main`
   waits `while (g_LoadingScreensActive && g_LoadingIntroPlaying)`. Patch `0x0059D5A7` `74 74` -> `EB 74`
   (sig `80 3D ?? ?? ?? ?? 00 74 74 80 3D ?? ?? ?? ?? 00 74 6B E8`, +7) to not wait. The loading screen may still
   show the legal image while the world loads; that's cosmetic.
7. **Skip the title screen.** In `Boot_TitleScreenWait`, queue no frontend action instead of `0xE`
   (`0x005C1BBB` `6A 0E` -> `6A 00`, sig `33 F6 6A 0E 89 74 24 14 E8`, +2) and NOP the loop back-edge at `0x005C1C67`
   (sig `83 FE 01 0F 85 64 FF FF FF 6A 00 B9`, +3, six bytes `90`) so it ticks one frame and continues.
   Patches 5 and 6 are in the DLL but untested; if the game misbehaves, these are the first to switch off.
8. **Keep the player out of story UI.** Block the frontend's new game / load / episode actions by hooking
   `CMenuManager_SetPendingAction` and dropping those ids, and disable autosave later.

Timing: the DLL must apply patches 1 to 3 before the game reaches `Boot_InitialLoadFlow`. Injection right after process
start should be early enough (the legal screens and loading take seconds), but the launcher could also create the process
suspended, wait for the `.rkstr` stub to finish, and inject before the boot flow. Patch 2 can be applied at any time
before the first `CGame_InitSession`.

### Option B: real network mode

Set `g_BootNetworkKind = 2` and force `Boot_CheckNetworkBootStep` to return 2 (state 8), which calls
`GameMode_ForceNetworkPending` and `Game_StartNetworkSession(1)`. The engine then runs with `g_GameModeCurrent == 1`
and launches `network_startup`. This gives us the engine's own network object code (`CNetObj*` sync) for free, but it
needs a working Rockstar session object, which is in code we can't read yet. Not recommended as the first step.

## Open questions and next research

* Dump the decrypted `.text` at runtime (after the menu shows) and load it into Ghidra, so the encrypted helpers
  (`FUN_00482150` network episode, `FUN_00482720/730` network boot checks, `FUN_00481b20`) can be read.
* Ghidra still misses functions; the MCP can't create them, so press F in Ghidra where needed (`0x5C2510`, the
  reload-after-frontend state machine on `DAT_01bb564c`, is the next one).
* Measure what boot time is left after the patches; most remaining time should be real world loading.
* Hook `CGame_Process` for a per-frame game-thread event (safer than the render thread for touching peds and vehicles).
  Keep work there short: the game thread watchdog kills the process if a frame stalls past its deadline.
* Check whether `Game_SpawnPlayerAfterLoad` with no save leaves wanted level, weather, clock and the phone in a sane state.
