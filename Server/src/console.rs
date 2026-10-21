//! Server console: commands typed into the server window, read on a background thread.
//!
//! * `spawn car <model> [x y z [heading]]`, `spawn ped <model> [x y z [heading]]`: a
//!   server-owned entity; without a position it goes next to the first player.
//! * `despawn <id>`: forget a server entity (the game keeps it until it's cleaned up).
//! * `entities`: list the server entities.
//! * `help`

use std::sync::mpsc::{self, Receiver};

use NetProtocol::SpawnKind;

pub enum Command {
    Spawn { kind: SpawnKind, model: String, position: Option<[f32; 3]>, heading: f32 },
    Despawn(u32),
    Entities,
    Help,
}

pub const HELP: &str = "commands: spawn car|ped <model> [x y z [heading]], despawn <id>, entities, help";

/// Starts reading stdin; commands arrive on the receiver.
pub fn start() -> Receiver<Result<Command, String>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            if sender.send(parse(&line)).is_err() {
                break;
            }
        }
    });
    receiver
}

pub fn parse(line: &str) -> Result<Command, String> {
    let words: Vec<&str> = line.split_whitespace().collect();
    match words.as_slice() {
        ["spawn", kind, model, rest @ ..] => {
            let kind = match *kind {
                "car" | "vehicle" => SpawnKind::Vehicle,
                "ped" => SpawnKind::Ped,
                other => return Err(format!("unknown kind {other:?} (car or ped)")),
            };
            let numbers: Vec<f32> = rest
                .iter()
                .map(|w| w.parse::<f32>().map_err(|_| format!("not a number: {w:?}")))
                .collect::<Result<_, _>>()?;
            let (position, heading) = match numbers.as_slice() {
                [] => (None, 0.0),
                [x, y, z] => (Some([*x, *y, *z]), 0.0),
                [x, y, z, h] => (Some([*x, *y, *z]), *h),
                _ => return Err("give x y z [heading] or no position".into()),
            };
            Ok(Command::Spawn { kind, model: model.to_string(), position, heading })
        }
        ["despawn", id] => id.parse().map(Command::Despawn).map_err(|_| format!("not an id: {id:?}")),
        ["entities"] => Ok(Command::Entities),
        ["help"] => Ok(Command::Help),
        _ => Err(format!("unknown command; {HELP}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_spawn() {
        match parse("spawn car infernus 10 20 30 90").unwrap() {
            Command::Spawn { kind: SpawnKind::Vehicle, model, position: Some(p), heading } => {
                assert_eq!(model, "infernus");
                assert_eq!(p, [10.0, 20.0, 30.0]);
                assert_eq!(heading, 90.0);
            }
            _ => panic!("wrong command"),
        }
        assert!(matches!(parse("spawn ped m_y_cop").unwrap(), Command::Spawn { position: None, .. }));
        assert!(parse("spawn boat x").is_err());
    }
}
