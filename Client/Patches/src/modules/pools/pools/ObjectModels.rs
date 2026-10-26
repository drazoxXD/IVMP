use crate::modules::pools::increase::{increase_pool, MODEL_POOL_TABLE};
pub fn apply() { increase_pool("object models", MODEL_POOL_TABLE + 48, 205); }
