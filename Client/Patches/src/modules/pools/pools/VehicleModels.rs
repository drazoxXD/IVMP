use crate::modules::pools::increase::{increase_pool, MODEL_POOL_TABLE};
pub fn apply() { increase_pool("vehicle models", MODEL_POOL_TABLE + 24, 1800); }
