//! Command line: `Server [bind address] [--capture <file>] [--ids <id,id,...>] [--host]`.
//!
//! `--host`: the server is the game's session host (client id 254); every game joins it and no
//! player hosts (Host crate, docs/onesync-plan.md). Without it the first player hosts.
//!
//! `--ids` hands out these client ids, in join order, before the usual lowest free id. Used to
//! record a session whose host has the id the server will use as host (254), for the server
//! host work (docs/onesync-plan.md).

use std::net::Ipv4Addr;

use NetProtocol::DEFAULT_PORT;

pub struct Config {
    /// Address the UDP socket binds to.
    pub bind: String,
    /// Where to record game packets (`--capture`).
    pub capture: Option<String>,
    /// Client ids to hand out first (`--ids`).
    pub ids: Vec<u8>,
    /// The server hosts the game session (`--host`).
    pub host: bool,
}

impl Config {
    pub fn from_args(args: impl Iterator<Item = String>) -> Self {
        let mut config = Config { bind: format!("{}:{DEFAULT_PORT}", Ipv4Addr::UNSPECIFIED), capture: None, ids: Vec::new(), host: false };
        let mut args = args.peekable();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--capture" => config.capture = args.next(),
                "--host" => config.host = true,
                "--ids" => {
                    config.ids = args
                        .next()
                        .unwrap_or_default()
                        .split(',')
                        .filter_map(|id| id.trim().parse().ok())
                        .filter(|id| *id != 0 && *id != 255)
                        .collect()
                }
                _ if !arg.starts_with("--") => config.bind = arg,
                _ => {}
            }
        }
        config
    }
}
