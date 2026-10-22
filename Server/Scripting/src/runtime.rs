//! Resources and their Lua states: loading, the script API, events, threads, commands, exports.
//!
//! Everything runs on one thread (the server loop). Each resource has its own Lua state; they meet
//! only through events and exports, where values cross as [`Variant`]s.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use mlua::{Function, Lua, LuaOptions, StdLib, Thread, Value, Variadic};

use crate::manifest::{self, Manifest};
use crate::variant::Variant;

type Log = Box<dyn Fn(String)>;

#[derive(Debug, Clone)]
pub struct Player {
    pub id: u32,
    pub name: String,
}

/// A `TriggerClientEvent` from a script, for the server to send (stage 2).
#[derive(Debug, Clone)]
pub struct ClientEvent {
    /// A player id, or -1 for everyone.
    pub target: i64,
    pub name: String,
    pub args: Vec<Variant>,
}

struct Th {
    thread: Thread,
    wake: Instant,
}

#[derive(Default)]
struct State {
    handlers: HashMap<String, Vec<(u64, Function)>>,
    net_events: HashSet<String>,
    commands: HashMap<String, Function>,
    exports: HashMap<String, Function>,
    threads: Vec<Th>,
    next_id: u64,
}

struct Resource {
    name: String,
    dir: PathBuf,
    manifest: Manifest,
    lua: Lua,
    state: Rc<RefCell<State>>,
}

/// What the resources share: the other resources, the players, the log.
pub struct World {
    dir: PathBuf,
    log: Log,
    started: Instant,
    resources: RefCell<BTreeMap<String, Rc<Resource>>>,
    /// Start order (stopping goes the other way).
    order: RefCell<Vec<String>>,
    players: RefCell<Vec<Player>>,
    drops: RefCell<Vec<(u32, String)>>,
    client_events: RefCell<Vec<ClientEvent>>,
    canceled: Cell<bool>,
    /// Why `playerConnecting` was canceled (SetKickReason).
    kick_reason: RefCell<String>,
    /// Resources being started right now (dependency cycles).
    starting: RefCell<Vec<String>>,
}

/// Where a resource's files are: its own folder, never outside it.
fn safe_join(base: &Path, relative: &str) -> Option<PathBuf> {
    let path = Path::new(relative);
    if relative.is_empty() || path.is_absolute() || relative.contains('\\') && relative.contains(':') {
        return None;
    }
    path.components().all(|c| matches!(c, Component::Normal(_))).then(|| base.join(path))
}

fn lua_error(message: impl Into<String>) -> mlua::Error {
    mlua::Error::runtime(message.into())
}

impl World {
    pub fn new(dir: PathBuf, log: Log) -> Rc<World> {
        Rc::new(World {
            dir,
            log,
            started: Instant::now(),
            resources: RefCell::new(BTreeMap::new()),
            order: RefCell::new(Vec::new()),
            players: RefCell::new(Vec::new()),
            drops: RefCell::new(Vec::new()),
            client_events: RefCell::new(Vec::new()),
            canceled: Cell::new(false),
            kick_reason: RefCell::new(String::new()),
            starting: RefCell::new(Vec::new()),
        })
    }

    fn say(&self, line: String) {
        (self.log)(line);
    }

    pub fn running(&self) -> Vec<String> {
        self.order.borrow().clone()
    }

    pub fn is_running(&self, name: &str) -> bool {
        self.resources.borrow().contains_key(name)
    }

    pub fn manifest_of(&self, name: &str) -> Option<Manifest> {
        self.resources.borrow().get(name).map(|r| r.manifest.clone())
    }

