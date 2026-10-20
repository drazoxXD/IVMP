//! Every networked entity the server has seen, from the clients' clone creates.

use std::collections::BTreeMap;
use std::time::Instant;

use crate::nodes::{self, Local, Sector};
use crate::packet::{self, CloneCreate, OBJECT_TYPES, id};

#[derive(Debug, Clone)]
pub struct Entity {
    pub object_type: u8,
    pub owner: u8,
    pub created: Instant,
    /// The last create data, to replay to players who connect later (phase 3).
    pub create: CloneCreate,
    pub sector: Option<Sector>,
    pub local: Option<Local>,
    /// Last known world position.
    pub position: Option<[f32; 3]>,
}

impl Entity {
    fn place(&mut self, placement: nodes::Placement) {
        self.sector = placement.sector.or(self.sector);
        self.local = placement.local.or(self.local);
        self.position = placement.world.or(nodes::world_position(self.sector, self.local)).or(self.position);
    }
}

#[derive(Default)]
pub struct Registry {
    pub entities: BTreeMap<u16, Entity>,
    pub clone_syncs: u64,
    pub migrations: u64,
    /// Creates whose position layout decoded.
    pub placed_creates: u64,
    pub packets: u64,
}

impl Registry {
    /// Looks at one game packet sent by client `from`; returns lines worth logging.
    pub fn observe(&mut self, from: u8, packet: &[u8]) -> Vec<String> {
        let mut events = Vec::new();
        self.packets += 1;
        for frame in packet::frames(packet) {
            for message in packet::messages(&frame) {
                match message.id {
                    id::PACKED_RELIABLES => {
                        let Some(reliables) = packet::packed_reliables(message.body) else { continue };
                        for create in reliables.creates {
                            if !self.entities.contains_key(&create.object_id) {
                                // A block that isn't the sender's means an id the server didn't
                                // give it, which can collide with another client's entity.
                                let outside = match crate::ids::client_of(create.object_id) {
                                    Some(client) if client != from => " (outside its id block)",
                                    _ => "",
                                };
                                events.push(format!(
                                    "entity {} {} created by client {from}{outside}",
                                    create.object_id, OBJECT_TYPES[create.object_type as usize]
                                ));
                            }
                            let placement = nodes::create_placement(&create);
                            if placement.is_some() {
                                self.placed_creates += 1;
                            }
                            let mut entity = Entity {
                                object_type: create.object_type,
                                owner: from,
                                created: Instant::now(),
                                create,
                                sector: None,
                                local: None,
                                position: None,
                            };
                            entity.place(placement.unwrap_or_default());
                            self.entities.insert(entity.create.object_id, entity);
                        }
                        for object_id in reliables.removes {
                            // Only the owner removes an entity for everyone.
                            if self.entities.get(&object_id).is_some_and(|e| e.owner == from) {
                                self.entities.remove(&object_id);
                            }
                        }
                    }
                    id::CLONE_SYNC => {
                        let Some(mut sync) = packet::clone_sync(message.body) else { continue };
                        self.clone_syncs += 1;
                        // Only the owner syncs an entity, so a sync from someone else means the
                        // entity migrated. The type changes when a dummy ped becomes a ped.
                        if let Some(entity) = self.entities.get_mut(&sync.object_id) {
                            if entity.owner != from {
                                events.push(format!(
                                    "entity {} {} now owned by client {from} (was {})",
                                    sync.object_id, OBJECT_TYPES[sync.object_type as usize], entity.owner
                                ));
                                entity.owner = from;
                                self.migrations += 1;
                            }
                            entity.object_type = sync.object_type;
                            let script = entity.create.flags & nodes::SCRIPT_FLAG != 0;
                            if let Some(placement) =
                                nodes::read_nodes(&mut sync.nodes, sync.object_type, script, sync.mask & 0b1111)
                            {
                                entity.place(placement);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        events
    }

    /// Entity counts by owner and type, for a periodic summary.
    pub fn summary(&self) -> String {
        let mut counts: BTreeMap<(u8, &str), u32> = BTreeMap::new();
        for entity in self.entities.values() {
            *counts.entry((entity.owner, OBJECT_TYPES[entity.object_type as usize])).or_default() += 1;
        }
        let parts: Vec<String> =
            counts.iter().map(|((owner, kind), n)| format!("client {owner}: {n} {kind}")).collect();
        let placed = self.entities.values().filter(|e| e.position.is_some()).count();
        let players: Vec<String> = self
            .entities
            .iter()
            .filter(|(_, e)| e.object_type == nodes::PLAYER)
            .map(|(id, e)| match e.position {
                Some([x, y, z]) => format!("player {id} (client {}) at {x:.0} {y:.0} {z:.0}", e.owner),
                None => format!("player {id} (client {}) at ?", e.owner),
            })
            .collect();
        format!(
            "{} entities ({}), {placed} with a position; {}; {} clone syncs, {} owner changes in {} packets",
            self.entities.len(),
            parts.join(", "),
            players.join(", "),
            self.clone_syncs,
            self.migrations,
            self.packets
        )
    }
}
