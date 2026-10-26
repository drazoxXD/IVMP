//! Diagnostics: logs every GIVE_CONTROL_EVENT this game triggers (0x4AFEA0), so we can see why
//! a game hands away entities, e.g. the car its own player is driving.
//!
//! Two call sites are traced:
//! * 0x48D533, the proximity handover (0x48D1F0, every 500 ms per owned object): when the object
//!   is farther than a threshold from the local player's ped (player info 0x11A8808[local index]
//!   +0x598), it goes to the nearest player that may take it.
//! * 0x4C9EB4, the vehicle update (0x4C9BF0): a vehicle we own whose driver is a remote player
//!   (driver +0x219 set) goes to the driver's player.
//!
//! TriggerGiveControl is thiscall (event manager 0x18ECFB0) with stack arguments (target
//! netPlayer, net object, distance / 0, flag). Logged: site, object id and type, owner and
//! pending owner, target player index, the local player's index, ped and position, the entity's
//! position, and for vehicles the driver (is player, owner).

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use crate::{log, memory};

/// How many handovers to log per site.
const LOG_LIMIT: u32 = 300;

const PROXIMITY_CALL: usize = 0x0048_D533;
const DRIVER_CALL: usize = 0x004C_9EB4;
/// The local netPlayer (its player index is at +0x18).
const LOCAL_PLAYER: usize = 0x018E_5228;
/// CPlayerInfo pointers by player index (0x93FBE0); the player's ped is at +0x598.
const PLAYER_INFOS: usize = 0x011A_8808;

static ORIGINAL: AtomicUsize = AtomicUsize::new(0);
static COUNTS: [AtomicU32; 2] = [const { AtomicU32::new(0) }; 2];

unsafe fn read<T: Copy>(address: usize) -> Option<T> {
    if address < 0x10000 { None } else { Some(unsafe { std::ptr::read_unaligned(address as *const T) }) }
}

/// The position of a physical entity: its matrix (+0x20) row 3, or +0x10 without one.
unsafe fn position(entity: usize) -> Option<[f32; 3]> {
    unsafe {
        let matrix: usize = read(entity + 0x20)?;
        let at = if matrix != 0 { matrix + 0x30 } else { entity + 0x10 };
        Some([read(at)?, read(at + 4)?, read(at + 8)?])
    }
}

fn describe(position: Option<[f32; 3]>) -> String {
    position.map_or("?".into(), |p| format!("({:.1} {:.1} {:.1})", p[0], p[1], p[2]))
}

extern "C" fn record(site: usize, args: *const u32) {
    let slot = if site == PROXIMITY_CALL { 0 } else { 1 };
    if COUNTS[slot].fetch_add(1, Ordering::Relaxed) >= LOG_LIMIT {
        return;
    }
    unsafe {
        let (target, object, distance) = (*args as usize, *args.add(1) as usize, f32::from_bits(*args.add(2)));
        let target_index = read::<i8>(target + 0x18).unwrap_or(-1);
        let object_id = read::<u16>(object + 0xC).unwrap_or(0);
        let object_type = read::<u32>(object + 8).unwrap_or(99);
        let (owner, pending) = (read::<i8>(object + 0xF).unwrap_or(-1), read::<u8>(object + 0x10).unwrap_or(0));
        // The entity: net object vtable slot 1.
        let entity = read::<usize>(object).and_then(|vtable| read::<usize>(vtable + 4)).map_or(0, |get| {
            let get: extern "thiscall" fn(usize) -> usize = std::mem::transmute(get);
            get(object)
        });
        let local_index = read::<i8>(memory::from_ghidra(LOCAL_PLAYER) + 0x18).unwrap_or(-1);
        let info = if (0..32).contains(&local_index) {
            read::<usize>(memory::from_ghidra(PLAYER_INFOS) + local_index as usize * 4).unwrap_or(0)
        } else {
            0
        };
        let local_ped = read::<usize>(info + 0x598).unwrap_or(0);
        // Vehicles (AUTOMOBILE, BIKE, TRAIN, BOAT, HELI, PLANE): the driver at +0xF50.
        let driver = if matches!(object_type, 3 | 5..=9) && entity != 0 {
            let driver = read::<usize>(entity + 0xF50).unwrap_or(0);
            if driver == 0 {
                ", no driver".to_string()
            } else {
                let driver_object = read::<usize>(driver + 0x6C).unwrap_or(0);
                format!(
                    ", driver {driver:#x} (is player {}, local ped {}, owner {}, object {})",
                    read::<u8>(driver + 0x219).unwrap_or(0),
                    driver == local_ped,
                    read::<i8>(driver_object + 0xF).unwrap_or(-1),
                    read::<u16>(driver_object + 0xC).unwrap_or(0)
                )
            }
        } else {
            String::new()
        };
        log!(
            "give control ({}): object {object_id} type {object_type} owner {owner} pending {pending:#x} -> player {target_index}, \
             arg {distance:.1}; local player {local_index} ped {local_ped:#x} at {}, entity at {}{driver}",
            if slot == 0 { "proximity" } else { "driver" },
            describe(position(local_ped)),
            describe(position(entity)),
        );
    }
}

macro_rules! thunk {
    ($name:ident, $site:expr) => {
        #[unsafe(naked)]
        extern "C" fn $name() {
            core::arch::naked_asm!(
                "pushad",
                "pushfd",
                "lea eax, [esp + 40]",
                "push eax",
                "push {site}",
                "call {record}",
                "add esp, 8",
                "popfd",
                "popad",
                "jmp dword ptr [{orig}]",
                site = const $site,
                record = sym record,
                orig = sym ORIGINAL,
            );
        }
    };
}

thunk!(proximity_thunk, PROXIMITY_CALL);
thunk!(driver_thunk, DRIVER_CALL);

pub fn install() {
    for (site, thunk) in [(PROXIMITY_CALL, proximity_thunk as usize), (DRIVER_CALL, driver_thunk as usize)] {
        let address = memory::from_ghidra(site);
        if unsafe { *(address as *const u8) } != 0xE8 {
            log!("control trace: no call at {site:#x}");
            continue;
        }
        match unsafe { memory::redirect_call(address, thunk) } {
            Ok(original) => {
                ORIGINAL.store(original, Ordering::SeqCst);
                log!("control trace: logging give control at {site:#x}");
            }
            Err(e) => log!("control trace: hook at {site:#x} failed: {e}"),
        }
    }
}
