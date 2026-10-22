//! Settings: `server.cfg` next to the executable (written with defaults when missing), then the
//! command line on top: `Server [bind address] [--capture <file>] [--ids <id,id,...>] [--host]`.
//!
//! `server.cfg` is `key = value` lines, `#` starts a comment:
//!
//! * `bind = 0.0.0.0:9999`
//! * `host = 0` (peer to peer, the default: the first player hosts, the server relays) or `1`
//!   (the server hosts the game session as client 254; every game joins it and no player hosts;
//!   Host crate; experimental)
//! * `gamemode = -2`: -2 = IVMP's freeroam (what IVMP runs now), -1 = the game's own multiplayer
//!   lobby (clients let `network_startup` run and don't host or join themselves), 16 = GTA IV's
//!   own online Free Mode, other values = the game's other network modes. IVMP's own modes
//!   (other negatives) run on the game's Free Mode session underneath.
//! * `slots = 32`: player slots of the session.
//! * `capture = file`: record game packets (empty = off).
//!
//! `--ids` hands out these client ids, in join order, before the usual lowest free id. Used to
//! record a session whose host has the id the server will use as host (254), for the server
//! host work (docs/onesync-plan.md). `--host` is `host = 1`.

use std::net::Ipv4Addr;
use std::path::PathBuf;

use NetProtocol::DEFAULT_PORT;

pub const FILE: &str = "server.cfg";

pub struct Config {
    /// Address the UDP socket binds to.
    pub bind: String,
    /// Where to record game packets (`--capture`).
    pub capture: Option<String>,
    /// Client ids to hand out first (`--ids`).
    pub ids: Vec<u8>,
    /// The server hosts the game session (`host = 1`, `--host`).
    pub host: bool,
    /// -2 = IVMP freeroam, 16 = GTA IV's online Free Mode, other values = the game's modes.
    pub gamemode: i32,
    pub slots: u8,
    /// Also log debugging detail (entities, game events).
    pub verbose: bool,
    /// Ask the router (UPnP) to forward the server port.
    pub upnp: bool,
    /// Where the resource folders are (`resources` in server.cfg), next to the executable by default.
    pub resources: Option<String>,
    /// Resources to start, in order (`start = name` lines).
    pub start: Vec<String>,
}

const DEFAULT_FILE: &str = "\
# IVMP server settings. Lines are `key = value`; `#` starts a comment.

# Address and port the server listens on.
bind = 0.0.0.0:9999

# Ask the router to forward the port (UPnP), so players outside your network can join.
# Off by default; turn it on for a server at home. The router must have UPnP enabled.
upnp = false

# Is the server or are the clients the host?
# The server tends to be the best, but for game modes you need peer to peer.
# 1 = server: scripting like in FiveM, the server is the host and clients are just clients
# 0 = peer to peer: the server is only a relay, game modes, no scripts, a client is the host
#     (the game's own peer to peer sessions)
host = 0

# Game mode:
# -2 IVMP FreeRoam with IVMP features
# -1 Lobby mode where one player is choosen to be the host and will make the gamemode selection (first to join is the host)
# 0\tDeathmatch
# 1\tTeam Deathmatch
# 2\tMafiya Work
# 3\tTeam Mafiya Work
# 4\tTeam Car Jack City
# 5\tCar Jack City
# 6\tRace
# 7\tGTA Race
# 8\tParty Mode
# 9\t(Unknown)
# 10\tCops and Crooks
# 11\t(Unknown)
# 12\tTurf War
# 13\tDeal Breaker
# 14\tHangman's NOOSE
# 15\tBomb da Base II
# 16\tFree Mode (NOT IVMP)
gamemode = -2

# Player slots of the session.
slots = 32

# Log only what matters (joins, leaves, warnings, errors). 1 = also the debugging detail: entities,
# game events, summaries.
verbose = 0

# Resources (scripts and UI, see docs/scripting-plan.md): the folder that holds them, and which to
# start, one `start` line each, in order. Console: start / stop / restart / ensure <name>, resources.
resources = resources
# start = example

# Record game packets to this file (empty = off).
capture =
";

