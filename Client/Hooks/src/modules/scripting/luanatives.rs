//! Natives for client scripts (docs/scripting-plan.md, stage 4). Like in FiveM every native is a
//! global function, in any style of its name: `GET_PLAYER_CHAR(0)`, `GetPlayerChar(0)` and
//! `getPlayerChar(0)` are the same call (case and underscores don't matter), and a native that
//! writes into pointers returns those values: `local x, y, z = GetCharCoordinates(ped)`.
//! `InvokeNative(name or hash, ...)`, `Natives.NAME(...)`, `DeclareNative(name, hash, args, ret)`
//! and `NativeExists(name or hash)` still exist.
//!
//! The name table (about 3000 natives with typed signatures, `Client/Hooks/natives.json`, built into
//! the DLL) comes from the GTA IV script tooling's native lists, whose hashes are this game
//! build's; `Client/tools/gen_natives.py` makes it. More natives, or fixes, come from a
//! `natives.json` next to the DLL (same format, it wins) and from `DeclareNative` in scripts.
//! `nativetrace` in the F1 console shows which natives (and arguments) an action in the game calls.
//!
//! A signature has argument and return letters: `i` int, `u` unsigned, `f` float, `b` bool, `s`
//! string, `a` any (typed by the Lua value), `I` / `F` / `V` out parameter (an int / a float / a
//! vector of three floats the native writes: they come back after the return values), and for
//! returns `s` string and `v` vector (three floats). A native without a signature takes its
//! arguments as they are in Lua (integer, float, boolean, string) and returns its first slot as an
//! integer. Wrong arguments can crash the game: natives don't check them.

use std::collections::HashMap;
use std::ffi::CString;
use std::sync::Mutex;

use ivmp_natives::natives;
use ivmp_scripting::mlua::{self, Lua, Table, Value, Variadic};