    pub fn resource_dir(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    // ---- events ----------------------------------------------------------------------------

    /// Calls every handler of `event` in every resource (or only in `only`). `source` is the player
    /// that caused it (net events), None for server events. Returns whether a handler canceled it.
    pub fn dispatch(&self, event: &str, args: &[Variant], source: Option<i64>, only: Option<&str>) -> bool {
        self.canceled.set(false);
        let resources: Vec<Rc<Resource>> = {
            let map = self.resources.borrow();
            self.order.borrow().iter().filter_map(|n| map.get(n).cloned()).collect()
        };
        for resource in resources {
            if only.is_some_and(|o| o != resource.name) {
                continue;
            }
            let handlers: Vec<Function> =
                resource.state.borrow().handlers.get(event).map(|h| h.iter().map(|(_, f)| f.clone()).collect()).unwrap_or_default();
            for handler in handlers {
                let converted: Result<Vec<Value>, _> = args.iter().map(|a| a.to_lua(&resource.lua)).collect();
                let Ok(converted) = converted else { continue };
                let _ = resource.lua.globals().set("source", source.map_or(Value::Nil, |s| Value::Integer(s as _)));
                if let Err(e) = handler.call::<()>(Variadic::from(converted)) {
                    self.say(format!("[{}] error in the handler of '{event}': {e}", resource.name));
                }
            }
        }
        self.canceled.get()
    }

    // ---- threads -----------------------------------------------------------------------------

    pub fn tick(&self) {
        let now = Instant::now();
        let resources: Vec<Rc<Resource>> = self.resources.borrow().values().cloned().collect();
        for resource in resources {
            resource.run_threads(self, now);
        }
    }

    // ---- lifecycle -----------------------------------------------------------------------------

    pub fn start(self: &Rc<World>, name: &str) -> Result<(), String> {
        if self.is_running(name) {
            return Err(format!("resource '{name}' is already running"));
        }
        if self.starting.borrow().iter().any(|n| n == name) {
            return Err(format!("dependency cycle through '{name}'"));
        }
        let dir = safe_join(&self.dir, name).ok_or_else(|| format!("'{name}' is not a valid resource name"))?;
        let source = std::fs::read_to_string(dir.join(manifest::FILE))
            .map_err(|e| format!("resource '{name}': cannot read {}: {e}", manifest::FILE))?;
        let manifest = manifest::parse(&source).map_err(|e| format!("resource '{name}': {}: {e}", manifest::FILE))?;
        self.starting.borrow_mut().push(name.to_string());
        let result = self.start_loaded(name, dir, manifest);
        self.starting.borrow_mut().retain(|n| n != name);
        result
    }

    fn start_loaded(self: &Rc<World>, name: &str, dir: PathBuf, manifest: Manifest) -> Result<(), String> {
        for dependency in &manifest.dependencies {
            if !self.is_running(dependency) {
                self.start(dependency).map_err(|e| format!("dependency '{dependency}' of '{name}': {e}"))?;
            }
        }
        let state = Rc::new(RefCell::new(State::default()));
        let lua = Lua::new_with(StdLib::TABLE | StdLib::STRING | StdLib::MATH | StdLib::COROUTINE | StdLib::UTF8, LuaOptions::default())
            .map_err(|e| e.to_string())?;
        install_api(&lua, self, name, &dir, &state).map_err(|e| format!("resource '{name}': {e}"))?;
        let resource = Rc::new(Resource { name: name.to_string(), dir, manifest, lua, state });
        self.resources.borrow_mut().insert(name.to_string(), resource.clone());
        self.order.borrow_mut().push(name.to_string());
        for script in resource.manifest.server_scripts.clone() {
            if let Err(e) = resource.run_script(self, &script) {
                self.say(format!("[{name}] {script}: {e}"));
                // The resource stays loaded with what ran, like FiveM; the error is in the log.
            }
        }
        self.say(format!("started resource {name}"));
        self.dispatch("onResourceStart", &[Variant::Str(name.to_string())], None, None);
        self.dispatch("onServerResourceStart", &[Variant::Str(name.to_string())], None, None);
        Ok(())
    }

    pub fn stop(&self, name: &str) -> Result<(), String> {
        if !self.is_running(name) {
            return Err(format!("resource '{name}' is not running"));
        }
        let dependents: Vec<String> = self
            .resources
            .borrow()
            .values()
            .filter(|r| r.manifest.dependencies.iter().any(|d| d == name))
            .map(|r| r.name.clone())
            .collect();
        if !dependents.is_empty() {
            return Err(format!("stop {} first, they depend on '{name}'", dependents.join(", ")));
        }
        self.dispatch("onResourceStop", &[Variant::Str(name.to_string())], None, None);
        self.dispatch("onServerResourceStop", &[Variant::Str(name.to_string())], None, None);
        self.resources.borrow_mut().remove(name);
        self.order.borrow_mut().retain(|n| n != name);
        self.say(format!("stopped resource {name}"));
        Ok(())
    }

    // ---- commands ------------------------------------------------------------------------------

    /// Runs a registered command line (`source` 0 = the server console). False when no resource
    /// registered the command.
    pub fn run_command(&self, source: i64, line: &str) -> bool {
        let mut words = line.split_whitespace();
        let Some(name) = words.next() else { return false };
        let name = name.to_ascii_lowercase();
        let args: Vec<String> = words.map(str::to_string).collect();
        let resources: Vec<Rc<Resource>> = {
            let map = self.resources.borrow();
            self.order.borrow().iter().filter_map(|n| map.get(n).cloned()).collect()
        };
        for resource in resources {
            let handler = resource.state.borrow().commands.get(&name).cloned();
            let Some(handler) = handler else { continue };
            let table = resource.lua.create_sequence_from(args.clone()).and_then(|t| Ok(Value::Table(t)));
            let raw = resource.lua.create_string(line).map(Value::String);
            match (table, raw) {
                (Ok(table), Ok(raw)) => {
                    if let Err(e) = handler.call::<()>((source, table, raw)) {
                        self.say(format!("[{}] error in the command '{name}': {e}", resource.name));
                    }
                }
                _ => self.say(format!("[{}] could not run the command '{name}'", resource.name)),
            }
            return true;
        }
        false
    }

    // ---- players -------------------------------------------------------------------------------

    pub fn player_joined(&self, id: u32, name: &str) {
        self.players.borrow_mut().retain(|p| p.id != id);
        self.players.borrow_mut().push(Player { id, name: name.to_string() });
        self.dispatch("playerJoining", &[Variant::Str(id.to_string())], Some(id as i64), None);
    }

    /// `playerConnecting(name)` before the player is let in: `Some(reason)` if a handler canceled it
    /// (CancelEvent, with the text from SetKickReason).
    pub fn player_connecting(&self, name: &str) -> Option<String> {
        self.kick_reason.borrow_mut().clear();
        let canceled = self.dispatch("playerConnecting", &[Variant::Str(name.to_string())], Some(0), None);
        canceled.then(|| {
            let reason = self.kick_reason.borrow().clone();
            if reason.is_empty() { "no reason given".to_string() } else { reason }
        })
    }

    pub fn player_dropped(&self, id: u32, reason: &str) {
        self.dispatch("playerDropped", &[Variant::Str(reason.to_string())], Some(id as i64), None);
        self.players.borrow_mut().retain(|p| p.id != id);
    }

    pub fn take_drops(&self) -> Vec<(u32, String)> {
        std::mem::take(&mut *self.drops.borrow_mut())
    }

    pub fn take_client_events(&self) -> Vec<ClientEvent> {
        std::mem::take(&mut *self.client_events.borrow_mut())
    }

    /// A network event from player `source`: only handlers of resources that registered it with
    /// `RegisterNetEvent` run (a script can't be triggered by a client that way otherwise).
    pub fn net_event(&self, source: u32, name: &str, args: &[Variant]) {
        let resources: Vec<Rc<Resource>> = {
            let map = self.resources.borrow();
            self.order.borrow().iter().filter_map(|n| map.get(n).cloned()).collect()
        };
        for resource in resources {
            if resource.state.borrow().net_events.contains(name) {
                self.dispatch(name, args, Some(source as i64), Some(&resource.name));
            }
        }
    }
}

impl Resource {
    fn run_script(self: &Rc<Resource>, world: &World, relative: &str) -> Result<(), String> {
        let path = safe_join(&self.dir, relative).ok_or_else(|| format!("'{relative}' is not a file of this resource"))?;
        let source = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {relative}: {e}"))?;
        let function = self.lua.load(source).set_name(format!("@{}/{relative}", self.name)).into_function().map_err(|e| e.to_string())?;
        let thread = self.lua.create_thread(function).map_err(|e| e.to_string())?;
        self.resume(world, Th { thread, wake: Instant::now() }, Instant::now());
        Ok(())
    }

