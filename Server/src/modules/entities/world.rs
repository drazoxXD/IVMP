//! Server-owned entities (docs/onesync-plan.md, phase 4).
//!
//! The server decides that a vehicle or ped exists; a client creates it in its game (the one
//! nearest to it, else the host) as a networked mission entity and tells us its object id. From
//! then on the engine syncs it like any other entity. If it disappears (its owner left and it
//! wasn't handed over, or it got deleted), the server has it created again where it was last
//! seen.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use ivmp_sync::registry::Registry;
use NetProtocol::{ClientId, SpawnKind};

/// How long a spawn may take before it's asked again (model streaming, a lost packet).
const SPAWN_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a server entity may be missing from the registry before it's created again.
const MISSING_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct ServerEntity {
    pub kind: SpawnKind,
    pub model: u32,
    pub position: [f32; 3],
    pub heading: f32,
    /// Network object id once created.
    pub object_id: Option<u16>,
    /// The client asked to create it, and when.
    pub creator: Option<(ClientId, Instant)>,
    /// When the registry last had it.
    pub last_seen: Instant,
    /// The registry has seen it. It only does once another player gets it as a clone, so an
    /// entity nobody else sees yet is never taken for missing.
    pub confirmed: bool,
}

/// A spawn the server should send.
pub struct SpawnOrder {
    pub request: u32,
    pub client: ClientId,
    pub entity: ServerEntity,
}

#[derive(Default)]
pub struct World {
    /// By request id (the server entity's id).
    pub entities: BTreeMap<u32, ServerEntity>,
    next_request: u32,
}

/// The model hash for a name (GET_HASH_KEY: Jenkins one-at-a-time over the lowercase name), or
/// a hex hash as `0x...`.
pub fn model_hash(name: &str) -> u32 {
    if let Some(hex) = name.strip_prefix("0x") {
        if let Ok(hash) = u32::from_str_radix(hex, 16) {
            return hash;
        }
    }
    let mut h: u32 = 0;
    for c in name.to_ascii_lowercase().bytes() {
        h = h.wrapping_add(c as u32);
        h = h.wrapping_add(h << 10);
        h ^= h >> 6;
    }
    h = h.wrapping_add(h << 3);
    h ^= h >> 11;
    h.wrapping_add(h << 15)
}

impl World {
    /// Adds a server entity; [`World::orders`] sends it to a client.
    pub fn add(&mut self, kind: SpawnKind, model: u32, position: [f32; 3], heading: f32) -> u32 {
        self.next_request += 1;
        let entity = ServerEntity {
            kind,
            model,
            position,
            heading,
            object_id: None,
            creator: None,
            last_seen: Instant::now(),
            confirmed: false,
        };
        self.entities.insert(self.next_request, entity);
        self.next_request
    }

    pub fn remove(&mut self, request: u32) -> Option<ServerEntity> {
        self.entities.remove(&request)
    }

    /// A client created (or failed to create, `object_id` 0) server entity `request`.
    pub fn spawned(&mut self, request: u32, client: ClientId, object_id: u16) -> Option<&ServerEntity> {
        let entity = self.entities.get_mut(&request)?;
        if entity.creator.map(|(c, _)| c) != Some(client) {
            return None;
        }
        if object_id == 0 {
            // Ask again later (maybe someone else).
            entity.creator = None;
        } else {
            entity.object_id = Some(object_id);
            entity.last_seen = Instant::now();
            entity.confirmed = false;
        }
        Some(entity)
    }

    /// Follows the server entities in the registry and returns the spawns to send: new
    /// entities, timed-out spawns and entities that disappeared. `clients` are the connected
    /// clients with their player's position when known; `host` is the fallback.
    pub fn orders(&mut self, registry: &Registry, clients: &[(ClientId, Option<[f32; 3]>)]) -> Vec<SpawnOrder> {
        let mut orders = Vec::new();
        let connected = |id: ClientId| clients.iter().any(|(c, _)| *c == id);
        for (request, entity) in &mut self.entities {
            let creator = entity.creator.map(|(c, _)| c);
            if let Some(object_id) = entity.object_id {
                match registry.entities.get(&object_id) {
                    Some(seen) => {
                        entity.last_seen = Instant::now();
                        entity.confirmed = true;
                        if let Some(position) = seen.position {
                            entity.position = position;
                        }
                        continue;
                    }
                    // Nobody got it as a clone yet: it lives as long as its creator is here.
                    None if !entity.confirmed && creator.is_some_and(connected) => continue,
                    None if entity.confirmed && entity.last_seen.elapsed() < MISSING_TIMEOUT => continue,
                    None => {
                        entity.object_id = None;
                        entity.creator = None;
                    }
                }
            }
            // A spawn in progress is asked again of the same client (it ignores repeats); only
            // a client that left is replaced.
            let client = match entity.creator {
                Some((c, at)) if connected(c) && at.elapsed() < SPAWN_TIMEOUT => continue,
                Some((c, _)) if connected(c) => c,
                _ => match nearest(clients, entity.position) {
                    Some(c) => c,
                    None => continue,
                },
            };
            entity.creator = Some((client, Instant::now()));
            orders.push(SpawnOrder { request: *request, client, entity: entity.clone() });
        }
        orders
    }
}

/// The client whose player is nearest to `position`; clients without a known position count as
/// far away, so the first one is the fallback.
fn nearest(clients: &[(ClientId, Option<[f32; 3]>)], position: [f32; 3]) -> Option<ClientId> {
    let distance = |p: Option<[f32; 3]>| {
        p.map_or(f32::MAX, |p| (0..3).map(|i| (p[i] - position[i]).powi(2)).sum::<f32>())
    };
    clients
        .iter()
        .min_by(|a, b| distance(a.1).total_cmp(&distance(b.1)))
        .map(|(id, _)| *id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_hashes() {
        assert_eq!(model_hash("infernus"), 0x18F2_5AC7);
        assert_eq!(model_hash("INFERNUS"), model_hash("infernus"));
        assert_eq!(model_hash("0x12345678"), 0x1234_5678);
    }

    #[test]
    fn picks_the_nearest_client() {
        let clients = [(1, Some([0.0, 0.0, 0.0])), (2, Some([100.0, 0.0, 0.0])), (3, None)];
        assert_eq!(nearest(&clients, [90.0, 0.0, 0.0]), Some(2));
        assert_eq!(nearest(&[(3, None), (4, None)], [0.0; 3]), Some(3));
    }
}
