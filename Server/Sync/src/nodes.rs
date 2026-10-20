//! Entity positions from the engine's sync nodes (GTA IV 1.2.0.59, docs/onesync-plan.md).
//!
//! A net object's data is a list of nodes picked by a mask, lowest bit first. Every type starts
//! with the CNetObjEntity nodes (0x48B970): 0 = flags (9 bits), 1 = type state (size depends
//! on the type and on the script flag, bit 3 of the create flags), 2 = map sector, 3 = position
//! in the sector. Reaching the position means skipping nodes 0 and 1, so this mirrors their
//! read functions (mode 0) for every type.
//!
//! Clone creates hold a type-specific header (vtable +0x94), then the nodes of the create mask
//! (vtable +0xA4: nodes 0..4 unless the entity is attached or a ped sits in a vehicle).

use crate::bits::BitReader;
use crate::packet::CloneCreate;

pub const PLAYER: u8 = 0;
pub const PED: u8 = 1;
pub const DUMMYPED: u8 = 2;
pub const AUTOMOBILE: u8 = 3;
pub const OBJECT: u8 = 4;
pub const BIKE: u8 = 5;
pub const TRAIN: u8 = 6;
pub const BOAT: u8 = 7;
pub const HELI: u8 = 8;
pub const PLANE: u8 = 9;

/// The create flag of a script (mission) entity; its nodes carry extra fields.
pub const SCRIPT_FLAG: u8 = 8;

/// Map sector (CNetObjEntity +0x11E..0x120), 7 bits each (node 2, 0x48C680).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sector(pub u8, pub u8, pub u8);

/// Position inside the sector, 12 bits each over the sector size (node 3, 0x48C9B0).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Local(pub u16, pub u16, pub u16);

impl Sector {
    /// World position (0x48C9B0 read case): sectors are 54 x 54 x 50 units, 64 is the origin.
    pub fn world(self, local: Local) -> [f32; 3] {
        let unit = |raw: u16, size: f32| raw as f32 / 4095.0 * size;
        [
            (self.0 as f32 - 64.0) * 54.0 + unit(local.0, 54.0) + 162.0,
            (self.1 as f32 - 64.0) * 54.0 + unit(local.1, 54.0),
            (self.2 as f32 - 64.0) * 50.0 + unit(local.2, 50.0) - 975.0,
        ]
    }
}

/// What a create or sync said about where an entity is.
#[derive(Debug, Default, Clone, Copy)]
pub struct Placement {
    pub sector: Option<Sector>,
    pub local: Option<Local>,
    /// A full world position (world objects carry one in their create data).
    pub world: Option<[f32; 3]>,
}

fn flag(r: &mut BitReader) -> Option<bool> {
    Some(r.read(1)? != 0)
}

/// CNetObjPhysical node 1 (0x4B3390) with its bases: entity (0x48C320, 4 bits for script
/// entities), dynamic entity (0x4EE740: 6 + 32 + 1), physical (3).
fn physical_state(r: &mut BitReader, script: bool) -> Option<()> {
    r.skip(if script { 4 } else { 0 } + 39 + 3)
}

/// CNetObjPed node 1 part (0x49F6F0).
fn ped_state(r: &mut BitReader, script: bool) -> Option<()> {
    r.skip(1 + 2 + 7)?;
    let weapon = r.read(6)?;
    r.skip(1)?;
    r.skip(if weapon == 0x2E { 14 } else { 1 })?;
    if flag(r)? {
        r.skip(14)?;
    }
    if flag(r)? {
        r.skip(14 + 3)?;
    }
    if script {
        r.skip(2 + 13 + 8 + 16 + 5)?;
    }
    Some(())
}

/// CNetObjPlayer node 1 part (0x4B9A50, read case at 0x4B9BC0).
fn player_state(r: &mut BitReader) -> Option<()> {
    r.skip(1 + 8 + 3 + 3 + 4 + 1 + 7 + 1)?;
    if flag(r)? {
        r.skip(14 + 12 + 12)?;
    }
    Some(())
}

/// CNetObjVehicle node 1 part (0x4CCB20).
fn vehicle_state(r: &mut BitReader, script: bool) -> Option<()> {
    r.skip(5 + 1 + 1 + 5 + 6 + 1 + 1 + if script { 11 } else { 0 })
}

