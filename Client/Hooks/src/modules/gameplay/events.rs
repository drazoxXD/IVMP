//! Game event bus. Hooks emit events; networking, sync and scripting subscribe.

use std::sync::RwLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameEvent {
    /// Once per rendered frame, on the game's render thread, before Present.
    Frame,
    /// The game reset its D3D device (alt-tab, resolution change).
    DeviceReset,
}

pub type Handler = fn(GameEvent);

static HANDLERS: RwLock<Vec<Handler>> = RwLock::new(Vec::new());

#[allow(dead_code)] // for the script engine, once there is one
pub fn subscribe(handler: Handler) {
    HANDLERS.write().unwrap().push(handler);
}

pub fn emit(event: GameEvent) {
    for handler in HANDLERS.read().unwrap().iter() {
        handler(event);
    }
}
