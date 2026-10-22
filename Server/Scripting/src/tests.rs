//! The runtime, end to end, on resources written to a temporary folder.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use super::*;

/// A resources folder in the temp directory, removed on drop.
struct Folder(PathBuf);

impl Folder {
    fn new(name: &str) -> Folder {
        let path = std::env::temp_dir().join(format!("ivmp_scripting_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Folder(path)
    }

    fn resource(&self, name: &str, manifest: &str, files: &[(&str, &str)]) {
        let dir = self.0.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ivmp.lua"), manifest).unwrap();
        for (file, content) in files {
            let path = dir.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scripting(folder: &Folder) -> (Scripting, Rc<RefCell<Vec<String>>>) {
    let log = Rc::new(RefCell::new(Vec::new()));
    let sink = log.clone();
    (Scripting::new(folder.0.clone(), move |line| sink.borrow_mut().push(line)), log)
}

fn lines(log: &Rc<RefCell<Vec<String>>>) -> Vec<String> {
    log.borrow().clone()
}

#[test]
fn a_resource_starts_and_prints() {
    let folder = Folder::new("start");
    folder.resource("hello", "server_script 'main.lua'", &[("main.lua", "print('hello', 1, true)")]);
    let (scripting, log) = scripting(&folder);
    scripting.start("hello").unwrap();
    let out = lines(&log);
    assert!(out.contains(&"[hello] hello\t1\ttrue".to_string()), "{out:?}");
    assert!(out.iter().any(|l| l == "started resource hello"));
    assert_eq!(scripting.running(), vec!["hello"]);
    assert!(scripting.start("hello").is_err(), "already running");
    scripting.stop("hello").unwrap();
    assert!(scripting.running().is_empty());
}

#[test]
fn events_pass_values_between_resources() {
    let folder = Folder::new("events");
    folder.resource(
        "listener",
        "server_script 'a.lua'",
        &[("a.lua", "AddEventHandler('greet', function(who, data) print('got', who, data.n, data.list[2]) end)")],
    );
    folder.resource(
        "sender",
        "server_script 'b.lua'",
        &[("b.lua", "TriggerEvent('greet', 'world', { n = 42, list = { 'a', 'b' } })")],
    );
    let (scripting, log) = scripting(&folder);
    scripting.start("listener").unwrap();
    scripting.start("sender").unwrap();
    assert!(lines(&log).contains(&"[listener] got\tworld\t42\tb".to_string()), "{:?}", lines(&log));
}

#[test]
fn threads_wait_and_timers_fire() {
    let folder = Folder::new("threads");
    folder.resource(
        "t",
        "server_script 'main.lua'",
        &[("main.lua", "CreateThread(function() print('one') Wait(30) print('two') end) SetTimeout(10, function() print('timer') end) print('top')")],
    );
    let (scripting, log) = scripting(&folder);
    scripting.start("t").unwrap();
    // The main chunk ran; the threads it created start on the next tick.
    assert!(lines(&log).contains(&"[t] top".to_string()));
    assert!(!lines(&log).contains(&"[t] one".to_string()));
    scripting.tick();
    let first = lines(&log);
    assert!(first.contains(&"[t] one".to_string()), "{first:?}");
    assert!(!first.contains(&"[t] two".to_string()));
    std::thread::sleep(std::time::Duration::from_millis(60));
    scripting.tick();
    let after = lines(&log);
    assert!(after.contains(&"[t] two".to_string()) && after.contains(&"[t] timer".to_string()), "{after:?}");
}

#[test]
fn commands_run_with_source_and_arguments() {
    let folder = Folder::new("commands");
    folder.resource(
        "c",
        "server_script 'main.lua'",
        &[("main.lua", "RegisterCommand('say', function(source, args, raw) print(source, #args, args[1], raw) end)")],
    );
    let (scripting, log) = scripting(&folder);
    scripting.start("c").unwrap();
    assert!(scripting.run_command(0, "say hello world"));
    assert!(!scripting.run_command(0, "unknown thing"));
    assert!(lines(&log).contains(&"[c] 0\t2\thello\tsay hello world".to_string()), "{:?}", lines(&log));
}

#[test]
fn exports_cross_resources() {
    let folder = Folder::new("exports");
    folder.resource("math", "server_script 'm.lua'", &[("m.lua", "exports('add', function(a, b) return a + b, 'done' end)")]);
    folder.resource(
        "user",
        "server_script 'u.lua'\ndependency 'math'",
        &[("u.lua", "local sum, note = exports.math:add(2, 3) print(sum, note) print(exports.math.add(4, 5))")],
    );
    let (scripting, log) = scripting(&folder);
    scripting.start("user").unwrap();
    let out = lines(&log);
    assert!(out.contains(&"[user] 5\tdone".to_string()), "{out:?}");
    assert!(out.contains(&"[user] 9\tdone".to_string()), "{out:?}");
    assert_eq!(scripting.running(), vec!["math", "user"], "the dependency started first");
    assert!(scripting.stop("math").is_err(), "'user' depends on it");
}

#[test]
fn player_events_and_net_events() {
    let folder = Folder::new("players");
    folder.resource(
        "p",
        "server_script 'main.lua'",
        &[(
            "main.lua",
            "AddEventHandler('playerJoining', function() print('joining', source, GetPlayerName(source)) end)\n\
             AddEventHandler('playerDropped', function(reason) print('dropped', source, reason, GetNumPlayerIndices()) end)\n\
             RegisterNetEvent('hello', function(text) print('net', source, text) end)\n\
             AddEventHandler('private', function() print('private ran') end)",
        )],
    );
    let (scripting, log) = scripting(&folder);
    scripting.start("p").unwrap();
    scripting.player_joined(3, "Niko");
    scripting.net_event(3, "hello", &[Variant::Str("hi".into())]);
    scripting.net_event(3, "private", &[]);
    scripting.player_dropped(3, "left");
    let out = lines(&log);
    assert!(out.contains(&"[p] joining\t3\tNiko".to_string()), "{out:?}");
    assert!(out.contains(&"[p] net\t3\thi".to_string()), "{out:?}");
    assert!(!out.iter().any(|l| l.contains("private ran")), "a client can't trigger an event that is not a net event");
    assert!(out.contains(&"[p] dropped\t3\tleft\t1".to_string()), "{out:?}");
}

#[test]
fn drops_and_client_events_are_queued() {
    let folder = Folder::new("queue");
    folder.resource(
        "q",
        "server_script 'main.lua'",
        &[("main.lua", "DropPlayer(5, 'bye') TriggerClientEvent('news', -1, 'a', { x = 1 }) TriggerClientEvent('only', '7')")],
    );
    let (scripting, _) = scripting(&folder);
    scripting.start("q").unwrap();
    assert_eq!(scripting.take_drops(), vec![(5, "bye".to_string())]);
    let events = scripting.take_client_events();
    assert_eq!(events.len(), 2);
    assert_eq!((events[0].target, events[0].name.as_str()), (-1, "news"));
    assert_eq!(events[1].target, 7);
}

#[test]
fn the_sandbox_holds() {
    let folder = Folder::new("sandbox");
    folder.resource(
        "s",
        "server_script 'main.lua'",
        &[(
            "main.lua",
            "print(io, os.execute, os.remove, debug, package, dofile, loadfile)\n\
             print(LoadResourceFile('s', '../x'), LoadResourceFile('s', 'main.lua') ~= nil)\n\
             print(pcall(require, '../escape'))",
        )],
    );
    let (scripting, log) = scripting(&folder);
    scripting.start("s").unwrap();
    let out = lines(&log);
    assert!(out.contains(&"[s] nil\tnil\tnil\tnil\tnil\tnil\tnil".to_string()), "{out:?}");
    assert!(out.iter().any(|l| l.starts_with("[s] nil\ttrue")), "{out:?}");
    assert!(out.iter().any(|l| l.starts_with("[s] false") && l.contains("not a file of this resource")), "{out:?}");
}

#[test]
fn script_errors_are_logged_not_fatal() {
    let folder = Folder::new("errors");
    folder.resource("bad", "server_script 'main.lua'", &[("main.lua", "error('boom')")]);
    folder.resource("worse", "server_script 'missing.lua'", &[]);
    let (scripting, log) = scripting(&folder);
    scripting.start("bad").unwrap();
    scripting.start("worse").unwrap();
    let out = lines(&log);
    assert!(out.iter().any(|l| l.contains("[bad]") && l.contains("boom")), "{out:?}");
    assert!(out.iter().any(|l| l.contains("[worse]") && l.contains("missing.lua")), "{out:?}");
    assert!(scripting.start("nope").is_err());
}

#[test]
fn json_round_trip() {
    let folder = Folder::new("json");
    folder.resource(
        "j",
        "server_script 'main.lua'",
        &[("main.lua", "local t = json.decode('{\"a\":[1,2,3],\"b\":\"x\"}') print(#t.a, t.b, json.encode({1,2,3}))")],
    );
    let (scripting, log) = scripting(&folder);
    scripting.start("j").unwrap();
    assert!(lines(&log).contains(&"[j] 3\tx\t[1,2,3]".to_string()), "{:?}", lines(&log));
}

#[test]
fn restart_runs_the_start_and_stop_events() {
    let folder = Folder::new("restart");
    folder.resource(
        "r",
        "server_script 'main.lua'",
        &[("main.lua", "AddEventHandler('onResourceStart', function(name) print('start', name) end) AddEventHandler('onResourceStop', function(name) print('stop', name) end)")],
    );
    let (scripting, log) = scripting(&folder);
    scripting.start("r").unwrap();
    scripting.restart("r").unwrap();
    let out = lines(&log);
    assert_eq!(out.iter().filter(|l| *l == "[r] start\tr").count(), 2, "{out:?}");
    assert_eq!(out.iter().filter(|l| *l == "[r] stop\tr").count(), 1, "{out:?}");
}

#[test]
fn net_events_come_from_json_and_go_out_as_json() {
    let folder = Folder::new("json_events");
    folder.resource(
        "e",
        "server_script 'main.lua'",
        &[(
            "main.lua",
            "RegisterNetEvent('ping', function(a, t) print('ping', source, a, t.x) TriggerClientEvent('pong', source, a + 1, { x = t.x }) end)",
        )],
    );
    let (scripting, log) = scripting(&folder);
    scripting.start("e").unwrap();
    scripting.net_event_json(4, "ping", br#"[41, {"x": "y"}]"#).unwrap();
    assert!(lines(&log).contains(&"[e] ping\t4\t41\ty".to_string()), "{:?}", lines(&log));
    let events = scripting.take_client_events();
    assert_eq!((events[0].target, events[0].name.as_str()), (4, "pong"));
    assert_eq!(args_to_json(&events[0].args), r#"[42,{"x":"y"}]"#);
    assert!(scripting.net_event_json(4, "ping", b"{}").is_err(), "arguments must be an array");
    assert!(scripting.net_event_json(4, "ping", b"not json").is_err());
}

#[test]
fn player_connecting_can_be_refused() {
    let folder = Folder::new("connecting");
    folder.resource(
        "gate",
        "server_script 'main.lua'",
        &[("main.lua", "AddEventHandler('playerConnecting', function(name) if name == 'Bad' then SetKickReason('no bad names') CancelEvent() end end)")],
    );
    let (scripting, _) = scripting(&folder);
    assert_eq!(scripting.player_connecting("Niko"), None);
    scripting.start("gate").unwrap();
    assert_eq!(scripting.player_connecting("Niko"), None);
    assert_eq!(scripting.player_connecting("Bad").as_deref(), Some("no bad names"));
    assert_eq!(scripting.player_connecting("Niko"), None, "the reason does not stick");
}

fn client_scripting(folder: &Folder) -> (Scripting, Rc<RefCell<Vec<String>>>) {
    let log = Rc::new(RefCell::new(Vec::new()));
    let sink = log.clone();
    (Scripting::client(folder.0.clone(), move |line| sink.borrow_mut().push(line)), log)
}

#[test]
fn the_client_runs_client_scripts_and_talks_to_the_server() {
    let folder = Folder::new("client_side");
    folder.resource(
        "r",
        "server_script 's.lua'\nclient_script 'c.lua'",
        &[
            ("s.lua", "print('server side')"),
            (
                "c.lua",
                "print('client side', DropPlayer, TriggerClientEvent)\n\
                 AddEventHandler('onClientResourceStart', function(name) print('started', name) end)\n\
                 RegisterNetEvent('news', function(a, t) print('news', a, t.x) TriggerServerEvent('thanks', a, { y = 2 }) end)",
            ),
        ],
    );
    let (scripting, log) = client_scripting(&folder);
    scripting.start("r").unwrap();
    let out = lines(&log);
    assert!(out.contains(&"[r] client side\tnil\tnil".to_string()), "{out:?}");
    assert!(out.contains(&"[r] started\tr".to_string()), "{out:?}");
    assert!(!out.iter().any(|l| l.contains("server side")), "a client must not run server scripts");
    scripting.client_event_json("news", br#"["hi", {"x": 7}]"#).unwrap();
    assert!(lines(&log).contains(&"[r] news\thi\t7".to_string()));
    assert_eq!(scripting.take_server_events(), vec![("thanks".to_string(), r#"["hi",{"y":2}]"#.to_string())]);
}

#[test]
fn an_endless_loop_is_stopped() {
    let folder = Folder::new("endless");
    folder.resource("loop", "client_script 'c.lua'", &[("c.lua", "print('before') while true do end print('never')")]);
    let (scripting, log) = client_scripting(&folder);
    scripting.start("loop").unwrap();
    let out = lines(&log);
    assert!(out.contains(&"[loop] before".to_string()), "{out:?}");
    assert!(out.iter().any(|l| l.contains("ran too long")), "{out:?}");
    assert!(!out.iter().any(|l| l.contains("never")));
}
