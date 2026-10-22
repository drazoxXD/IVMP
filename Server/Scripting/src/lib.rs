//! FiveM style resources for the IVMP server (docs/scripting-plan.md): a manifest, one Lua state
//! per resource, events, threads and timers, commands, exports, player events.

pub mod manifest;
mod runtime;
pub mod variant;

pub use runtime::{ClientEvent, Player};
pub use variant::Variant;

use std::path::PathBuf;
use std::rc::Rc;

use runtime::World;

/// The scripting runtime. Not `Send`: it lives on the server thread.
pub struct Scripting {
    world: Rc<World>,
}

impl Scripting {
    /// `dir` holds the resource folders; `log` receives every line scripts print or fail with.
    pub fn new(dir: PathBuf, log: impl Fn(String) + 'static) -> Scripting {
        Scripting { world: World::new(dir, Box::new(log)) }
    }

    pub fn start(&self, name: &str) -> Result<(), String> {
        self.world.start(name)
    }

    pub fn stop(&self, name: &str) -> Result<(), String> {
        self.world.stop(name)
    }

    /// Stops (when running) and starts again.
    pub fn restart(&self, name: &str) -> Result<(), String> {
        if self.world.is_running(name) {
            self.world.stop(name)?;
        }
        self.world.start(name)
    }

    /// Starts the resource, or restarts it when it runs.
    pub fn ensure(&self, name: &str) -> Result<(), String> {
        self.restart(name)
    }

    /// The running resources, in start order.
    pub fn running(&self) -> Vec<String> {
        self.world.running()
    }

    /// Every folder under the resources directory that has a manifest.
    pub fn available(&self) -> Vec<String> {
        let mut out: Vec<String> = std::fs::read_dir(self.world.resource_dir(""))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().join(manifest::FILE).is_file())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        out.sort();
        out
    }

    pub fn manifest_of(&self, name: &str) -> Option<manifest::Manifest> {
        self.world.manifest_of(name)
    }

    /// Runs the threads and timers that are due. Call every server loop.
    pub fn tick(&self) {
        self.world.tick();
    }

    pub fn player_joined(&self, id: u32, name: &str) {
        self.world.player_joined(id, name);
    }

    pub fn player_dropped(&self, id: u32, reason: &str) {
        self.world.player_dropped(id, reason);
    }

    /// Runs a command a resource registered with `RegisterCommand`; `source` 0 is the console.
    pub fn run_command(&self, source: i64, line: &str) -> bool {
        self.world.run_command(source, line)
    }

    /// Players scripts asked to drop (`DropPlayer`): (id, reason).
    pub fn take_drops(&self) -> Vec<(u32, String)> {
        self.world.take_drops()
    }

    /// `TriggerClientEvent` calls waiting to be sent.
    pub fn take_client_events(&self) -> Vec<ClientEvent> {
        self.world.take_client_events()
    }

    /// A network event from a player: runs the handlers registered with `RegisterNetEvent`.
    pub fn net_event(&self, source: u32, name: &str, args: &[Variant]) {
        self.world.net_event(source, name, args);
    }
}

#[cfg(test)]
mod tests;