impl Config {
    fn defaults() -> Self {
        Config {
            bind: format!("{}:{DEFAULT_PORT}", Ipv4Addr::UNSPECIFIED),
            capture: None,
            ids: Vec::new(),
            host: false,
            gamemode: -2,
            slots: 32,
            verbose: false,
            upnp: false,
            resources: None,
            start: Vec::new(),
        }
    }

    fn path() -> PathBuf {
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join(FILE)))
            .unwrap_or_else(|| FILE.into())
    }

    /// `server.cfg`, then the command line. Returns the config and what to report.
    pub fn load(args: impl Iterator<Item = String>) -> (Self, Vec<String>) {
        let mut config = Config::defaults();
        let mut notes = Vec::new();
        let path = Config::path();
        match std::fs::read_to_string(&path) {
            Ok(text) => notes.extend(config.apply_file(&text)),
            Err(_) => match std::fs::write(&path, DEFAULT_FILE) {
                Ok(()) => notes.push(format!("wrote default settings to {}", path.display())),
                Err(e) => notes.push(format!("could not write {}: {e}", path.display())),
            },
        }
        config.apply_args(args);
        (config, notes)
    }

    /// Applies `key = value` lines; returns warnings.
    fn apply_file(&mut self, text: &str) -> Vec<String> {
        let mut warnings = Vec::new();
        for (number, line) in text.lines().enumerate() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                warnings.push(format!("{FILE} line {}: expected `key = value`", number + 1));
                continue;
            };
            let (key, value) = (key.trim().to_ascii_lowercase(), value.trim());
            let ok = match key.as_str() {
                "bind" => {
                    self.bind = value.to_string();
                    true
                }
                "host" => match value.to_ascii_lowercase().as_str() {
                    "0" | "player" | "p2p" => {
                        self.host = false;
                        true
                    }
                    "1" | "server" => {
                        self.host = true;
                        true
                    }
                    _ => false,
                },
                "gamemode" => value.parse().map(|v| self.gamemode = v).is_ok(),
                "slots" => value.parse::<u8>().ok().filter(|s| (1..=32).contains(s)).map(|s| self.slots = s).is_some(),
                "verbose" => match value.to_ascii_lowercase().as_str() {
                    "0" | "false" | "off" => {
                        self.verbose = false;
                        true
                    }
                    "1" | "true" | "on" => {
                        self.verbose = true;
                        true
                    }
                    _ => false,
                },
                "upnp" => value.parse().map(|v| self.upnp = v).is_ok(),
                "resources" => {
                    self.resources = (!value.is_empty()).then(|| value.to_string());
                    true
                }
                "start" => {
                    if !value.is_empty() {
                        self.start.push(value.to_string());
                    }
                    true
                }
                "capture" => {
                    self.capture = (!value.is_empty()).then(|| value.to_string());
                    true
                }
                _ => {
                    warnings.push(format!("{FILE} line {}: unknown setting `{key}`", number + 1));
                    continue;
                }
            };
            if !ok {
                warnings.push(format!("{FILE} line {}: bad value `{value}` for `{key}`", number + 1));
            }
        }
        warnings
    }

    fn apply_args(&mut self, args: impl Iterator<Item = String>) {
        let mut args = args.peekable();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--capture" => self.capture = args.next(),
                "--host" => self.host = true,
                "--verbose" => self.verbose = true,
                "--ids" => {
                    self.ids = args
                        .next()
                        .unwrap_or_default()
                        .split(',')
                        .filter_map(|id| id.trim().parse().ok())
                        .filter(|id| *id != 0 && *id != 255)
                        .collect()
                }
                _ if !arg.starts_with("--") => self.bind = arg,
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_default_file() {
        let mut config = Config::defaults();
        assert!(config.apply_file(DEFAULT_FILE).is_empty());
        assert!(!config.host);
        assert_eq!((config.gamemode, config.slots, config.capture.clone()), (-2, 32, None));
        let warnings = config.apply_file("host = 1\ngamemode = -2 # freeroam\nslots = 99\nfoo = 1");
        assert!(config.host);
        assert_eq!(config.gamemode, -2);
        assert_eq!(warnings.len(), 2);
    }
}
