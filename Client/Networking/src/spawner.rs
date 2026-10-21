//! Creates the entities the server owns (docs/onesync-plan.md, phase 4).
//!
//! The server picks a client and sends [`ServerMessage::Spawn`](NetProtocol::ServerMessage); we
//! create the vehicle or ped with the engine's own factories as a mission entity (it isn't
//! cleaned up by population), networked, so its network object gets one of our object ids and
//! the other players get it as a clone. The answer carries that id, so the server can track it.
//!
//! This version's native hashes don't match the public lists, so the factories are called
//! directly (the same calls the clone create path makes):
//! * model hash -> model index 0x9DC6A0, streaming request 0x8D8750 / loaded check 0x8D8400 with
//!   the model module index at 0x12B4138
//! * CVehicleFactoryNY (instance at 0x166D9FC) slot 1, 0x5DA990: (model, pop type, matrix,
//!   networked); pop type 2 = mission
//! * CPedFactoryNY (instance at 0x166D9F4) slot 5, 0x5C3770: (pop type, model, matrix,
//!   networked, mission)
//! * add to the world 0x93E2D0 (entity, false)

use std::collections::VecDeque;
use std::sync::Mutex;

use NetProtocol::{ClientMessage, SpawnKind};

use crate::{log, memory, net};

const MODEL_INDEX: usize = 0x009D_C6A0;
const STREAMING_REQUEST: usize = 0x008D_8750;
const STREAMING_IS_LOADED: usize = 0x008D_8400;
const MODEL_MODULE: usize = 0x012B_4138;
const VEHICLE_FACTORY: usize = 0x0166_D9FC;
const PED_FACTORY: usize = 0x0166_D9F4;
const ADD_TO_WORLD: usize = 0x0093_E2D0;
/// Streaming flags the clone create path uses for vehicle and ped models.
const VEHICLE_STREAMING_FLAGS: u32 = 0x1A;
const PED_STREAMING_FLAGS: u32 = 0x18;
/// Pop type of mission entities: kept until removed on purpose.
const POP_TYPE_MISSION: u32 = 2;
/// Entity +0x6C = its network object; network object +0xC = object id.
const ENTITY_NET_OBJECT: usize = 0x6C;
const NET_OBJECT_ID: usize = 0xC;
/// Frames to wait for a model to stream in.
const LOAD_TIMEOUT_FRAMES: u32 = 600;
/// Frames our networked player must exist before we spawn anything: the factories touch the
/// local player (spawning right as the session started crashed in 0x5DA990 -> 0xA31B10).
const READY_FRAMES: u32 = 300;
/// Local player: index 0x1036F14 into the CPlayerInfo pointers at 0x11A8808; info +0x598 = ped.
const LOCAL_PLAYER_INDEX: usize = 0x0103_6F14;
const PLAYER_INFOS: usize = 0x011A_8808;
const PLAYER_INFO_PED: usize = 0x598;

pub struct Request {
    pub id: u32,
    pub kind: SpawnKind,
    pub model: u32,
    pub position: [f32; 3],
    pub heading: f32,
}

struct Pending {
    request: Request,
    model_index: i32,
    frames: u32,
}

static QUEUE: Mutex<VecDeque<Request>> = Mutex::new(VecDeque::new());
static LOADING: Mutex<Vec<Pending>> = Mutex::new(Vec::new());
/// Frames our networked player has existed for.
static READY: Mutex<u32> = Mutex::new(0);

/// Our player's ped exists and is networked.
fn local_player_networked() -> bool {
    unsafe {
        let index = *(memory::from_ghidra(LOCAL_PLAYER_INDEX) as *const u32);
        if index >= 32 {
            return false;
        }
        let info = *(memory::from_ghidra(PLAYER_INFOS + 4 * index as usize) as *const usize);
        if info == 0 {
            return false;
        }
        let ped = *((info + PLAYER_INFO_PED) as *const usize);
        ped != 0 && *((ped + ENTITY_NET_OBJECT) as *const usize) != 0
    }
}

/// Queues a spawn from the server (any thread); done by [`tick`].
pub fn request(request: Request) {
    log!(
        "spawner: server asks for {:?} {:#010x} at {:?} (request {})",
        request.kind,
        request.model,
        request.position,
        request.id
    );
    QUEUE.lock().unwrap().push_back(request);
}