use crate::log;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Arg {
    Int,
    Uint,
    Float,
    Bool,
    Str,
    Any,
    OutInt,
    OutFloat,
    OutVec,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Ret {
    Int,
    Uint,
    Float,
    Bool,
    Str,
    Vector,
}

#[derive(Clone, Debug)]
struct Signature {
    hash: u32,
    /// None: the arguments are typed by their Lua values.
    args: Option<Vec<Arg>>,
    ret: Vec<Ret>,
}

static TABLE: Mutex<Option<HashMap<String, Signature>>> = Mutex::new(None);

/// The natives this project has identified in this game build (name, hash); their arguments are
/// not typed yet.
/// Names this project used before the full table: kept as aliases of the table's names.
const KNOWN: &[(&str, u32)] = &[
    ("NETWORK_HOST_GAME", 0x5BEA05E2),
    ("NETWORK_IS_OPERATION_PENDING", 0x391E4575),
    ("NETWORK_HOST_SUCCEEDED", 0x1CA77E94),
    ("NETWORK_FIND_GAME", 0x5D4D0C86),
    ("NETWORK_IS_FIND_PENDING", 0x23D60810),
    ("NETWORK_FOUND_GAME_COUNT", 0x10DF4CED),
    ("NETWORK_JOIN_GAME", 0x60806A0C),
    ("NETWORK_IS_JOIN_PENDING", 0x76C53927),
    ("NETWORK_JOIN_SUCCEEDED", 0x59F24327),
    ("LOCAL_PLAYER_READY_TO_START", 0x5C03585C),
    ("LAUNCH_LOCAL_PLAYER_IN_NETWORK_GAME", 0x70FE415C),
    ("PLAYER_WANTS_TO_JOIN", 0x7D99343C),
    ("TELL_NET_PLAYER_TO_START_PLAYING", 0x465D424D),
    ("NETWORK_START_SESSION", 0x58802CE5),
];

fn parse_args(text: &str) -> Result<Vec<Arg>, String> {
    text.chars()
        .map(|c| match c {
            'i' => Ok(Arg::Int),
            'u' => Ok(Arg::Uint),
            'f' => Ok(Arg::Float),
            'b' => Ok(Arg::Bool),
            's' => Ok(Arg::Str),
            'a' => Ok(Arg::Any),
            'I' => Ok(Arg::OutInt),
            'F' => Ok(Arg::OutFloat),
            'V' => Ok(Arg::OutVec),
            other => Err(format!("'{other}' is not an argument type (i u f b s a I F V)")),
        })
        .collect()
}

fn parse_ret(text: &str) -> Result<Vec<Ret>, String> {
    text.chars()
        .map(|c| match c {
            'i' => Ok(Ret::Int),
            'u' => Ok(Ret::Uint),
            'f' => Ok(Ret::Float),
            'b' => Ok(Ret::Bool),
            's' => Ok(Ret::Str),
            'v' => Ok(Ret::Vector),
            other => Err(format!("'{other}' is not a return type (i u f b s v)")),
        })
        .collect()
}

/// The table of natives generated from the script tooling's lists (see the module docs).
const BUILT_IN: &str = include_str!("../../../natives.json");

fn with_table<R>(f: impl FnOnce(&mut HashMap<String, Signature>) -> R) -> R {
    let mut guard = TABLE.lock().unwrap();
    let table = guard.get_or_insert_with(|| {
        let mut table = HashMap::new();
        let count = load_text(&mut table, BUILT_IN, "the built-in table");
        for (name, hash) in KNOWN {
            table.entry(name.to_string()).or_insert(Signature { hash: *hash, args: None, ret: vec![Ret::Int] });
        }
        log!("natives: {count} natives known");
        table
    });
    f(table)
}

/// Adds or replaces a native; `args` / `ret` are signature letters (None: untyped / one int).
fn signature(hash: u32, args: Option<&str>, ret: Option<&str>) -> Result<Signature, String> {
    Ok(Signature {
        hash,
        args: args.map(parse_args).transpose()?,
        ret: ret.map(parse_ret).transpose()?.unwrap_or_else(|| vec![Ret::Int]),
    })
}

fn declare(name: &str, hash: u32, args: Option<&str>, ret: Option<&str>) -> Result<(), String> {
    let signature = signature(hash, args, ret)?;
    with_table(|t| t.insert(name.to_ascii_uppercase(), signature));
    Ok(())
}

fn parse_hash(value: &serde_json::Value) -> Option<u32> {
    match value {
        serde_json::Value::Number(n) => n.as_u64().and_then(|n| u32::try_from(n).ok()),
        serde_json::Value::String(s) => u32::from_str_radix(s.trim_start_matches("0x").trim_start_matches("0X"), 16).ok(),
        _ => None,
    }
}

/// Reads `natives.json`: `{ "NAME": "0xHASH" }` or `{ "NAME": { "hash": "0xHASH", "args": "iif", "ret": "b" } }`.
pub fn load_file(path: &std::path::Path) {
    let Ok(text) = std::fs::read_to_string(path) else { return };
    let added = with_table(|table| load_text(table, &text, &path.display().to_string()));
    log!("natives: {added} native(s) from {}", path.display());
}

/// Adds the natives of a `natives.json` text; returns how many.
fn load_text(table: &mut HashMap<String, Signature>, text: &str, source: &str) -> usize {
    let value: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            log!("natives: {source} is not valid JSON: {e}");
            return 0;
        }
    };
    let Some(map) = value.as_object() else {
        log!("natives: {source} must be a JSON object");
        return 0;
    };
    let mut added = 0;
    for (name, entry) in map {
        let (hash, args, ret) = match entry {
            serde_json::Value::Object(o) => (
                o.get("hash").and_then(parse_hash),
                o.get("args").and_then(|a| a.as_str()),
                o.get("ret").and_then(|r| r.as_str()),
            ),
            other => (parse_hash(other), None, None),
        };
        match hash.map(|hash| signature(hash, args, ret)) {
            Some(Ok(signature)) => {
                table.insert(name.to_ascii_uppercase(), signature);
                added += 1;
            }
            Some(Err(e)) => log!("natives: {name}: {e}"),
            None => log!("natives: {name}: needs a hash"),
        }
    }
    added
}

/// The name with only its lowercase letters and digits: `GET_PLAYER_CHAR`, `GetPlayerChar` and
/// `getplayerchar` are one native.
fn normalized(name: &str) -> String {
    name.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()).collect()
}

