![](https://raw.githubusercontent.com/drazoxXD/IVMP/a4180cf9466a1b8862f157d2d35b2ad8afbd5687/.github/media/banner.svg)

<div align="center">

**A multiplayer mod for Grand Theft Auto IV, written in Rust.**

[![Latest release](https://img.shields.io/github/v/release/drazoxXD/IVMP?style=for-the-badge&label=release)](https://github.com/drazoxXD/IVMP/releases/latest)
[![Build](https://img.shields.io/github/actions/workflow/status/drazoxXD/IVMP/build.yml?style=for-the-badge&label=build)](https://github.com/drazoxXD/IVMP/actions/workflows/build.yml)
[![License](https://img.shields.io/github/license/drazoxXD/IVMP?style=for-the-badge)](LICENSE)
![Game](https://img.shields.io/badge/GTA%20IV-Complete%20Edition%201.2.0.59-orange?style=for-the-badge)

[Download](https://github.com/drazoxXD/IVMP/releases/latest) · [Getting started](#getting-started) · [Server setup](#running-a-server) · [Building](#building) · [Docs](docs)

</div>

---

## About

IVMP runs GTA IV's own network game over a server of your choice. Players connect through the IVMP server, which relays the game's packets, and the game thinks it is in an ordinary peer-to-peer session. That gives you every original mode, a freeroam like FiveM and GTA Connected, and the game's own multiplayer lobby, all on one server.

Based on the works of [LibertyM](https://github.com/citizenfx/fivem) and [IV-Network](https://github.com/GTA-Network/IV-Network).

**Supported version:** Grand Theft Auto IV Complete Edition ([Steam](https://store.steampowered.com/app/12210/Grand_Theft_Auto_IV_The_Complete_Edition/), 1.2.0.59). The Rockstar Launcher version is planned.

## Features

| | |
|---|---|
| **Freeroam** | IVMP's own freeroam on the game's Free Mode session, with players, vehicles and peds synced. |
| **Every game mode** | Deathmatch, Team Deathmatch, races, Mafiya Work and the rest of the game's network modes. |
| **Game lobby** | `gamemode = -1` opens GTA IV's own multiplayer lobby. The first player is the host and picks the mode; the others follow, and follow again when the host changes the mode or starts a new lobby. |
| **Peer to peer** | The first player hosts the game, the server only relays. The default. |
| **Server host** | The server hosts the session itself, so no player has to (experimental, `host = 1`). |
| **Linux server** | A Linux build for hosting panels such as Pelican. |

**Planned:** full scripting, and a server-side host without peer to peer.

## Getting started

1. Download `IVMP_Client.zip` from the [latest release](https://github.com/drazoxXD/IVMP/releases/latest) and unzip it anywhere.
2. Start `ivmp_launcher.exe`, point it at your GTA IV folder and the server address, and press play.
3. The launcher starts the game, loads the IVMP client and takes you straight online.

The window title shows `IVMP - Build: …` once the client is loaded.

## Running a server

Download `IVMP_Server.zip` (Windows) or `IVMP_Server_Linux.zip` (Linux) from the [latest release](https://github.com/drazoxXD/IVMP/releases/latest) and start `Server`. It writes a `server.cfg` next to itself the first time it runs.

```ini
bind     = 0.0.0.0:9999   # address and port (UDP)
host     = 0              # 0 = peer to peer, 1 = the server hosts (experimental)
gamemode = -2             # -2 IVMP freeroam, -1 the game's lobby, 0.. the game's own modes
slots    = 32
```

Open the UDP port in your firewall. The comments in the generated `server.cfg` list every game mode number.

**Pelican / Pterodactyl:** import an egg that downloads `IVMP_Server_Linux.zip` from the latest release and runs `./Server 0.0.0.0:{{SERVER_PORT}}`.

## Building

You need [Rust](https://rustup.rs) and the 32-bit Windows target for the client (the game is 32-bit).

```bash
rustup target add i686-pc-windows-msvc

# client: the DLL injected into the game, and the launcher
cd Client
cargo build --release --target i686-pc-windows-msvc -p Hooks
cargo build --release -p Launcher

# server
cd ../Server
cargo build --release
```

Releases are built by the [Build workflow](.github/workflows/build.yml), which is run by hand. Each release lists the commits since the previous one and how long each part took to build.

## Documentation

- [docs/onesync-plan.md](docs/onesync-plan.md): the server-host plan
- [docs/networking-plan.md](docs/networking-plan.md): how the game's networking works
- [docs/web-engine-research.md](docs/web-engine-research.md): the web UI engine (WebView2) research and spike plan
- [docs/boot-flow-research.md](docs/boot-flow-research.md): the game's boot flow, and the intro-skip patch note

## Images

<img width="3305" height="1082" alt="image" src="https://github.com/user-attachments/assets/7feb68ba-ef02-49a9-ab6c-7f3fac7a2a25" />
<img width="2503" height="1013" alt="image" src="https://github.com/user-attachments/assets/ad3550cd-de0a-4216-8c9a-e38630050fdd" />
<img alt="IVMP" src="https://raw.githubusercontent.com/drazoxXD/IVMP/main/.github/media/3.png" />

---

<div align="center">

IVMP is not affiliated with or endorsed by Rockstar Games or Take-Two Interactive. You need your own copy of Grand Theft Auto IV.

</div>
