use crate::modules::pools::increase::{increase_pool, MODEL_POOL_TABLE};
pub fn apply() { increase_pool("model pool 23", MODEL_POOL_TABLE + 276, 30); }