/// The table's name for a name in any style, if it is a native.
fn canonical(name: &str) -> Option<String> {
    // Natives made with DeclareNative after the index was built are found too, by their own name.
    let key = normalized(name);
    if key.len() < 3 {
        return None;
    }
    with_table(|t| {
        if let Some(found) = t.get(&name.to_ascii_uppercase()) {
            let _ = found;
            return Some(name.to_ascii_uppercase());
        }
        NORMALIZED
            .get_or_init(|| {
                let mut index: HashMap<String, String> = HashMap::new();
                let mut names: Vec<&String> = t.keys().collect();
                names.sort();
                for native in names {
                    index.entry(normalized(native)).or_insert_with(|| native.clone());
                }
                index
            })
            .get(&key)
            .cloned()
    })
}

static NORMALIZED: std::sync::OnceLock<HashMap<String, String>> = std::sync::OnceLock::new();

/// The name of a native by its hash, for the trace log.
pub fn name_of(hash: u32) -> Option<String> {
    static BY_HASH: std::sync::OnceLock<HashMap<u32, String>> = std::sync::OnceLock::new();
    BY_HASH
        .get_or_init(|| {
            let mut names: Vec<(String, u32)> = with_table(|t| t.iter().map(|(name, s)| (name.clone(), s.hash)).collect());
            // Several names for one hash: the longest (the full name, not an alias) is listed last and wins.
            names.sort_by_key(|(name, _)| (name.len(), name.clone()));
            names.into_iter().map(|(name, hash)| (hash, name)).collect()
        })
        .get(&hash)
        .cloned()
}

fn lua_error(message: impl Into<String>) -> mlua::Error {
    mlua::Error::runtime(message.into())
}

fn hash_of(value: &Value) -> Option<u32> {
    match value {
        Value::Integer(i) => Some(*i as u32),
        Value::Number(n) => Some(*n as u32),
        Value::String(s) => u32::from_str_radix(s.to_str().ok()?.trim_start_matches("0x"), 16).ok(),
        _ => None,
    }
}

/// A Lua value as a native argument slot, keeping strings alive in `strings`.
fn slot_of(value: &Value, kind: Option<Arg>, strings: &mut Vec<CString>) -> mlua::Result<u32> {
    let kind = kind.filter(|k| *k != Arg::Any);
    Ok(match (value, kind) {
        (Value::Integer(i), None | Some(Arg::Int) | Some(Arg::Uint)) => *i as i32 as u32,
        (Value::Number(n), None | Some(Arg::Float)) => (*n as f32).to_bits(),
        (Value::Number(n), Some(Arg::Int | Arg::Uint)) => *n as i64 as u32,
        (Value::Integer(i), Some(Arg::Float)) => (*i as f32).to_bits(),
        (Value::Boolean(b), None | Some(Arg::Bool)) => *b as u32,
        (Value::Integer(i), Some(Arg::Bool)) => (*i != 0) as u32,
        (Value::Nil, None) => 0,
        (Value::String(s), None | Some(Arg::Str)) => {
            let text = CString::new(s.as_bytes().to_vec()).map_err(|_| lua_error("a native string can't contain a 0 byte"))?;
            let pointer = text.as_ptr() as usize as u32;
            strings.push(text);
            pointer
        }
        (other, kind) => return Err(lua_error(format!("a {} is not a valid {kind:?} argument", other.type_name()))),
    })
}

