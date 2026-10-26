//! Forwards the engine's own UDP packets (rage netSocket) between clients by virtual address.
//! The engine never sees real addresses, so it treats every player as a LAN peer.

use std::net::Ipv4Addr;

use NetProtocol::{BROADCAST, ClientId, client_id_of};

/// Where a tunnelled packet goes.
pub enum Target {
    Everyone,
    Client(ClientId),
    Nobody,
}

pub fn target(to: Ipv4Addr) -> Target {
    if to == BROADCAST {
        Target::Everyone
    } else {
        client_id_of(to).map_or(Target::Nobody, Target::Client)
    }
}