    /// Resumes a thread once; if it yields, it goes back to the scheduler for later.
    fn resume(&self, world: &World, th: Th, now: Instant) {
        match th.thread.resume::<Value>(()) {
            Ok(value) => {
                if th.thread.is_resumable() {
                    let wait = match value {
                        Value::Integer(i) => i.max(0) as u64,
                        Value::Number(n) => n.max(0.0) as u64,
                        _ => 0,
                    };
                    self.state.borrow_mut().threads.push(Th { thread: th.thread, wake: now + Duration::from_millis(wait) });
                }
            }
            Err(e) => world.say(format!("[{}] {e}", self.name)),
        }
    }

    fn run_threads(&self, world: &World, now: Instant) {
        let due: Vec<Th> = {
            let mut state = self.state.borrow_mut();
            let (due, later): (Vec<Th>, Vec<Th>) = std::mem::take(&mut state.threads).into_iter().partition(|t| t.wake <= now);
            state.threads = later;
            due
        };
        for th in due {
            self.resume(world, th, now);
        }
    }
}

/// The functions a resource's Lua state gets.
fn install_api(lua: &Lua, world: &Rc<World>, name: &str, dir: &Path, state: &Rc<RefCell<State>>) -> mlua::Result<()> {
    let globals = lua.globals();
    // Not available to scripts.
    for unwanted in ["dofile", "loadfile", "print", "require", "package", "os", "io", "debug"] {
        globals.set(unwanted, Value::Nil)?;
    }

    let weak: Weak<World> = Rc::downgrade(world);
    let resource_name = name.to_string();

    // print
    {
        let (weak, name) = (weak.clone(), resource_name.clone());
        globals.set(
            "print",
            lua.create_function(move |lua, args: Variadic<Value>| {
                let tostring: Function = lua.globals().get("tostring")?;
                let mut parts = Vec::new();
                for arg in args.iter() {
                    parts.push(tostring.call::<String>(arg.clone())?);
                }
                if let Some(world) = weak.upgrade() {
                    world.say(format!("[{name}] {}", parts.join("\t")));
                }
                Ok(())
            })?,
        )?;
    }
    globals.set("GetCurrentResourceName", {
        let name = resource_name.clone();
        lua.create_function(move |_, ()| Ok(name.clone()))?
    })?;
    globals.set("GetGameTimer", {
        let weak = weak.clone();
        lua.create_function(move |_, ()| Ok(weak.upgrade().map_or(0, |w| w.started.elapsed().as_millis() as i64)))?
    })?;

    // os: time and clock only
    let os = lua.create_table()?;
    os.set("time", lua.create_function(|_, ()| Ok(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)))?)?;
    {
        let started = Instant::now();
        os.set("clock", lua.create_function(move |_, ()| Ok(started.elapsed().as_secs_f64()))?)?;
    }
    globals.set("os", os)?;

