//! Positional sounds for client scripts (docs/audio.md): `PlaySoundAtCoords`, `PlaySoundFrontend`,
//! `StopSound`, `SetSoundCoords`, `SetSoundVolume`.
//!
//! The game's audio engine only plays its own bank formats, so the sounds (wav, mp3, ogg, flac files
//! of a resource) play in the web layer (`web/host/ui/audio.js`, WebAudio, a 3D panner each). This file
//! keeps the list of sounds, tells the page where the listener is (the game camera) about thirty
//! times a second, and works out the occlusion of every sound ten times a second (0 clear, 1 fully
//! blocked: the page makes it quieter and muffled), the way the game muffles its own sounds.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use ivmp_natives::natives;
use ivmp_scripting::mlua::{self, Function, Lua, Table};

struct Sound {
    resource: String,
    /// None for a frontend sound (no position, nothing to occlude).
    position: Option<[f32; 3]>,
    /// The occlusion the page was last told.
    occlusion: f32,
}

static SOUNDS: Mutex<Option<HashMap<u32, Sound>>> = Mutex::new(None);
static NEXT_ID: AtomicU32 = AtomicU32::new(1);

fn with_sounds<R>(f: impl FnOnce(&mut HashMap<u32, Sound>) -> R) -> R {
    f(SOUNDS.lock().unwrap().get_or_insert_with(HashMap::new))
}

/// A plain relative path with `/`: the sound is a file of the resource, nothing outside it.
fn plain_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 200
        && !path.starts_with('/')
        && path.split('/').all(|part| !part.is_empty() && part != "." && part != ".." && !part.contains(['\\', ':', '\0']))
}

fn emit(name: &str, json: serde_json::Value) {
    crate::web::emit(name, &json.to_string());
}

/// Starts a sound: a file of `resource`, at `position` (None: a frontend sound, no position).
pub fn play(resource: &str, path: &str, position: Option<[f32; 3]>, volume: f32, radius: f32, looped: bool) -> Result<u32, String> {
    if !plain_path(path) {
        return Err(format!("'{path}' is not a file of the resource"));
    }
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let [x, y, z] = position.unwrap_or([0.0; 3]);
    emit(
        "audio.play",
        serde_json::json!({
            "id": id, "url": format!("ivmp://{resource}/{path}"), "x": x, "y": y, "z": z,
            "volume": volume, "radius": radius, "loop": looped, "frontend": position.is_none(),
        }),
    );
    with_sounds(|s| s.insert(id, Sound { resource: resource.to_string(), position, occlusion: 0.0 }));
    Ok(id)
}

pub fn stop(id: u32) {
    if with_sounds(|s| s.remove(&id)).is_some() {
        emit("audio.stop", serde_json::json!({ "id": id }));
    }
}

/// A resource stopped: its sounds stop.
pub fn stop_resource(resource: &str) {
    let ids: Vec<u32> = with_sounds(|s| s.iter().filter(|(_, v)| v.resource == resource).map(|(k, _)| *k).collect());
    for id in ids {
        stop(id);
    }
}

pub fn set_coords(id: u32, position: [f32; 3]) {
    let known = with_sounds(|s| match s.get_mut(&id) {
        Some(sound) if sound.position.is_some() => {
            sound.position = Some(position);
            true
        }
        _ => false,
    });
    if known {
        emit("audio.coords", serde_json::json!({ "id": id, "x": position[0], "y": position[1], "z": position[2] }));
    }
}

pub fn set_volume(id: u32, volume: f32) {
    if with_sounds(|s| s.contains_key(&id)) {
        emit("audio.volume", serde_json::json!({ "id": id, "volume": volume }));
    }
}

/// How much is between the listener and a sound, 0 (nothing) to 1 (shut out).
fn occlusion(listener: [f32; 3], listener_interior: u32, sound: [f32; 3]) -> f32 {
    // The game's own probe counts the walls and doors between the two points (what its audio uses);
    // different interiors shut a sound out as well.
    let solids = natives::solids_between(listener, sound, 3);
    let interiors = natives::interior_at(sound).unwrap_or(0) != listener_interior;
    match solids {
        0 if interiors => 0.6,
        0 => 0.0,
        1 => 0.9,
        _ => 1.0,
    }
}

