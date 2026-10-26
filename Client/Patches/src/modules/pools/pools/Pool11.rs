use crate::modules::pools::increase::{increase_pool, MODEL_POOL_TABLE};
pub fn apply() { increase_pool("model pool 11", MODEL_POOL_TABLE + 132, 10900); }
