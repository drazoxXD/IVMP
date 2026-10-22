//! IVMP server (`Server.exe`).
//!
//! Settings come from `server.cfg` next to the executable (written with defaults the first time),
//! with the command line on top: `Server [bind address] [--capture <file>] [--host]` (see
//! [`config`]). Output also goes to `ivmp_server.log` next to the executable.
//!
//! * [`config`]: command line.
//! * [`log`]: console + log file output (`say!`).
//! * [`lobby`]: who is connected, client ids, which client hosts the engine session.
//! * [`relay`]: forwards the engine's game packets between clients.
//! * [`capture`]: records game packets for analysis (`--capture`).
//! * [`world`]: server-owned entities (a client creates them on request, the server keeps them).
//! * [`console`]: commands typed into the server window (`spawn`, `entities`, ...).
//! * [`server`]: the main loop tying them together, plus the entity registry (`ivmp_sync`).
//!
//! Crates: `NetProtocol` (wire protocol shared with the client), `Sync` (decodes the game's
//! sync traffic), and `Auth`, `Natives`, `Scripting` for what comes next.

#[macro_use]
mod log;
mod capture;
mod config;
mod console;
mod lobby;
mod relay;
mod server;
mod world;

fn main() -> std::io::Result<()> {
    log::open();
    let (config, notes) = config::Config::load(std::env::args().skip(1));
    log::VERBOSE.store(config.verbose, std::sync::atomic::Ordering::Relaxed);
    for note in notes {
        say!("{note}");
    }
    server::Server::start(&config)?.run()
}
