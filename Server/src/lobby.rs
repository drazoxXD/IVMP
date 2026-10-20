//! Connected clients. Clients say hello and get a client id, which is also their virtual address
//! (10.0.0.<id>). The first client to join hosts the engine session; when the host leaves, the
//! oldest remaining client takes over.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use NetProtocol::{ClientId, RejectReason, TIMEOUT_SECS};

pub const MAX_CLIENTS: usize = 32;

pub struct Client {
    pub addr: SocketAddr,
    pub nickname: String,
    pub last_seen: Instant,
}

#[derive(Default)]
pub struct Lobby {
    /// Ordered by id, which is also join order, so the first entry is the oldest client.
    pub clients: BTreeMap<ClientId, Client>,
    pub host: Option<ClientId>,
}

impl Lobby {
    pub fn id_of(&self, addr: SocketAddr) -> Option<ClientId> {
        self.clients.iter().find(|(_, c)| c.addr == addr).map(|(id, _)| *id)
    }

    pub fn touch(&mut self, id: ClientId) {
        if let Some(client) = self.clients.get_mut(&id) {
            client.last_seen = Instant::now();
        }
    }

    /// Adds a client with a validated nickname; returns its id.
    pub fn join(&mut self, addr: SocketAddr, nickname: &str) -> Result<ClientId, RejectReason> {
        if nickname.is_empty() || nickname.len() > 32 {
            return Err(RejectReason::BadNickname);
        }
        let id = (1..=MAX_CLIENTS as ClientId)
            .find(|id| !self.clients.contains_key(id))
            .ok_or(RejectReason::ServerFull)?;
        self.clients.insert(id, Client { addr, nickname: nickname.to_string(), last_seen: Instant::now() });
        self.host.get_or_insert(id);
        Ok(id)
    }

    /// Removes a client; returns it and the new host if the host changed.
    pub fn leave(&mut self, id: ClientId) -> Option<(Client, Option<ClientId>)> {
        let client = self.clients.remove(&id)?;
        let mut new_host = None;
        if self.host == Some(id) {
            self.host = self.clients.keys().next().copied();
            new_host = self.host;
        }
        Some((client, new_host))
    }

    pub fn timed_out(&self) -> Vec<ClientId> {
        let timeout = Duration::from_secs(TIMEOUT_SECS);
        self.clients.iter().filter(|(_, c)| c.last_seen.elapsed() > timeout).map(|(id, _)| *id).collect()
    }
}