/// Game thread, every frame once we are in the session.
pub fn tick() {
    {
        let mut ready = READY.lock().unwrap();
        *ready = if local_player_networked() { ready.saturating_add(1) } else { 0 };
        if *ready < READY_FRAMES {
            return;
        }
    }
    let new: Vec<Request> = QUEUE.lock().unwrap().drain(..).collect();
    let mut loading = LOADING.lock().unwrap();
    for request in new {
        let model_index = unsafe { model_index(request.model) };
        if model_index < 0 {
            log!("spawner: unknown model {:#010x} (request {})", request.model, request.id);
            answer(request.id, 0);
            continue;
        }
        let flags = match request.kind {
            SpawnKind::Vehicle => VEHICLE_STREAMING_FLAGS,
            SpawnKind::Ped => PED_STREAMING_FLAGS,
        };
        unsafe { request_model(model_index, flags) };
        loading.push(Pending { request, model_index, frames: 0 });
    }
    loading.retain_mut(|pending| {
        pending.frames += 1;
        if unsafe { is_loaded(pending.model_index) } {
            let object_id = unsafe { spawn(&pending.request, pending.model_index) };
            answer(pending.request.id, object_id);
            return false;
        }
        if pending.frames > LOAD_TIMEOUT_FRAMES {
            log!("spawner: model {:#010x} didn't load (request {})", pending.request.model, pending.request.id);
            answer(pending.request.id, 0);
            return false;
        }
        true
    });
}

fn answer(request: u32, object_id: u16) {
    net::send(&ClientMessage::Spawned { request, object_id });
}

unsafe fn model_index(hash: u32) -> i32 {
    let f: extern "C" fn(u32) -> i32 = unsafe { std::mem::transmute(memory::from_ghidra(MODEL_INDEX)) };
    f(hash)
}

unsafe fn model_module() -> i32 {
    unsafe { *(memory::from_ghidra(MODEL_MODULE) as *const i32) }
}

unsafe fn request_model(index: i32, flags: u32) {
    let f: extern "C" fn(i32, i32, u32) = unsafe { std::mem::transmute(memory::from_ghidra(STREAMING_REQUEST)) };
    f(index, unsafe { model_module() }, flags)
}

unsafe fn is_loaded(index: i32) -> bool {
    let f: extern "C" fn(i32, i32) -> bool = unsafe { std::mem::transmute(memory::from_ghidra(STREAMING_IS_LOADED)) };
    f(index, unsafe { model_module() })
}

/// rage Matrix34 as the factories take it: right, forward, up, position (each padded to 16).
fn matrix(position: [f32; 3], heading_degrees: f32) -> [f32; 16] {
    let (sin, cos) = heading_degrees.to_radians().sin_cos();
    [
        cos, sin, 0.0, 0.0, //
        -sin, cos, 0.0, 0.0, //
        0.0, 0.0, 1.0, 0.0, //
        position[0], position[1], position[2], 1.0,
    ]
}

/// Virtual method `slot` of the object at `this`.
unsafe fn method(this: usize, slot: usize) -> usize {
    unsafe { *((*(this as *const usize)) as *const usize).add(slot) }
}

/// Creates the entity; returns its network object id (0 = failed).
unsafe fn spawn(request: &Request, model_index: i32) -> u16 {
    let matrix = matrix(request.position, request.heading);
    let entity = unsafe {
        match request.kind {
            SpawnKind::Vehicle => {
                let factory = *(memory::from_ghidra(VEHICLE_FACTORY) as *const usize);
                let create: extern "thiscall" fn(usize, i32, u32, *const f32, u32) -> usize =
                    std::mem::transmute(method(factory, 1));
                create(factory, model_index, POP_TYPE_MISSION, matrix.as_ptr(), 1)
            }
            SpawnKind::Ped => {
                let factory = *(memory::from_ghidra(PED_FACTORY) as *const usize);
                let create: extern "thiscall" fn(usize, *const u16, i32, *const f32, u32, u32) -> usize =
                    std::mem::transmute(method(factory, 5));
                let pop_type = POP_TYPE_MISSION as u16;
                create(factory, &pop_type, model_index, matrix.as_ptr(), 1, 1)
            }
        }
    };
    if entity == 0 {
        log!("spawner: the factory made nothing (request {})", request.id);
        return 0;
    }
    unsafe {
        let add: extern "C" fn(usize, u32) = std::mem::transmute(memory::from_ghidra(ADD_TO_WORLD));
        add(entity, 0);
        let net_object = *((entity + ENTITY_NET_OBJECT) as *const usize);
        if net_object == 0 {
            log!("spawner: {entity:#x} isn't networked (request {})", request.id);
            return 0;
        }
        let object_id = *((net_object + NET_OBJECT_ID) as *const u16);
        log!("spawner: created {:?} {entity:#x} as object {object_id} (request {})", request.kind, request.id);
        object_id
    }
}