    // events
    {
        let state = state.clone();
        globals.set(
            "AddEventHandler",
            lua.create_function(move |_, (event, handler): (String, Function)| {
                let mut state = state.borrow_mut();
                state.next_id += 1;
                let id = state.next_id;
                state.handlers.entry(event).or_default().push((id, handler));
                Ok(id as i64)
            })?,
        )?;
    }
    {
        let state = state.clone();
        globals.set(
            "RemoveEventHandler",
            lua.create_function(move |_, id: i64| {
                for handlers in state.borrow_mut().handlers.values_mut() {
                    handlers.retain(|(h, _)| *h as i64 != id);
                }
                Ok(())
            })?,
        )?;
    }
    {
        let state = state.clone();
        globals.set(
            "RegisterNetEvent",
            lua.create_function(move |_, (event, handler): (String, Option<Function>)| {
                let mut state = state.borrow_mut();
                state.net_events.insert(event.clone());
                if let Some(handler) = handler {
                    state.next_id += 1;
                    let id = state.next_id;
                    state.handlers.entry(event).or_default().push((id, handler));
                }
                Ok(())
            })?,
        )?;
    }
    {
        let weak = weak.clone();
        globals.set(
            "TriggerEvent",
            lua.create_function(move |_, args: Variadic<Value>| {
                let mut args = args.into_iter();
                let Some(Value::String(event)) = args.next() else { return Err(lua_error("TriggerEvent: the first argument is the event name")) };
                let variants: Result<Vec<Variant>, String> = args.map(|v| Variant::from_lua(&v)).collect();
                let variants = variants.map_err(|e| lua_error(format!("TriggerEvent: {e}")))?;
                if let Some(world) = weak.upgrade() {
                    world.dispatch(&event.to_string_lossy(), &variants, None, None);
                }
                Ok(())
            })?,
        )?;
    }
    {
        let cancel_weak = weak.clone();
        globals.set("CancelEvent", lua.create_function(move |_, ()| {
            if let Some(world) = cancel_weak.upgrade() {
                world.canceled.set(true);
            }
            Ok(())
        })?)?;
        let kick_weak = weak.clone();
        globals.set("SetKickReason", lua.create_function(move |_, reason: String| {
            if let Some(world) = kick_weak.upgrade() {
                *world.kick_reason.borrow_mut() = reason;
            }
            Ok(())
        })?)?;
        let was_weak = weak.clone();
        globals.set("WasEventCanceled", lua.create_function(move |_, ()| Ok(was_weak.upgrade().is_some_and(|w| w.canceled.get())))?)?;
    }

