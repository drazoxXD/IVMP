//! GTA IV script natives: the registration table, calling a native by hash ([`natives`]) and
//! the native / script-name logger ([`nativelog`]).

pub use ivmp_core::{log, memory};

mod modules;
pub use modules::{nativelog, natives};
