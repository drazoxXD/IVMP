use ivmp_core::{log, memory};

pub const MODEL_POOL_TABLE: usize = 0x0103_AD64;

/// Applies the common capacity patch for one model pool.
pub fn increase_pool(name: &'static str, address: usize, original: u32) {
    let at = memory::from_ghidra(address);
    let mut current = [0u8; 4];
    if !memory::read(at, &mut current) || u32::from_le_bytes(current) != original {
        log!(
            "pools: {name} at {address:#x} holds {}, expected {original}; left alone",
            u32::from_le_bytes(current)
        );
        return;
    }

    let increased = if original >= 10_000 { original * 2 } else { original * 3 };
    match unsafe { memory::patch(at, &increased.to_le_bytes()) } {
        Ok(()) => log!("pools: {name} raised from {original} to {increased}"),
        Err(e) => log!("pools: {name} could not be raised: {e}"),
    }
}

pub fn raise_model_pools() {
    super::pools::ped_models::apply();
    super::pools::vehicle_models::apply();
    super::pools::weapon_models::apply();
    super::pools::object_models::apply();
    super::pools::pool05::apply();
    super::pools::pool06::apply();
    super::pools::pool07::apply();
    super::pools::pool08::apply();
    super::pools::pool09::apply();
    super::pools::pool10::apply();
    super::pools::pool11::apply();
    super::pools::pool12::apply();
    super::pools::pool13::apply();
    super::pools::pool14::apply();
    super::pools::pool15::apply();
    super::pools::pool16::apply();
    super::pools::pool17::apply();
    super::pools::pool18::apply();
    super::pools::pool19::apply();
    super::pools::pool20::apply();
    super::pools::pool21::apply();
    super::pools::pool22::apply();
    super::pools::pool23::apply();
}
