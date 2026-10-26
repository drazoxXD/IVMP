use crate::modules::pools::increase::{increase_pool, MODEL_POOL_TABLE};
pub fn apply() { increase_pool("weapon models", MODEL_POOL_TABLE + 36, 100); }