fn invoke(lua: &Lua, args: Variadic<Value>) -> mlua::Result<Variadic<Value>> {
    let mut args = args.into_iter();
    let target = args.next().ok_or_else(|| lua_error("InvokeNative(name or hash, ...)"))?;
    let (signature, label) = match &target {
        Value::String(name) => {
            let name = canonical(&name.to_string_lossy()).unwrap_or_else(|| name.to_string_lossy().to_ascii_uppercase());
            match with_table(|t| t.get(&name).cloned()) {
                Some(signature) => (signature, name),
                None => {
                    return Err(lua_error(format!(
                        "unknown native '{name}': give its hash (InvokeNative(0x...)), DeclareNative it or add it to natives.json"
                    )));
                }
            }
        }
        other => {
            let hash = hash_of(other).ok_or_else(|| lua_error("the first argument is a native's name or hash"))?;
            (Signature { hash, args: None, ret: vec![Ret::Int] }, format!("{hash:#010x}"))
        }
    };
    let rest: Vec<Value> = args.collect();

    let mut slots: Vec<u32> = Vec::new();
    let mut strings: Vec<CString> = Vec::new();
    // Out parameters: the native writes into these, which stay alive (and in place) for the call.
    let mut outs: Vec<(Arg, Box<[u32; 4]>)> = Vec::new();
    match &signature.args {
        None => {
            for value in &rest {
                slots.push(slot_of(value, None, &mut strings)?);
            }
        }
        Some(kinds) => {
            let mut values = rest.iter();
            for kind in kinds {
                match kind {
                    Arg::OutInt | Arg::OutFloat | Arg::OutVec => {
                        let cell = Box::new([0u32; 4]);
                        slots.push(cell.as_ptr() as usize as u32);
                        outs.push((*kind, cell));
                    }
                    kind => {
                        let value = values.next().ok_or_else(|| lua_error(format!("{label}: too few arguments")))?;
                        slots.push(slot_of(value, Some(*kind), &mut strings)?);
                    }
                }
            }
            if values.next().is_some() {
                return Err(lua_error(format!("{label}: too many arguments")));
            }
        }
    }
    if slots.len() > 16 {
        return Err(lua_error(format!("{label}: a native takes at most 16 arguments")));
    }
    let returned = natives::invoke_slots(signature.hash, &slots)
        .ok_or_else(|| lua_error(format!("the native {label} does not exist in this game build")))?;

    let mut results: Vec<Value> = Vec::new();
    let mut next = 0usize;
    for kind in &signature.ret {
        let slot = |i: usize| returned[i.min(3)];
        match kind {
            Ret::Int => results.push(Value::Integer(slot(next) as i32 as i64)),
            Ret::Uint => results.push(Value::Integer(slot(next) as i64)),
            Ret::Float => results.push(Value::Number(f32::from_bits(slot(next)) as f64)),
            Ret::Bool => results.push(Value::Boolean(slot(next) & 0xFF != 0)),
            Ret::Str => {
                let pointer = slot(next) as usize as *const std::ffi::c_char;
                results.push(if pointer.is_null() {
                    Value::Nil
                } else {
                    Value::String(lua.create_string(unsafe { std::ffi::CStr::from_ptr(pointer) }.to_bytes())?)
                });
            }
            Ret::Vector => {
                for i in 0..3 {
                    results.push(Value::Number(f32::from_bits(slot(next + i)) as f64));
                }
                next += 2;
            }
        }
        next += 1;
    }
    for (kind, cell) in &outs {
        match kind {
            Arg::OutFloat => results.push(Value::Number(f32::from_bits(cell[0]) as f64)),
            Arg::OutVec => results.extend((0..3).map(|i| Value::Number(f32::from_bits(cell[i]) as f64))),
            _ => results.push(Value::Integer(cell[0] as i32 as i64)),
        }
    }
    drop(strings);
    Ok(Variadic::from(results))
}

