//! A value that can cross from one resource's Lua state to another (events, exports) and, as JSON,
//! over the network: nil, boolean, number, string, table. Functions and userdata can't.

use mlua::{Lua, Value};

#[derive(Debug, Clone, PartialEq)]
pub enum Variant {
    Nil,
    Bool(bool),
    Int(i64),
    Num(f64),
    Str(String),
    /// Key / value pairs, in the order Lua gave them.
    Table(Vec<(Variant, Variant)>),
}

const MAX_DEPTH: usize = 16;

impl Variant {
    pub fn from_lua(value: &Value) -> Result<Variant, String> {
        Self::from_lua_depth(value, 0)
    }

    fn from_lua_depth(value: &Value, depth: usize) -> Result<Variant, String> {
        if depth > MAX_DEPTH {
            return Err("the table is nested too deep (or refers to itself)".into());
        }
        Ok(match value {
            Value::Nil => Variant::Nil,
            Value::Boolean(b) => Variant::Bool(*b),
            Value::Integer(i) => Variant::Int(*i as i64),
            Value::Number(n) => Variant::Num(*n),
            Value::String(s) => Variant::Str(s.to_string_lossy().to_string()),
            Value::Table(table) => {
                let mut pairs = Vec::new();
                for pair in table.pairs::<Value, Value>() {
                    let (k, v) = pair.map_err(|e| e.to_string())?;
                    pairs.push((Self::from_lua_depth(&k, depth + 1)?, Self::from_lua_depth(&v, depth + 1)?));
                }
                Variant::Table(pairs)
            }
            other => return Err(format!("a {} can't be passed between scripts", other.type_name())),
        })
    }

    pub fn to_lua(&self, lua: &Lua) -> mlua::Result<Value> {
        Ok(match self {
            Variant::Nil => Value::Nil,
            Variant::Bool(b) => Value::Boolean(*b),
            Variant::Int(i) => Value::Integer((*i).into()),
            Variant::Num(n) => Value::Number(*n),
            Variant::Str(s) => Value::String(lua.create_string(s)?),
            Variant::Table(pairs) => {
                let table = lua.create_table()?;
                for (k, v) in pairs {
                    table.set(k.to_lua(lua)?, v.to_lua(lua)?)?;
                }
                Value::Table(table)
            }
        })
    }

    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::Value as J;
        match self {
            Variant::Nil => J::Null,
            Variant::Bool(b) => J::Bool(*b),
            Variant::Int(i) => J::from(*i),
            Variant::Num(n) => serde_json::Number::from_f64(*n).map_or(J::Null, J::Number),
            Variant::Str(s) => J::String(s.clone()),
            Variant::Table(pairs) => {
                // A table with keys 1..n in order is a JSON array, anything else an object.
                let is_array = !pairs.is_empty()
                    && pairs.iter().enumerate().all(|(i, (k, _))| matches!(k, Variant::Int(n) if *n == i as i64 + 1));
                if is_array {
                    J::Array(pairs.iter().map(|(_, v)| v.to_json()).collect())
                } else {
                    let mut map = serde_json::Map::new();
                    for (k, v) in pairs {
                        let key = match k {
                            Variant::Str(s) => s.clone(),
                            Variant::Int(i) => i.to_string(),
                            Variant::Num(n) => n.to_string(),
                            Variant::Bool(b) => b.to_string(),
                            _ => continue,
                        };
                        map.insert(key, v.to_json());
                    }
                    J::Object(map)
                }
            }
        }
    }

    pub fn from_json(value: &serde_json::Value) -> Variant {
        use serde_json::Value as J;
        match value {
            J::Null => Variant::Nil,
            J::Bool(b) => Variant::Bool(*b),
            J::Number(n) => n.as_i64().map_or_else(|| Variant::Num(n.as_f64().unwrap_or(0.0)), Variant::Int),
            J::String(s) => Variant::Str(s.clone()),
            J::Array(items) => {
                Variant::Table(items.iter().enumerate().map(|(i, v)| (Variant::Int(i as i64 + 1), Variant::from_json(v))).collect())
            }
            J::Object(map) => Variant::Table(map.iter().map(|(k, v)| (Variant::Str(k.clone()), Variant::from_json(v))).collect()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_lua_and_json() {
        let lua = Lua::new();
        let value: Value = lua.load("return { 1, 2, name = 'x', nested = { true, 2.5 } }").eval().unwrap();
        let variant = Variant::from_lua(&value).unwrap();
        let json = variant.to_json();
        // Mixed keys: an object.
        assert_eq!(json["name"], "x");
        let back = Variant::from_json(&json);
        let lua_again = back.to_lua(&lua).unwrap();
        let table = lua_again.as_table().unwrap();
        assert_eq!(table.get::<String>("name").unwrap(), "x");
    }

    #[test]
    fn arrays_stay_arrays() {
        let lua = Lua::new();
        let value: Value = lua.load("return { 10, 20, 30 }").eval().unwrap();
        let json = Variant::from_lua(&value).unwrap().to_json();
        assert_eq!(json, serde_json::json!([10, 20, 30]));
    }

    #[test]
    fn functions_are_refused() {
        let lua = Lua::new();
        let value: Value = lua.load("return function() end").eval().unwrap();
        assert!(Variant::from_lua(&value).is_err());
    }

    #[test]
    fn cyclic_tables_are_refused() {
        let lua = Lua::new();
        let value: Value = lua.load("local t = {} t.self = t return t").eval().unwrap();
        assert!(Variant::from_lua(&value).is_err());
    }
}
