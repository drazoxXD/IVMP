use crate::modules::pools::increase::{increase_pool, MODEL_POOL_TABLE};
pub fn apply() { increase_pool("model pool 19", MODEL_POOL_TABLE + 228, 20); }