/// Adds the native functions to a script's Lua state.
pub fn install(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();
    globals.set("InvokeNative", lua.create_function(|lua, args: Variadic<Value>| invoke(lua, args))?)?;
    globals.set(
        "DeclareNative",
        lua.create_function(|_, (name, hash, args, ret): (String, Value, Option<String>, Option<String>)| {
            let hash = hash_of(&hash).ok_or_else(|| lua_error("DeclareNative(name, hash, args, ret): the hash is a number or \"0x...\""))?;
            declare(&name, hash, args.as_deref(), ret.as_deref()).map_err(lua_error)
        })?,
    )?;
    globals.set(
        "NativeExists",
        lua.create_function(|_, target: Value| {
            let hash = match &target {
                Value::String(name) => canonical(&name.to_string_lossy()).and_then(|n| with_table(|t| t.get(&n).map(|s| s.hash))),
                other => hash_of(other),
            };
            Ok(hash.is_some_and(natives::exists))
        })?,
    )?;
    // Natives.NAME(...) is InvokeNative('NAME', ...).
    let table = lua.create_table()?;
    let meta = lua.create_table()?;
    meta.set(
        "__index",
        lua.create_function(|lua, (_, name): (Value, String)| {
            lua.create_function(move |lua, args: Variadic<Value>| {
                let mut call = vec![Value::String(lua.create_string(&name)?)];
                call.extend(args);
                invoke(lua, Variadic::from(call))
            })
        })?,
    )?;
    table.set_metatable(Some(meta))?;
    globals.set("Natives", table)?;

    // Every native is a global function, like in FiveM: a name that is no global yet but is a native
    // (in any style) becomes a function that calls it, kept in the globals for the next time. The
    // globals the runtime and the resource define come first, so `Wait` and `GetPlayerName` stay ours.
    let global_meta = match globals.metatable() {
        Some(existing) => existing,
        None => lua.create_table()?,
    };
    global_meta.set(
        "__index",
        lua.create_function(|lua, (globals, key): (Table, Value)| {
            let Value::String(key) = key else { return Ok(Value::Nil) };
            let name = key.to_string_lossy().to_string();
            let Some(native) = canonical(&name) else { return Ok(Value::Nil) };
            let function = lua.create_function(move |lua, args: Variadic<Value>| {
                let mut call = vec![Value::String(lua.create_string(&native)?)];
                call.extend(args);
                invoke(lua, Variadic::from(call))
            })?;
            globals.raw_set(name, function.clone())?;
            Ok(Value::Function(function))
        })?,
    )?;
    globals.set_metatable(Some(global_meta))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_parse() {
        assert_eq!(parse_args("ifsbIFaV").unwrap(), [Arg::Int, Arg::Float, Arg::Str, Arg::Bool, Arg::OutInt, Arg::OutFloat, Arg::Any, Arg::OutVec]);
        assert!(parse_args("x").is_err());
        assert_eq!(parse_ret("bv").unwrap(), [Ret::Bool, Ret::Vector]);
    }

    #[test]
    fn declared_natives_are_found_case_insensitively() {
        declare("Test_Native", 0x1234, Some("if"), Some("b")).unwrap();
        let signature = with_table(|t| t.get("TEST_NATIVE").cloned()).unwrap();
        assert_eq!(signature.hash, 0x1234);
        assert_eq!(signature.args.unwrap(), [Arg::Int, Arg::Float]);
        assert!(with_table(|t| t.contains_key("NETWORK_HOST_GAME")), "the known natives are built in");
    }

    #[test]
    fn the_built_in_table_has_the_game_natives_with_signatures() {
        let player = with_table(|t| t.get("GET_PLAYER_CHAR").cloned()).unwrap();
        assert_eq!(player.hash, 0x511454A9);
        assert_eq!(player.args.unwrap(), [Arg::Int, Arg::OutInt]);
        let coords = with_table(|t| t.get("GET_CHAR_COORDINATES").cloned()).unwrap();
        assert_eq!(coords.args.unwrap(), [Arg::Int, Arg::OutFloat, Arg::OutFloat, Arg::OutFloat]);
        assert!(coords.ret.is_empty(), "a void native returns nothing but its out parameters");
        let name = with_table(|t| t.get("GET_PLAYER_NAME").cloned()).unwrap();
        assert_eq!(name.ret, [Ret::Str]);
        assert!(with_table(|t| t.len()) > 2900);
    }

    #[test]
    fn natives_json_adds_natives() {
        let path = std::env::temp_dir().join(format!("ivmp_natives_{}.json", std::process::id()));
        std::fs::write(&path, r#"{ "JSON_ONE": "0xAB", "JSON_TWO": { "hash": 4660, "args": "ff", "ret": "v" } }"#).unwrap();
        load_file(&path);
        let one = with_table(|t| t.get("JSON_ONE").cloned()).unwrap();
        let two = with_table(|t| t.get("JSON_TWO").cloned()).unwrap();
        assert_eq!((one.hash, two.hash), (0xAB, 4660));
        assert_eq!(two.ret, [Ret::Vector]);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_native_is_found_by_its_name_in_any_style() {
        for name in ["GET_PLAYER_CHAR", "GetPlayerChar", "getPlayerChar", "getplayerchar", "get_Player_char"] {
            assert_eq!(canonical(name).as_deref(), Some("GET_PLAYER_CHAR"), "{name}");
        }
        assert_eq!(canonical("GetCharCoordinates").as_deref(), Some("GET_CHAR_COORDINATES"));
        assert_eq!(canonical("Wait2Frames"), None);
        assert_eq!(canonical("ab"), None, "too short to be a native");
        assert_eq!(canonical("SomethingNobodyDefined"), None);
    }
}
