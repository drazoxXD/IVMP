//! FiveM style resources for the IVMP server (docs/scripting-plan.md): a manifest, one Lua state
//! per resource, events, threads and timers, commands, exports, player events.

pub mod manifest;
mod runtime;
pub mod variant;

pub use mlua;
pub use runtime::{ClientEvent, Player, Side};
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
        Scripting { world: World::new(Side::Server, dir, Box::new(log)) }
    }

    /// The client's runtime: `dir` holds the downloaded resources, it runs their `client_scripts`.
    pub fn client(dir: PathBuf, log: impl Fn(String) + 'static) -> Scripting {
        Scripting { world: World::new(Side::Client, dir, Box::new(log)) }
    }

    /// Functions every Lua state made from now on gets, besides the standard API.
    pub fn set_extra_api(&self, api: impl Fn(&mlua::Lua) -> mlua::Result<()> + 'static) {
        self.world.set_extra_api(Rc::new(api));
    }

    /// Runs a line of Lua in the `console` resource and logs what it returns (F1 `lua` command).
    pub fn eval(&self, code: &str) -> Result<(), String> {
        self.world.eval(code)
    }

    /// What client scripts asked the server for (`TriggerServerEvent`), as (name, JSON arguments).
    pub fn take_server_events(&self) -> Vec<(String, String)> {
        self.world.take_server_events().into_iter().map(|(name, args)| (name, args_to_json(&args))).collect()
    }

    /// An event the server triggered on this client: the arguments are a JSON array.
    pub fn client_event_json(&self, name: &str, json: &[u8]) -> Result<(), String> {
        let args = args_from_json(json)?;
        self.world.dispatch(name, &args, None, None);
        Ok(())
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

    /// The folder of a resource.
    pub fn resource_dir(&self, name: &str) -> PathBuf {
        self.world.resource_dir(name)
    }

    pub fn manifest_of(&self, name: &str) -> Option<manifest::Manifest> {
        self.world.manifest_of(name)
    }

    /// Runs the threads and timers that are due. Call every server loop.
    pub fn tick(&self) {
        self.world.tick();
    }

    /// Asks the resources whether `name` may join: `Some(reason)` if one canceled `playerConnecting`.
    pub fn player_connecting(&self, name: &str) -> Option<String> {
        self.world.player_connecting(name)
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

    /// An event a client triggered: its arguments are a JSON array.
    pub fn net_event_json(&self, source: u32, name: &str, json: &[u8]) -> Result<(), String> {
        let args = args_from_json(json)?;
        self.world.net_event(source, name, &args);
        Ok(())
    }
}

#[cfg(test)]
mod tests;

/// The arguments of an event as the JSON array that goes over the network.
pub fn args_to_json(args: &[Variant]) -> String {
    serde_json::Value::Array(args.iter().map(Variant::to_json).collect()).to_string()
}

/// The inverse of [`args_to_json`]; a client could send anything, so it must be an array.
pub fn args_from_json(json: &[u8]) -> Result<Vec<Variant>, String> {
    match serde_json::from_slice::<serde_json::Value>(json).map_err(|e| e.to_string())? {
        serde_json::Value::Array(items) => Ok(items.iter().map(Variant::from_json).collect()),
        _ => Err("the arguments must be a JSON array".into()),
    }
}
