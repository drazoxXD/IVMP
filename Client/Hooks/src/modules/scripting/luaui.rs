//! UI for client scripts (docs/scripting-plan.md, stage 5): a resource's `ui_page` is shown by the
//! web layer (`ivmp://<resource>/<page>`, files from the cache), and scripts talk to it:
//!
//! * `SendNUIMessage(table)`: the page gets it as a `message` event (`event.data` is the table).
//! * `RegisterNUICallback(name, function(data, cb) ... end)`: the page calls it with
//!   `ivmpNui.post(name, data)` (`<script src="ivmp://ui/nui.js">`); `cb(result)` answers the promise.
//! * `SetNuiFocus(true)`: the page gets the keyboard and mouse (Escape takes them back).

use ivmp_scripting::mlua::{self, Function, Lua, Value};
use ivmp_scripting::Variant;

/// `RegisterNUICallback` is Lua on top of events: a page's call arrives as the event `__nui:<name>`
/// for that resource only, and `cb` answers through `__NuiReply`.
const PRELUDE: &str = r#"
function RegisterNUICallback(name, handler)
    AddEventHandler('__nui:' .. name, function(data, id)
        local replied = false
        local function cb(result)
            if replied then return end
            replied = true
            __NuiReply(id, result)
        end
        local ok, err = pcall(handler, data, cb)
        if not ok then
            print('NUI callback ' .. name .. ': ' .. tostring(err))
            cb({ error = tostring(err) })
        end
    end)
end
"#;

fn resource_name(lua: &Lua) -> mlua::Result<String> {
    lua.globals().get::<Function>("GetCurrentResourceName")?.call::<String>(())
}

fn json_of(value: &Value) -> mlua::Result<serde_json::Value> {
    Variant::from_lua(value).map(|v| v.to_json()).map_err(mlua::Error::runtime)
}

/// Adds the UI functions to a script's Lua state.
pub fn install(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();
    globals.set(
        "SendNUIMessage",
        lua.create_function(|lua, message: Value| {
            let resource = resource_name(lua)?;
            let data = json_of(&message)?;
            crate::web::emit("ui.message", &serde_json::json!({ "resource": resource, "data": data }).to_string());
            Ok(())
        })?,
    )?;
    globals.set(
        "SetNuiFocus",
        lua.create_function(|lua, (focus, _cursor): (bool, Option<bool>)| {
            crate::web::set_focus(focus);
            // The page moves the keyboard focus into this resource's frame (or out of it).
            let resource = resource_name(lua)?;
            crate::web::emit("ui.focus", &serde_json::json!({ "resource": resource, "focus": focus }).to_string());
            Ok(())
        })?,
    )?;
    globals.set(
        "__NuiReply",
        lua.create_function(|lua, (id, result): (i64, Value)| {
            let resource = resource_name(lua)?;
            let result = json_of(&result)?;
            crate::web::emit("ui.reply", &serde_json::json!({ "resource": resource, "id": id, "result": result }).to_string());
            Ok(())
        })?,
    )?;
    lua.load(PRELUDE).set_name("=nui").exec()
}
