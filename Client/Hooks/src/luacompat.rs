//! FiveM names for client scripts, so resources written for FiveM (the chat) run as they are:
//!
//! * `PlayerId()`, and `GetPlayerName(id)`: our nickname for our own player, the game's name for others.
//! * `IsControlPressed` / `IsControlJustPressed` / `IsControlJustReleased` (and the `Disabled`
//!   ones): GTA V's control ids that resources use for keys IV doesn't have are read from the
//!   keyboard (245, INPUT_MP_TEXT_CHAT_ALL = T), anything else goes to IV's IS_CONTROL_PRESSED.
//!   A key counts only while the game window is in front and the console or a page doesn't have
//!   the keyboard.
//! * `IsScreenFadedOut()`, `IsPauseMenuActive()`: IV's natives. `SetTextChatEnabled` does nothing
//!   (IV has no text chat of its own to turn off).

use ivmp_scripting::mlua::{self, Lua};
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

/// GTA V control ids and the key each one is in IVMP.
const KEY_CONTROLS: &[(i64, i32)] = &[
    (245, 0x54), // INPUT_MP_TEXT_CHAT_ALL: T
    (246, 0x59), // INPUT_MP_TEXT_CHAT_TEAM: Y
];

/// The key is down for the game: held, the game window in front, nothing else has the keyboard.
fn key_down(vk: i32) -> bool {
    let ours = unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut pid));
        pid == std::process::id()
    };
    ours && !crate::ui::capturing() && unsafe { GetAsyncKeyState(vk) } as u16 & 0x8000 != 0
}

const PRELUDE: &str = r#"
local key_of, key_down = __KeyOfControl, __KeyDown
__KeyOfControl, __KeyDown = nil, nil

function PlayerId() return Natives.GET_PLAYER_ID() end

local local_name = __LocalName
__LocalName = nil
function GetPlayerName(id)
    if id == nil or id == PlayerId() then return local_name() end
    local ok, name = pcall(Natives.GET_PLAYER_NAME, id)
    return ok and name or ('player ' .. tostring(id))
end

-- Pressed state per control, for the Just* functions (one answer per frame and control).
local previous, current, frame = {}, {}, -1
local function pressed(pad, control)
    local now = GetGameTimer()
    if now ~= frame then previous, current, frame = current, {}, now end
    local value = current[control]
    if value == nil then
        local vk = key_of(control)
        if vk then
            value = key_down(vk)
        else
            local ok, down = pcall(Natives.IS_CONTROL_PRESSED, pad or 0, control)
            value = ok and down or false
        end
        current[control] = value
    end
    return value
end

function IsControlPressed(pad, control) return pressed(pad, control) end
function IsControlJustPressed(pad, control) return pressed(pad, control) and not previous[control] end
function IsControlJustReleased(pad, control) return not pressed(pad, control) and previous[control] == true end
IsDisabledControlPressed, IsDisabledControlJustPressed, IsDisabledControlJustReleased =
    IsControlPressed, IsControlJustPressed, IsControlJustReleased

function IsScreenFadedOut() local ok, v = pcall(Natives.IS_SCREEN_FADED_OUT) return ok and v or false end
function IsPauseMenuActive() local ok, v = pcall(Natives.IS_PAUSE_MENU_ACTIVE) return ok and v or false end
function SetTextChatEnabled() end
"#;

/// Adds the FiveM names to a client script's Lua state (after the natives).
pub fn install(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();
    globals.set(
        "__KeyOfControl",
        lua.create_function(|_, control: i64| Ok(KEY_CONTROLS.iter().find(|(c, _)| *c == control).map(|(_, vk)| *vk)))?,
    )?;
    globals.set("__KeyDown", lua.create_function(|_, vk: i32| Ok(key_down(vk)))?)?;
    globals.set("__LocalName", lua.create_function(|_, ()| Ok(ivmp_networking::nickname().to_string()))?)?;
    lua.load(PRELUDE).set_name("=fivem").exec()
}
