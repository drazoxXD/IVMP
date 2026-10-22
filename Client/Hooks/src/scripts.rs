//! The client's Lua runtime (docs/scripting-plan.md, stage 3): the resources the server sent run
//! here, on the game thread. Once per frame it starts and stops resources as the downloads finish,
//! hands the server's events to the scripts, runs their threads and timers, and passes what they
//! trigger for the server on.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use ivmp_networking::net;
use ivmp_networking::resources::{self, Change};
use ivmp_scripting::Scripting;

use crate::log;

/// Console lines for the scripts (the console runs on the render thread, the runtime on the game
/// thread), and how many resources run, so the console knows whether to pass a line on.
static PENDING: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
static RUNNING: AtomicUsize = AtomicUsize::new(0);
/// Lua lines typed into the console (`lua ...`).
static PENDING_LUA: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

thread_local! {
    static RUNTIME: RefCell<Option<Scripting>> = const { RefCell::new(None) };
}

/// Where the downloaded resources are kept (`cache` next to the DLL).
pub fn init() {
    resources::set_cache_dir(crate::dll_dir().join("cache"));
    crate::luanatives::load_file(&crate::dll_dir().join("natives.json"));
}

fn with_runtime<R>(f: impl FnOnce(&Scripting) -> R) -> R {
    RUNTIME.with(|cell| {
        let mut slot = cell.borrow_mut();
        let runtime = slot.get_or_insert_with(|| {
            let runtime = Scripting::client(crate::dll_dir().join("cache"), |line| log!("script: {line}"));
            runtime.set_extra_api(crate::luanatives::install);
            runtime
        });
        f(runtime)
    })
}

/// Call once per frame on the game thread.
pub fn tick() {
    let changes = resources::take_changes();
    let events = net::take_script_events();
    // Nothing downloaded and nothing running: nothing to do, and no Lua state is made.
    let commands: Vec<String> = PENDING.lock().unwrap().drain(..).collect();
    let lua_lines: Vec<String> = PENDING_LUA.lock().unwrap().drain(..).collect();
    let idle = changes.is_empty() && events.is_empty() && commands.is_empty() && lua_lines.is_empty();
    let started = RUNTIME.with(|cell| cell.borrow().is_some());
    if idle && !started {
        return;
    }
    with_runtime(|runtime| {
        // Stops can be refused while a resource that depends on the one stopping still runs;
        // go through them again until nothing more is stopped.
        let mut stops: Vec<String> = Vec::new();
        for change in changes {
            match change {
                Change::Start(name) => {
                    if let Err(e) = runtime.start(&name) {
                        log!("script: cannot start {name}: {e}");
                    }
                }
                Change::Stop(name) => stops.push(name),
            }
        }
        for _ in 0..8 {
            if stops.is_empty() {
                break;
            }
            stops.retain(|name| runtime.stop(name).is_err() && runtime.running().contains(name));
        }
        for (name, json) in events {
            if let Err(e) = runtime.client_event_json(&name, json.as_bytes()) {
                log!("script: event '{name}' from the server: {e}");
            }
        }
        for line in commands {
            if !runtime.run_command(0, &line) {
                log!("script: no resource registered the command '{}'", line.split_whitespace().next().unwrap_or(""));
            }
        }
        for line in lua_lines {
            if let Err(e) = runtime.eval(&line) {
                log!("script: lua: {e}");
            }
        }
        runtime.tick();
        RUNNING.store(runtime.running().len(), Ordering::Relaxed);
        for (name, json) in runtime.take_server_events() {
            if let Err(e) = net::trigger_server_event(&name, &json) {
                log!("script: TriggerServerEvent '{name}': {e}");
            }
        }
    });
}

/// A console line that no built-in command took: handed to the scripts (on the game thread, next
/// frame). False if no resource runs on this client, so the line is just unknown.
pub fn queue_command(line: &str) -> bool {
    if RUNNING.load(Ordering::Relaxed) == 0 {
        return false;
    }
    PENDING.lock().unwrap().push_back(line.to_string());
    true
}

/// A line of Lua from the console, run on the game thread next frame (the output goes to the log).
pub fn queue_lua(code: &str) {
    PENDING_LUA.lock().unwrap().push_back(code.to_string());
}

/// How many resources run on this client.
pub fn running() -> usize {
    RUNNING.load(Ordering::Relaxed)
}