/// Skips node 1, the type's state node (vtable +0x10C).
fn state_node(r: &mut BitReader, object_type: u8, script: bool) -> Option<()> {
    match object_type {
        PLAYER => {
            physical_state(r, script)?;
            ped_state(r, script)?;
            player_state(r)
        }
        PED => {
            physical_state(r, script)?;
            ped_state(r, script)
        }
        DUMMYPED => r.skip(if script { 4 } else { 0 } + 39),
        AUTOMOBILE | HELI | PLANE => {
            physical_state(r, script)?;
            vehicle_state(r, script)?;
            r.skip(if script { 1 } else { 0 })
        }
        BIKE => {
            physical_state(r, script)?;
            vehicle_state(r, script)?;
            r.skip(1)
        }
        BOAT => {
            physical_state(r, script)?;
            vehicle_state(r, script)?;
            r.skip(3)
        }
        TRAIN => {
            physical_state(r, script)?;
            vehicle_state(r, script)
        }
        OBJECT => {
            // 0x4D7710.
            physical_state(r, script)?;
            if flag(r)? {
                let wide = flag(r)?;
                r.skip(if wide { 21 } else { 8 })?;
            }
            r.skip(if script { 7 } else { 0 })
        }
        _ => None,
    }
}

/// Reads nodes 0..3 of `mask` and returns the sector and position when present.
pub fn read_nodes(r: &mut BitReader, object_type: u8, script: bool, mask: u32) -> Option<Placement> {
    let mut placement = Placement::default();
    if mask & 1 != 0 {
        r.skip(9)?;
    }
    if mask & 2 != 0 {
        state_node(r, object_type, script)?;
    }
    if mask & 4 != 0 {
        placement.sector = Some(Sector(r.read(7)? as u8, r.read(7)? as u8, r.read(7)? as u8));
    }
    if mask & 8 != 0 {
        placement.local = Some(Local(r.read(12)? as u16, r.read(12)? as u16, r.read(12)? as u16));
    }
    Some(placement)
}

/// Signed value over `range` (0x4BB700): sign bit, then `bits - 1` bits of magnitude.
fn signed(r: &mut BitReader, bits: usize, range: f32) -> Option<f32> {
    let negative = flag(r)?;
    let magnitude = r.read(bits - 1)? as f32 / ((1u32 << (bits - 1)) - 1) as f32 * range;
    Some(if negative { -magnitude } else { magnitude })
}

/// Skips a create's type header (vtable +0x94). Returns false when the position nodes don't
/// follow (a ped in a vehicle, or a world object, whose header holds `placement.world`).
fn create_header(r: &mut BitReader, object_type: u8, script: bool, placement: &mut Placement) -> Option<bool> {
    // CNetObjVehicle (0x4CA560).
    let vehicle = |r: &mut BitReader| r.skip(32 + 3 + if script { 11 } else { 0 } + 3 + 4);
    match object_type {
        PLAYER => {
            // 0x4B9340: physical index, model, in vehicle.
            r.skip(7 + 32)?;
            Some(!flag(r)?)
        }
        PED => {
            // 0x49E060.
            r.skip(2 + 32 + 16)?;
            if script {
                r.skip(8 + 1)?;
                if flag(r)? {
                    r.skip(10)?;
                }
                r.skip(3)?;
            }
            if flag(r)? {
                r.skip(32)?;
            }
            let in_vehicle = flag(r)?;
            r.skip(1)?;
            Some(!in_vehicle)
        }
        DUMMYPED => {
            // 0x4D9F70.
            r.skip(32)?;
            if flag(r)? {
                r.skip(32 + 6)?;
            }
            r.skip(20 + 32 + 32)?;
            Some(true)
        }
        AUTOMOBILE | PLANE | HELI => {
            // 0x4E4D50 (and 0x4DABB0 for helis).
            vehicle(r)?;
            if !flag(r)? {
                r.skip(6)?;
            }
            if object_type == HELI {
                if flag(r)? {
                    r.skip(6)?;
                }
                r.skip(2)?;
            }
            Some(true)
        }
        BIKE | TRAIN | BOAT => {
            vehicle(r)?;
            Some(true)
        }
        OBJECT => {
            // 0x4D6C10: world objects (not script, kind other than 3 and 6) are found by a
            // world position (0x4BB810).
            let kind = r.read(3)? + 1;
            if !script && kind != 6 && kind != 3 {
                let x = signed(r, 19, 3456.0)? + 162.0;
                let y = signed(r, 19, 3456.0)?;
                let z = r.read(19)? as f32 / ((1u32 << 19) - 1) as f32 * 3200.0 - 975.0;
                placement.world = Some([x, y, z]);
                return Some(false);
            }
            r.skip(32 + if script { 8 } else { 0 } + 1)?;
            Some(true)
        }
        _ => None,
    }
}

/// Where a clone create puts the entity.
pub fn create_placement(create: &CloneCreate) -> Option<Placement> {
    let mut r = BitReader::new(&create.data);
    r.truncate(create.data_bits);
    let script = create.flags & SCRIPT_FLAG != 0;
    let mut placement = Placement::default();
    if !create_header(&mut r, create.object_type, script, &mut placement)? {
        return Some(placement);
    }
    read_nodes(&mut r, create.object_type, script, 0b1111)
}

/// World position from what is known about an entity.
pub fn world_position(sector: Option<Sector>, local: Option<Local>) -> Option<[f32; 3]> {
    Some(sector?.world(local?))
}
