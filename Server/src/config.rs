//! Command line: `Server [bind address] [--capture <file>]`.

use std::net::Ipv4Addr;

use NetProtocol::DEFAULT_PORT;

pub struct Config {
    /// Address the UDP socket binds to.
    pub bind: String,
    /// Where to record game packets (`--capture`).
    pub capture: Option<String>,
}

impl Config {
    pub fn from_args(args: impl Iterator<Item = String>) -> Self {
        let mut config = Config { bind: format!("{}:{DEFAULT_PORT}", Ipv4Addr::UNSPECIFIED), capture: None };
        let mut args = args.peekable();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--capture" => config.capture = args.next(),
                _ if !arg.starts_with("--") => config.bind = arg,
                _ => {}
            }
        }
        config
    }
}
