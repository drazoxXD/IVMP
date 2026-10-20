//! Network object id blocks handed out by the server (phase 2, docs/onesync-plan.md).
//!
//! Object ids are 14 bits. Every client gets its own block, so two engines never create
//! entities with the same id. Ids below the first block are left to the engine: the host's
//! own allocator hands out a few (its player, early peds) before the client takes its block.

/// Ids per client; 32 blocks after the reserved range stay below 0x4000.
pub const BLOCK: u16 = 490;

/// The ids client `id` (1..=32) may use: `first..first + count`.
pub fn block(client: u8) -> (u16, u16) {
    (client as u16 * BLOCK, BLOCK)
}

/// The client whose block holds `object_id`, if any.
pub fn client_of(object_id: u16) -> Option<u8> {
    let client = object_id / BLOCK;
    (1..=32).contains(&client).then_some(client as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_fit_in_14_bits_and_map_back() {
        let (first, count) = block(32);
        assert!(first + count <= 0x4000);
        assert_eq!(client_of(block(5).0 + 10), Some(5));
        assert_eq!(client_of(10), None);
    }
}