    // threads: Wait yields to the scheduler (see the prelude)
    {
        let state = state.clone();
        globals.set(
            "CreateThread",
            lua.create_function(move |lua, function: Function| {
                let thread = lua.create_thread(function)?;
                state.borrow_mut().threads.push(Th { thread, wake: Instant::now() });
                Ok(())
            })?,
        )?;
    }

    // commands
    {
        let state = state.clone();
        globals.set(
            "RegisterCommand",
            lua.create_function(move |_, (command, handler, _restricted): (String, Function, Option<bool>)| {
                state.borrow_mut().commands.insert(command.to_ascii_lowercase(), handler);
                Ok(())
            })?,
        )?;
    }

    // players
    {
        let weak2 = weak.clone();
        globals.set(
            "GetPlayers",
            lua.create_function(move |lua, ()| {
                let ids: Vec<String> = weak2.upgrade().map(|w| w.players.borrow().iter().map(|p| p.id.to_string()).collect()).unwrap_or_default();
                lua.create_sequence_from(ids)
            })?,
        )?;
        let weak2 = weak.clone();
        globals.set(
            "GetNumPlayerIndices",
            lua.create_function(move |_, ()| Ok(weak2.upgrade().map_or(0, |w| w.players.borrow().len() as i64)))?,
        )?;
        let weak2 = weak.clone();
        globals.set(
            "GetPlayerName",
            lua.create_function(move |_, id: Value| {
                let id = match id {
                    Value::Integer(i) => i as u32,
                    Value::Number(n) => n as u32,
                    Value::String(s) => s.to_string_lossy().parse().unwrap_or(u32::MAX),
                    _ => u32::MAX,
                };
                Ok(weak2.upgrade().and_then(|w| w.players.borrow().iter().find(|p| p.id == id).map(|p| p.name.clone())))
            })?,
        )?;
        let weak2 = weak.clone();
        globals.set(
            "DropPlayer",
            lua.create_function(move |_, (id, reason): (Value, Option<String>)| {
                let id = match id {
                    Value::Integer(i) => i as u32,
                    Value::Number(n) => n as u32,
                    Value::String(s) => s.to_string_lossy().parse().unwrap_or(u32::MAX),
                    _ => return Ok(()),
                };
                if let Some(world) = weak2.upgrade() {
                    world.drops.borrow_mut().push((id, reason.unwrap_or_else(|| "Dropped by a script".into())));
                }
                Ok(())
            })?,
        )?;
        let weak2 = weak.clone();
        globals.set(
            "TriggerClientEvent",
            lua.create_function(move |_, args: Variadic<Value>| {
                let mut args = args.into_iter();
                let (Some(Value::String(event)), Some(target)) = (args.next(), args.next()) else {
                    return Err(lua_error("TriggerClientEvent(name, player, ...)"));
                };
                let target = match target {
                    Value::Integer(i) => i as i64,
                    Value::Number(n) => n as i64,
                    Value::String(s) => s.to_string_lossy().parse().unwrap_or(-1),
                    _ => -1,
                };
                let variants: Result<Vec<Variant>, String> = args.map(|v| Variant::from_lua(&v)).collect();
                let variants = variants.map_err(|e| lua_error(format!("TriggerClientEvent: {e}")))?;
                if let Some(world) = weak2.upgrade() {
                    world.client_events.borrow_mut().push(ClientEvent { target, name: event.to_string_lossy().to_string(), args: variants });
                }
                Ok(())
            })?,
        )?;
    }

    // files
    {
        let weak2 = weak.clone();
        globals.set(
            "LoadResourceFile",
            lua.create_function(move |_, (resource, file): (String, String)| {
                let Some(world) = weak2.upgrade() else { return Ok(None) };
                let Some(base) = safe_join(&world.dir, &resource) else { return Ok(None) };
                let Some(path) = safe_join(&base, &file) else { return Ok(None) };
                Ok(std::fs::read_to_string(path).ok())
            })?,
        )?;
        let weak2 = weak.clone();
        globals.set(
            "GetResourceState",
            lua.create_function(move |_, resource: String| {
                Ok(if weak2.upgrade().is_some_and(|w| w.is_running(&resource)) { "started" } else { "stopped" })
            })?,
        )?;
    }