/// Call every frame on the game thread: the listener and the occlusion of the positional sounds.
pub fn tick() {
    static LAST: Mutex<Option<(Instant, Instant)>> = Mutex::new(None);
    if with_sounds(|s| s.is_empty()) {
        return;
    }
    let now = Instant::now();
    let (mut listener_due, mut occlusion_due) = (false, false);
    {
        let mut last = LAST.lock().unwrap();
        let (l, o) = last.get_or_insert((now - Duration::from_secs(1), now - Duration::from_secs(1)));
        if now.duration_since(*l) >= Duration::from_millis(33) {
            *l = now;
            listener_due = true;
        }
        if now.duration_since(*o) >= Duration::from_millis(100) {
            *o = now;
            occlusion_due = true;
        }
    }
    if !listener_due && !occlusion_due {
        return;
    }
    let Some((position, rotation)) = natives::camera() else { return };
    if listener_due {
        // The camera's Euler angles in degrees: heading 0 looks along +y, a positive heading turns left.
        // The player stands in front of a third person camera, so the way from the camera to the player
        // is where it looks; in first person (too close to tell) the camera's angles are used.
        let forward = match natives::player_position() {
            Some(p) if (p[0] - position[0]).hypot(p[1] - position[1]) > 0.8 => {
                let (dx, dy) = (p[0] - position[0], p[1] - position[1]);
                let len = dx.hypot(dy);
                [dx / len, dy / len, 0.0]
            }
            _ => {
                let (pitch, yaw) = (rotation[0].to_radians(), rotation[2].to_radians());
                [-yaw.sin() * pitch.cos(), yaw.cos() * pitch.cos(), pitch.sin()]
            }
        };
        emit("audio.listener", serde_json::json!({ "p": position, "f": forward, "u": [0.0, 0.0, 1.0] }));
    }
    if occlusion_due {
        // The ears are at the player (the camera can hang outside the wall the player stands behind).
        let position = natives::player_position().map(|p| [p[0], p[1], p[2] + 0.6]).unwrap_or(position);
        let listener_interior = natives::interior_at(position).unwrap_or(0);
        let sounds: Vec<(u32, [f32; 3], f32)> =
            with_sounds(|s| s.iter().filter_map(|(id, v)| v.position.map(|p| (*id, p, v.occlusion))).collect());
        for (id, sound_position, before) in sounds {
            let now = occlusion(position, listener_interior, sound_position);
            if (now - before).abs() > 0.02 {
                with_sounds(|s| {
                    if let Some(sound) = s.get_mut(&id) {
                        sound.occlusion = now;
                    }
                });
                emit("audio.occlusion", serde_json::json!({ "id": id, "o": now }));
            }
        }
    }
}

fn resource_name(lua: &Lua) -> mlua::Result<String> {
    lua.globals().get::<Function>("GetCurrentResourceName")?.call::<String>(())
}

fn option(options: &Option<Table>, key: &str, default: f64) -> f64 {
    options.as_ref().and_then(|t| t.get::<Option<f64>>(key).ok().flatten()).unwrap_or(default)
}

/// Adds the sound functions to a script's Lua state.
pub fn install(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();
    globals.set(
        "PlaySoundAtCoords",
        lua.create_function(|lua, (path, x, y, z, options): (String, f64, f64, f64, Option<Table>)| {
            let resource = resource_name(lua)?;
            let looped = options.as_ref().and_then(|t| t.get::<Option<bool>>("loop").ok().flatten()).unwrap_or(false);
            play(&resource, &path, Some([x as f32, y as f32, z as f32]), option(&options, "volume", 1.0) as f32, option(&options, "radius", 30.0) as f32, looped)
                .map_err(mlua::Error::runtime)
        })?,
    )?;
    globals.set(
        "PlaySoundFrontend",
        lua.create_function(|lua, (path, volume, looped): (String, Option<f64>, Option<bool>)| {
            let resource = resource_name(lua)?;
            play(&resource, &path, None, volume.unwrap_or(1.0) as f32, 0.0, looped.unwrap_or(false)).map_err(mlua::Error::runtime)
        })?,
    )?;
    globals.set("StopSound", lua.create_function(|_, id: u32| Ok(stop(id)))?)?;
    globals.set(
        "SetSoundCoords",
        lua.create_function(|_, (id, x, y, z): (u32, f64, f64, f64)| Ok(set_coords(id, [x as f32, y as f32, z as f32])))?,
    )?;
    globals.set("SetSoundVolume", lua.create_function(|_, (id, volume): (u32, f64)| Ok(set_volume(id, volume as f32)))?)?;
    globals.set(
        "SetAudioMasterVolume",
        lua.create_function(|_, volume: f64| Ok(emit("audio.master", serde_json::json!({ "volume": volume.clamp(0.0, 1.0) }))))?,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_paths_are_sounds() {
        assert!(plain_path("sounds/siren.mp3"));
        assert!(plain_path("a.wav"));
        for bad in ["", "/etc/x.wav", "../x.wav", "a/../b.wav", "C:/x.wav", "a\\b.wav", "a//b.wav"] {
            assert!(!plain_path(bad), "{bad}");
        }
    }
}
