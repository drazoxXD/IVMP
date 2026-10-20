//! IVMP server (`Server.exe`).
//!
//! Usage: `Server [bind address] [--capture <file>]` (default `0.0.0.0:9999`). Output also goes
//! to `ivmp_server.log` next to the executable.
//!
//! * [`config`]: command line.
//! * [`log`]: console + log file output (`say!`).
//! * [`lobby`]: who is connected, client ids, which client hosts the engine session.
//! * [`relay`]: forwards the engine's game packets between clients.
//! * [`capture`]: records game packets for analysis (`--capture`).
//! * [`server`]: the main loop tying them together, plus the entity registry (`ivmp_sync`).
//!
//! Crates: `NetProtocol` (wire protocol shared with the client), `Sync` (decodes the game's
//! sync traffic), and `Auth`, `Natives`, `Scripting` for what comes next.

#[macro_use]
mod log;
mod capture;
mod config;
mod lobby;
mod relay;
mod server;

fn main() -> std::io::Result<()> {
    log::open();
    let config = config::Config::from_args(std::env::args().skip(1));
    server::Server::start(&config)?.run()
}