    // require: files of this resource only
    {
        let dir = dir.to_path_buf();
        let cache: Rc<RefCell<HashMap<String, Value>>> = Rc::new(RefCell::new(HashMap::new()));
        globals.set(
            "require",
            lua.create_function(move |lua, module: String| {
                if let Some(value) = cache.borrow().get(&module) {
                    return Ok(value.clone());
                }
                let relative = format!("{}.lua", module.replace('.', "/"));
                let path = safe_join(&dir, &relative).ok_or_else(|| lua_error(format!("require '{module}': not a file of this resource")))?;
                let source = std::fs::read_to_string(&path).map_err(|e| lua_error(format!("require '{module}': {e}")))?;
                let value: Value = lua.load(source).set_name(format!("@{relative}")).eval()?;
                let value = if matches!(value, Value::Nil) { Value::Boolean(true) } else { value };
                cache.borrow_mut().insert(module, value.clone());
                Ok(value)
            })?,
        )?;
    }

    // json
    let json = lua.create_table()?;
    json.set(
        "encode",
        lua.create_function(|_, (value, _options): (Value, Option<Value>)| {
            let variant = Variant::from_lua(&value).map_err(lua_error)?;
            serde_json::to_string(&variant.to_json()).map_err(|e| lua_error(e.to_string()))
        })?,
    )?;
    json.set(
        "decode",
        lua.create_function(|lua, text: String| {
            let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| lua_error(format!("json.decode: {e}")))?;
            Variant::from_json(&value).to_lua(lua)
        })?,
    )?;
    globals.set("json", json)?;

    // exports
    {
        let state = state.clone();
        globals.set(
            "__export",
            lua.create_function(move |_, (export, function): (String, Function)| {
                state.borrow_mut().exports.insert(export, function);
                Ok(())
            })?,
        )?;
        let weak2 = weak.clone();
        globals.set(
            "__call_export",
            lua.create_function(move |lua, args: Variadic<Value>| {
                let mut args = args.into_iter();
                let (Some(Value::String(resource)), Some(Value::String(export))) = (args.next(), args.next()) else {
                    return Err(lua_error("exports: bad call"));
                };
                let (resource, export) = (resource.to_string_lossy().to_string(), export.to_string_lossy().to_string());
                let world = weak2.upgrade().ok_or_else(|| lua_error("the server is shutting down"))?;
                let target = world.resources.borrow().get(&resource).cloned().ok_or_else(|| lua_error(format!("exports: resource '{resource}' is not running")))?;
                let function = target
                    .state
                    .borrow()
                    .exports
                    .get(&export)
                    .cloned()
                    .ok_or_else(|| lua_error(format!("exports: '{resource}' has no export '{export}'")))?;
                let variants: Result<Vec<Variant>, String> = args.map(|v| Variant::from_lua(&v)).collect();
                let variants = variants.map_err(|e| lua_error(format!("exports: {e}")))?;
                let converted: Result<Vec<Value>, _> = variants.iter().map(|v| v.to_lua(&target.lua)).collect();
                let results: Variadic<Value> = function.call(Variadic::from(converted?))?;
                // Back into the caller's Lua state.
                let mut out = Vec::new();
                for result in results.iter() {
                    out.push(Variant::from_lua(result).map_err(lua_error)?.to_lua(lua)?);
                }
                Ok(Variadic::from(out))
            })?,
        )?;
    }

    lua.load(PRELUDE).set_name("=prelude").exec()?;
    Ok(())
}

/// Lua part of the API: Wait, SetTimeout, exports. Runs once per state, before any script.
const PRELUDE: &str = r#"
local export, call_export = __export, __call_export
__export, __call_export = nil, nil

function Wait(ms) coroutine.yield(ms or 0) end

function SetTimeout(ms, fn)
    CreateThread(function() Wait(ms) fn() end)
end

local proxies = {}
exports = setmetatable({}, {
    __call = function(_, name, fn) export(name, fn) end,
    __index = function(_, resource)
        local proxy = proxies[resource]
        if not proxy then
            proxy = setmetatable({}, { __index = function(self, name)
                return function(...)
                    local first = ...
                    if first == self then
                        return call_export(resource, name, select(2, ...))
                    end
                    return call_export(resource, name, ...)
                end
            end })
            proxies[resource] = proxy
        end
        return proxy
    end,
})
"#;
