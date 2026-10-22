//! In-game UI drawn with dear imgui on the game's render thread: the F1 console and the stats
//! overlay (FPS, ping, network, client performance). `render.rs` calls `frame` before every
//! Present; `input` feeds it the window messages the game would otherwise get.

use std::collections::{BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use imgui::{Condition, HistoryDirection, InputTextCallback, InputTextCallbackHandler, TextCallbackData, Context, ItemHoveredFlags, Key, MouseButton, StyleColor, WindowFlags};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Direct3D9::*;
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::imgui_dx9::Renderer;
use crate::log;

/// The console is open: the game gets no keyboard or mouse input.
static CONSOLE_OPEN: AtomicBool = AtomicBool::new(false);
/// Last display size, for centring the raw cursor.
static DISPLAY_SIZE: (AtomicU32, AtomicU32) = (AtomicU32::new(0), AtomicU32::new(0));

/// The game gets no keyboard or mouse input: the console or the web page is up.
pub fn capturing() -> bool {
    CONSOLE_OPEN.load(Ordering::Relaxed) || crate::web::visible()
}

fn console_open() -> bool {
    CONSOLE_OPEN.load(Ordering::Relaxed)
}

/// The raw cursor starts at the middle of the screen.
pub fn reset_raw_cursor() {
    RAW_SEEN.store(false, Ordering::Relaxed);
    *RAW_POSITION.lock().unwrap() = [DISPLAY_SIZE.0.load(Ordering::Relaxed) as f32 / 2.0, DISPLAY_SIZE.1.load(Ordering::Relaxed) as f32 / 2.0];
}

/// The console shuts when the game can't show it (loading screens).
pub fn close_console() {
    CONSOLE_OPEN.store(false, Ordering::Relaxed);
    crate::web::close();
}

pub fn toggle_console() {
    let open = !CONSOLE_OPEN.load(Ordering::Relaxed);
    if open {
        reset_raw_cursor();
    }
    CONSOLE_OPEN.store(open, Ordering::Relaxed);
}

/// Overlay panels, toggled from the console.
static SHOW_FPS: AtomicBool = AtomicBool::new(false);
static SHOW_PING: AtomicBool = AtomicBool::new(false);
static SHOW_NET: AtomicBool = AtomicBool::new(false);
static SHOW_PERF: AtomicBool = AtomicBool::new(false);

/// Microseconds the game's own update took last frame (set by `game_process` in boot.rs).
pub static GAME_TICK_US: AtomicU32 = AtomicU32::new(0);

/// The mouse as DirectInput reports it: the game's exclusive mouse sends no window messages, so
/// while the console is open `focus.rs` hands the raw movement and buttons over.
static RAW_SEEN: AtomicBool = AtomicBool::new(false);
static RAW_POSITION: Mutex<[f32; 2]> = Mutex::new([0.0, 0.0]);

pub fn raw_mouse(dx: i32, dy: i32, wheel: i32) {
    RAW_SEEN.store(true, Ordering::Relaxed);
    let mut position = RAW_POSITION.lock().unwrap();
    let (width, height) = (DISPLAY_SIZE.0.load(Ordering::Relaxed) as f32, DISPLAY_SIZE.1.load(Ordering::Relaxed) as f32);
    position[0] = (position[0] + dx as f32).clamp(0.0, (width - 1.0).max(0.0));
    position[1] = (position[1] + dy as f32).clamp(0.0, (height - 1.0).max(0.0));
    let at = *position;
    drop(position);
    if !console_open() && crate::web::visible() {
        if dx != 0 || dy != 0 {
            crate::web::mouse_move(at[0], at[1]);
        }
        if wheel != 0 {
            crate::web::mouse_wheel(wheel);
        }
    } else if wheel != 0 {
        push(Event::Wheel(wheel as f32 / 120.0));
    }
}

pub fn raw_button(index: usize, down: bool) {
    static STATE: [AtomicBool; 3] = [const { AtomicBool::new(false) }; 3];
    if index < 3 && STATE[index].swap(down, Ordering::Relaxed) != down {
        if !console_open() && crate::web::visible() {
            crate::web::mouse_button(index, down);
            return;
        }
        let button = [MouseButton::Left, MouseButton::Right, MouseButton::Middle][index];
        push(Event::Button(button, down));
    }
}

enum Event {
    Char(char),
    Key(Key, bool),
    Button(MouseButton, bool),
    Wheel(f32),
}

static EVENTS: Mutex<Vec<Event>> = Mutex::new(Vec::new());

fn push(event: Event) {
    let mut events = EVENTS.lock().unwrap();
    if events.len() < 256 {
        events.push(event);
    }
}

fn map_key(vk: u16) -> Option<Key> {
    Some(match VIRTUAL_KEY(vk) {
        VK_TAB => Key::Tab,
        VK_LEFT => Key::LeftArrow,
        VK_RIGHT => Key::RightArrow,
        VK_UP => Key::UpArrow,
        VK_DOWN => Key::DownArrow,
        VK_PRIOR => Key::PageUp,
        VK_NEXT => Key::PageDown,
        VK_HOME => Key::Home,
        VK_END => Key::End,
        VK_INSERT => Key::Insert,
        VK_DELETE => Key::Delete,
        VK_BACK => Key::Backspace,
        VK_RETURN => Key::Enter,
        VK_ESCAPE => Key::Escape,
        VK_SPACE => Key::Space,
        VIRTUAL_KEY(0x41) => Key::A,
        VIRTUAL_KEY(0x43) => Key::C,
        VIRTUAL_KEY(0x56) => Key::V,
        VIRTUAL_KEY(0x58) => Key::X,
        VIRTUAL_KEY(0x59) => Key::Y,
        VIRTUAL_KEY(0x5A) => Key::Z,
        _ => return None,
    })
}

/// A window message for the game's window. Returns true when the message is used up (F1, or any
/// keyboard / mouse message while the console is open) and must not reach the game.
pub fn window_message(message: u32, wparam: usize, lparam: isize) -> bool {
    match message {
        WM_KEYDOWN | WM_SYSKEYDOWN if wparam as u16 == VK_F1.0 => {
            // Only the first press, not the key repeat.
            if lparam & (1 << 30) == 0 && crate::hang::game_running() {
                toggle_console();
            }
            return true;
        }
        WM_KEYDOWN | WM_SYSKEYDOWN if wparam as u16 == VK_F2.0 => {
            if lparam & (1 << 30) == 0 && crate::hang::game_running() {
                crate::web::toggle();
            }
            return true;
        }
        _ => {}
    }
    if !capturing() {
        return false;
    }
    if !console_open() {
        // The web page has the keyboard and mouse.
        return web_message(message, wparam, lparam);
    }
    match message {
        WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP => {
            if let Some(key) = map_key(wparam as u16) {
                push(Event::Key(key, matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN)));
            }
            true
        }
        WM_CHAR => {
            if let Some(c) = char::from_u32(wparam as u32).filter(|c| !c.is_control()) {
                push(Event::Char(c));
            }
            true
        }
        WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_MBUTTONDOWN | WM_MBUTTONUP if RAW_SEEN.load(Ordering::Relaxed) => true,
        WM_LBUTTONDOWN | WM_LBUTTONUP => {
            push(Event::Button(MouseButton::Left, message == WM_LBUTTONDOWN));
            true
        }
        WM_RBUTTONDOWN | WM_RBUTTONUP => {
            push(Event::Button(MouseButton::Right, message == WM_RBUTTONDOWN));
            true
        }
        WM_MBUTTONDOWN | WM_MBUTTONUP => {
            push(Event::Button(MouseButton::Middle, message == WM_MBUTTONDOWN));
            true
        }
        WM_MOUSEWHEEL => {
            push(Event::Wheel(((wparam >> 16) as i16) as f32 / 120.0));
            true
        }
        WM_MOUSEMOVE | WM_SETCURSOR | WM_INPUT => false,
        _ => false,
    }
}

/// FiveM's debug tools that IVMP doesn't have yet: (group, console name, what it does). They show
/// in the console's tool list as disabled entries with a "WIP" tooltip.
const WIP_TOOLS: &[(&str, &str, &str)] = &[
    ("Network", "netgraph", "Real-time graph of ping, packets, bytes and routing"),
    ("Network", "netEventLog", "Network traffic with direction, name and size"),
    ("Network", "net_statsFile", "Write network metrics to a CSV file"),
    ("Network", "netobjviewer", "List the network objects and sync nodes being synchronised"),
    ("Network", "net_printOwner", "Print the owner of a network object id"),
    ("Streaming", "strdbg", "What the streamer is loading right now"),
    ("Streaming", "strlist", "Streamer entries and their status"),
    ("Streaming", "strmem", "Streaming memory use per asset"),
    ("Game", "resmon", "CPU and memory use per script/resource"),
    ("Game", "modelviewer", "Load and view models and textures"),
    ("Game", "poolInspector", "Inspect the game's entity pools"),
    ("Game", "inputViewer", "Show the keys and axes the game receives"),
    ("Game", "handlingEditor", "Edit vehicle handling live"),
    ("Game", "timecycleEditor", "Edit the time cycle live"),
    ("Console", "con_miniconChannels", "Show chosen log channels on screen without opening the console"),
    ("Console", "cmdlist", "List every command and variable"),
    ("Console", "developer", "Extra developer logging"),
    ("Scripting", "script_debug", "Script engine debugger: native calls, running scripts (needs the native hook)"),
];

/// FiveM's look (conhost-v2): dark translucent window, blue accent, square corners.
fn set_style(context: &mut Context) {
    let accent = [81.0 / 255.0, 179.0 / 255.0, 236.0 / 255.0, 1.0];
    let style = context.style_mut();
    style.window_rounding = 0.0;
    style.window_border_size = 0.0;
    style.window_padding = [4.0, 3.0];
    style[StyleColor::WindowBg] = [22.0 / 255.0, 24.0 / 255.0, 28.0 / 255.0, 200.0 / 255.0];
    style[StyleColor::PopupBg] = [22.0 / 255.0, 24.0 / 255.0, 28.0 / 255.0, 240.0 / 255.0];
    style[StyleColor::Border] = accent;
    style[StyleColor::TitleBg] = accent;
    style[StyleColor::TitleBgActive] = accent;
    style[StyleColor::FrameBg] = [50.0 / 255.0, 50.0 / 255.0, 50.0 / 255.0, 0.6];
    style[StyleColor::FrameBgHovered] = [80.0 / 255.0, 80.0 / 255.0, 80.0 / 255.0, 0.6];
    style[StyleColor::FrameBgActive] = [80.0 / 255.0, 80.0 / 255.0, 80.0 / 255.0, 0.8];
    style[StyleColor::TextSelectedBg] = [accent[0], accent[1], accent[2], 180.0 / 255.0];
}

/// Rounded tag color per channel: hue from the name, saturation 0.8, lightness 0.4.
fn channel_color(channel: &str) -> [f32; 4] {
    let hash = channel.bytes().fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    let hue = (hash % 360) as f32 / 360.0;
    let (s, l) = (0.8f32, 0.4f32);
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((hue * 6.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (hue * 6.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [r + m, g + m, b + m, 1.0]
}

/// The menu bar across the top (FiveM's dev GUI). Returns its height.
fn menu_bar(ui: &imgui::Ui) -> f32 {
    ui.main_menu_bar(|| {
        ui.menu("Overlays", || {
            flag_menu(ui, "Draw FPS", &SHOW_FPS);
            flag_menu(ui, "Ping", &SHOW_PING);
            flag_menu(ui, "Network info", &SHOW_NET);
            wip_menu(ui, "NetGraph", "Real-time graph of ping, packets, bytes and routing");
            ui.menu("Performance", || {
                flag_menu(ui, "Draw Performance", &SHOW_PERF);
            });
        });
        ui.menu("Launch", || {
            ui.menu("MP", || {
                wip_menu(ui, "Localhost", "Connect to a server on this PC");
                wip_menu(ui, "Connect to host", "Connect to a typed address");
                wip_menu(ui, "Disconnect", "Leave the server");
            });
            if ui.menu_item("Quit") {
                crate::render::quit_game();
            }
        });
        ui.menu("Tools", || {
            ui.menu("Performance", || {
                ui.menu("Profiler", || {
                    wip_menu(ui, "Start Recording - 5 frames", "Record a profiler capture");
                    wip_menu(ui, "View Last Recording", "Open the last capture");
                });
                wip_menu(ui, "Resource Monitor", "CPU and memory use per script/resource");
                wip_menu(ui, "Streaming Memory Viewer", "Streaming memory use per asset");
            });
            wip_menu(ui, "Windowed Console", "The console in its own window");
            for g in ["Streaming", "Network", "Game", "Console"] {
                ui.menu(g, || {
                    for (group, name, about) in WIP_TOOLS {
                        if *group == g {
                            wip_menu(ui, name, about);
                        }
                    }
                });
            }
            ui.menu("NUI", || {
                if crate::web::devtools_allowed() {
                    if ui.menu_item("Open DevTools") {
                        crate::web::open_devtools();
                    }
                } else {
                    wip_menu(ui, "Open DevTools", "Developer builds only (or a `developer` file next to the client)");
                }
            });
            ui.menu("Scripting", || {
                wip_menu(ui, "Script debugger", "Native calls and running scripts (needs the native hook)");
            });
        });
        ui.menu("Game", || {
            wip_menu(ui, "SFX Volume", "Sound effects volume");
            wip_menu(ui, "Mute", "Mute the game");
            wip_menu(ui, "Unmute", "Unmute the game");
        });
    });
    ui.frame_height()
}

/// A menu checkbox bound to an overlay flag.
fn flag_menu(ui: &imgui::Ui, label: &str, flag: &AtomicBool) {
    let mut value = flag.load(Ordering::Relaxed);
    if ui.menu_item_config(label).build_with_ref(&mut value) {
        flag.store(value, Ordering::Relaxed);
        log!("ui: overlay {label} {}", if value { "on" } else { "off" });
    }
}

/// A disabled menu entry whose tooltip says it's not implemented.
fn wip_menu(ui: &imgui::Ui, label: &str, about: &str) {
    ui.menu_item_config(label).enabled(false).build();
    if ui.is_item_hovered_with_flags(ItemHoveredFlags::ALLOW_WHEN_DISABLED) {
        ui.tooltip_text(format!("WIP: not implemented yet.\n{about}"));
    }
}

/// Keyboard and mouse messages while the web page is up: forwarded to it, hidden from the game.
fn web_message(message: u32, wparam: usize, lparam: isize) -> bool {
    let modifiers = unsafe {
        (if GetKeyState(VK_SHIFT.0 as i32) < 0 { 2 } else { 0 })
            | (if GetKeyState(VK_CONTROL.0 as i32) < 0 { 4 } else { 0 })
            | (if GetKeyState(VK_MENU.0 as i32) < 0 { 8 } else { 0 })
    };
    match message {
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            if wparam as u16 == VK_ESCAPE.0 {
                crate::web::close();
            } else {
                crate::web::key(0, wparam as i32, lparam, modifiers);
            }
            true
        }
        WM_KEYUP | WM_SYSKEYUP => {
            crate::web::key(1, wparam as i32, lparam, modifiers);
            true
        }
        WM_CHAR => {
            crate::web::key(2, wparam as i32, lparam, modifiers);
            true
        }
        // Mouse comes from the raw DirectInput state; the window messages are not used.
        WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_MBUTTONDOWN | WM_MBUTTONUP | WM_MOUSEWHEEL => true,
        _ => false,
    }
}

struct Line {
    text: String,
    color: [f32; 4],
    /// The log channel (the word before the first colon of a log line), empty for console lines.
    channel: String,
}

/// "net: connecting" -> "net".
fn channel_of(text: &str) -> String {
    match text.split_once(':') {
        Some((channel, _)) if !channel.is_empty() && channel.len() <= 16 && !channel.contains(' ') => channel.to_lowercase(),
        _ => String::new(),
    }
}

/// A stable muted color per channel, red for failures.
fn log_color(channel: &str, text: &str) -> [f32; 4] {
    let lower = text.to_lowercase();
    if lower.contains("failed") || lower.contains("error") || lower.contains("could not") || lower.contains("crash") {
        return ERROR_LOG_COLOR;
    }
    const PALETTE: [[f32; 4]; 7] = [
        [0.55, 0.78, 1.0, 1.0],
        [0.6, 0.9, 0.6, 1.0],
        [0.95, 0.8, 0.5, 1.0],
        [0.85, 0.65, 0.95, 1.0],
        [0.5, 0.9, 0.9, 1.0],
        [0.95, 0.65, 0.7, 1.0],
        [0.8, 0.8, 0.55, 1.0],
    ];
    if channel.is_empty() {
        return LOG_COLOR;
    }
    let hash = channel.bytes().fold(7usize, |h, b| h.wrapping_mul(31).wrapping_add(b as usize));
    PALETTE[hash % PALETTE.len()]
}

/// Lines the Tab completion wants in the log (the callback can't reach the console).
static COMPLETION_OUTPUT: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Every command the console knows, for Tab completion and suggestions.
fn command_names() -> Vec<&'static str> {
    let mut names = vec!["help", "clear", "echo", "overlay", "status", "devtools", "quit"];
    names.extend(WIP_TOOLS.iter().map(|(_, name, _)| *name));
    names.sort_unstable();
    names
}

/// History (Up / Down) and Tab completion for the command line.
struct CommandLine<'a> {
    history: &'a [String],
    position: &'a mut Option<usize>,
}

impl InputTextCallbackHandler for CommandLine<'_> {
    fn on_history(&mut self, direction: HistoryDirection, mut data: TextCallbackData) {
        if self.history.is_empty() {
            return;
        }
        let last = self.history.len() - 1;
        let next = match (direction, *self.position) {
            (HistoryDirection::Up, None) => Some(last),
            (HistoryDirection::Up, Some(p)) => Some(p.saturating_sub(1)),
            (HistoryDirection::Down, Some(p)) if p < last => Some(p + 1),
            (HistoryDirection::Down, _) => None,
        };
        *self.position = next;
        data.clear();
        if let Some(p) = next {
            data.push_str(&self.history[p]);
        }
    }

    fn on_completion(&mut self, mut data: TextCallbackData) {
        let typed = data.str().to_string();
        if typed.contains(' ') {
            return;
        }
        let matches: Vec<&str> = command_names().into_iter().filter(|n| n.to_lowercase().starts_with(&typed.to_lowercase())).collect();
        let Some(first) = matches.first() else {
            COMPLETION_OUTPUT.lock().unwrap().push(format!("No match for {typed}"));
            return;
        };
        if matches.len() > 1 {
            let mut output = COMPLETION_OUTPUT.lock().unwrap();
            output.push("Possible matches:".to_string());
            output.extend(matches.iter().map(|m| format!("- {m}")));
        }
        // Complete as far as all matches agree.
        let mut common = first.len();
        for m in &matches {
            common = common.min(first.bytes().zip(m.bytes()).take_while(|(a, b)| a.eq_ignore_ascii_case(b)).count());
        }
        let completed = if matches.len() == 1 { format!("{first} ") } else { first[..common].to_string() };
        if completed.len() >= typed.len() {
            data.clear();
            data.push_str(&completed);
        }
    }
}

const LOG_COLOR: [f32; 4] = [0.62, 0.64, 0.68, 1.0];
const INPUT_COLOR: [f32; 4] = [0.55, 0.8, 1.0, 1.0];
const OUTPUT_COLOR: [f32; 4] = [0.9, 0.9, 0.9, 1.0];
const ERROR_COLOR: [f32; 4] = [1.0, 0.45, 0.4, 1.0];
/// Log lines that look like failures.
const ERROR_LOG_COLOR: [f32; 4] = [1.0, 0.5, 0.45, 1.0];

struct Ui {
    context: Context,
    renderer: Renderer,
    lines: VecDeque<Line>,
    log_seen: u64,
    input: String,
    history: Vec<String>,
    history_position: Option<usize>,
    filter: String,
    auto_scroll: bool,
    /// The log view is pinned to the bottom (it was there, and no new line moved it).
    stick_to_bottom: bool,
    channels: BTreeSet<String>,
    focus_input: bool,
    scroll_to_end: bool,
    last_frame: Instant,
    frame_ms: VecDeque<f32>,
}

/// Only the render thread touches the UI; the mutex is for the statics' sake.
struct Shared(Option<Ui>);
unsafe impl Send for Shared {}

static UI: Mutex<Shared> = Mutex::new(Shared(None));

const MAX_LINES: usize = 1000;

impl Ui {
    fn new(device: &IDirect3DDevice9) -> Option<Ui> {
        let mut context = Context::create();
        context.set_ini_filename(None);
        context.set_log_filename(None);
        context.io_mut().mouse_draw_cursor = true;
        set_style(&mut context);
        let mut renderer = Renderer::new();
        let font = context.fonts().build_rgba32_texture();
        if !renderer.create_font(device, font.width, font.height, font.data) {
            return None;
        }
        context.fonts().tex_id = crate::imgui_dx9::FONT_TEXTURE;
        let mut ui = Ui {
            context,
            renderer,
            lines: VecDeque::new(),
            log_seen: 0,
            input: String::new(),
            history: Vec::new(),
            history_position: None,
            filter: String::new(),
            auto_scroll: true,
            stick_to_bottom: true,
            channels: BTreeSet::new(),
            focus_input: true,
            scroll_to_end: true,
            last_frame: Instant::now(),
            frame_ms: VecDeque::new(),
        };
        ui.print(OUTPUT_COLOR, "IVMP console. Type help for the commands; F1 closes it.");
        Some(ui)
    }

    fn print(&mut self, color: [f32; 4], text: impl Into<String>) {
        for line in text.into().lines() {
            self.push_line(line, color, String::new());
        }
    }

    fn push_line(&mut self, text: &str, color: [f32; 4], channel: String) {
        if self.lines.len() == MAX_LINES {
            self.lines.pop_front();
        }
        if !channel.is_empty() {
            self.channels.insert(channel.clone());
        }
        self.lines.push_back(Line { text: text.to_string(), color, channel });
        self.scroll_to_end = true;
    }

    fn run_command(&mut self, command: &str) {
        let command = command.trim();
        if command.is_empty() {
            return;
        }
        self.print(INPUT_COLOR, format!("> {command}"));
        self.history.push(command.to_string());
        self.history_position = None;
        let mut words = command.split_whitespace();
        let name = words.next().unwrap_or("").to_lowercase();
        let args: Vec<&str> = words.collect();
        match name.as_str() {
            "devtools" => crate::web::open_devtools(),
            "quit" => crate::render::quit_game(),
            "help" => self.print(
                OUTPUT_COLOR,
                "help                          this list\n\
                 clear                         clear the console\n\
                 overlay <fps|ping|net|perf|all> [on|off]   show or hide a stats panel\n\
                 status                        network and game state\n\
                 devtools                      open the web page's DevTools (developer builds)\n\
                 quit                          close the game\n\
                 echo <text>                   print text",
            ),
            "clear" => self.lines.clear(),
            "echo" => self.print(OUTPUT_COLOR, args.join(" ")),
            "overlay" => self.overlay_command(&args),
            "status" => {
                let (sent, received) = ivmp_networking::net::packet_counts();
                let (mode, slots) = ivmp_networking::net::game_settings();
                self.print(
                    OUTPUT_COLOR,
                    format!(
                        "client id {:?}, host {:?}{}\ngame mode {mode}, slots {slots}\npackets sent {sent}, received {received}",
                        ivmp_networking::net::our_id(),
                        ivmp_networking::net::host_id(),
                        if ivmp_networking::net::is_host() { " (we host)" } else { "" },
                    ),
                );
            }
            _ if WIP_TOOLS.iter().any(|(_, n, _)| n.eq_ignore_ascii_case(&name)) => {
                self.print(ERROR_COLOR, format!("{name} is WIP: not implemented yet"));
            }
            _ => self.print(ERROR_COLOR, format!("unknown command '{name}', try help")),
        }
    }

    fn overlay_command(&mut self, args: &[&str]) {
        let panels: [(&str, &AtomicBool); 4] = [("fps", &SHOW_FPS), ("ping", &SHOW_PING), ("net", &SHOW_NET), ("perf", &SHOW_PERF)];
        let Some(which) = args.first().map(|a| a.to_lowercase()) else {
            let state: Vec<String> =
                panels.iter().map(|(n, f)| format!("{n} {}", if f.load(Ordering::Relaxed) { "on" } else { "off" })).collect();
            self.print(OUTPUT_COLOR, state.join(", "));
            return;
        };
        let value = match args.get(1).map(|a| a.to_lowercase()).as_deref() {
            Some("on") | Some("1") => Some(true),
            Some("off") | Some("0") => Some(false),
            _ => None,
        };
        let mut found = false;
        for (name, flag) in panels {
            if which == "all" || which == name {
                found = true;
                flag.store(value.unwrap_or(!flag.load(Ordering::Relaxed)), Ordering::Relaxed);
            }
        }
        if !found {
            self.print(ERROR_COLOR, "usage: overlay <fps|ping|net|perf|all> [on|off]");
        }
    }

    fn frame(&mut self, device: &IDirect3DDevice9, window: HWND) {
        let now = Instant::now();
        let delta = now.duration_since(self.last_frame).as_secs_f32().clamp(0.0001, 0.5);
        self.last_frame = now;
        self.frame_ms.push_back(delta * 1000.0);
        if self.frame_ms.len() > 240 {
            self.frame_ms.pop_front();
        }

        for line in std::mem::take(&mut *COMPLETION_OUTPUT.lock().unwrap()) {
            self.print(OUTPUT_COLOR, line);
        }
        // New log lines.
        let (next, lines) = log::tail(self.log_seen);
        self.log_seen = next;
        for line in lines {
            let channel = channel_of(&line);
            let color = log_color(&channel, &line);
            self.push_line(&line, color, channel);
        }

        // Display size: the back buffer's viewport.
        let mut viewport = D3DVIEWPORT9::default();
        if unsafe { device.GetViewport(&mut viewport) }.is_err() || viewport.Width == 0 {
            return;
        }
        let size = [viewport.Width as f32, viewport.Height as f32];
        let open = console_open();
        let cursor_wanted = capturing();
        DISPLAY_SIZE.0.store(size[0] as u32, Ordering::Relaxed);
        DISPLAY_SIZE.1.store(size[1] as u32, Ordering::Relaxed);

        // Input.
        let io = self.context.io_mut();
        io.display_size = size;
        io.delta_time = delta;
        if cursor_wanted {
            let mut cursor = POINT::default();
            let mut client = RECT::default();
            if RAW_SEEN.load(Ordering::Relaxed) {
                let mut position = RAW_POSITION.lock().unwrap();
                position[0] = position[0].clamp(0.0, size[0] - 1.0);
                position[1] = position[1].clamp(0.0, size[1] - 1.0);
                io.add_mouse_pos_event(*position);
            } else if unsafe { GetCursorPos(&mut cursor) }.is_ok() && unsafe { ScreenToClient(window, &mut cursor) }.as_bool() {
                let _ = unsafe { GetClientRect(window, &mut client) };
                let scale_x = size[0] / (client.right - client.left).max(1) as f32;
                let scale_y = size[1] / (client.bottom - client.top).max(1) as f32;
                io.add_mouse_pos_event([cursor.x as f32 * scale_x, cursor.y as f32 * scale_y]);
            }
            for (key, vk) in [(Key::ModCtrl, VK_CONTROL), (Key::ModShift, VK_SHIFT), (Key::ModAlt, VK_MENU)] {
                io.add_key_event(key, unsafe { GetKeyState(vk.0 as i32) } < 0);
            }
        }
        for event in std::mem::take(&mut *EVENTS.lock().unwrap()) {
            let io = self.context.io_mut();
            match event {
                Event::Char(c) => io.add_input_character(c),
                Event::Key(key, down) => io.add_key_event(key, down),
                Event::Button(button, down) => io.add_mouse_button_event(button, down),
                Event::Wheel(wheel) => io.add_mouse_wheel_event([0.0, wheel]),
            }
        }
        self.context.io_mut().mouse_draw_cursor = cursor_wanted;

        // The panels are built into locals first: `context.frame()` borrows the context.
        let stats = Stats::collect(&self.frame_ms);
        let mut command: Option<String> = None;
        let mut input = std::mem::take(&mut self.input);
        let focus_input = std::mem::take(&mut self.focus_input);
        let scroll = std::mem::take(&mut self.scroll_to_end);
        let lines = &self.lines;
        let channels = &self.channels;
        let history = &self.history;
        let position = &mut self.history_position;
        let mut filter = std::mem::take(&mut self.filter);
        let mut auto_scroll = self.auto_scroll;
        let mut stick = self.stick_to_bottom;
        let mut clear_log = false;
        let mut open_log = false;

        // Larger text on bigger screens (FiveM uses 18 px up to 1080p, 22 px above).
        self.context.io_mut().font_global_scale = if size[1] <= 1080.0 { 1.4 } else { 1.7 };
        let ui = self.context.frame();
        if open {
            let menu_height = menu_bar(ui);
            let [width, height] = size;
            let line_height = ui.frame_height_with_spacing();
            let _border = ui.push_style_var(imgui::StyleVar::WindowBorderSize(0.0));
            let _padding = ui.push_style_var(imgui::StyleVar::WindowPadding([4.0, 3.0]));
            ui.window("##_BigConsole")
                .position([0.0, menu_height], Condition::Always)
                .size([width, line_height * 12.0], Condition::FirstUseEver)
                .size_constraints([width, line_height * 6.0], [width, height - menu_height])
                .flags(
                    WindowFlags::NO_BRING_TO_FRONT_ON_FOCUS
                        | WindowFlags::NO_TITLE_BAR
                        | WindowFlags::NO_SCROLLBAR
                        | WindowFlags::NO_MOVE
                        | WindowFlags::NO_SAVED_SETTINGS,
                )
                .build(|| {
                    let needle = filter.to_lowercase();
                    let visible: Vec<&Line> = lines
                        .iter()
                        .filter(|l| needle.is_empty() || l.text.to_lowercase().contains(&needle) || l.channel.contains(&needle))
                        .collect();
                    let tag_width = channels.iter().map(|c| ui.calc_text_size(c)[0]).fold(0.0, f32::max) + 16.0;

                    let spacing = ui.push_style_var(imgui::StyleVar::ItemSpacing([4.0, 1.0]));
                    ui.child_window("ScrollingRegion").size([0.0, -ui.frame_height_with_spacing() - 8.0]).build(|| {
                        let clipper = imgui::ListClipper::new(visible.len() as i32).begin(ui);
                        for index in clipper.iter() {
                            let line = visible[index as usize];
                            if line.channel.is_empty() {
                                let _style = ui.push_style_color(StyleColor::Text, line.color);
                                ui.text_wrapped(&line.text);
                                continue;
                            }
                            // Rounded tag with the channel name, then the message.
                            let start = ui.cursor_screen_pos();
                            let size = ui.calc_text_size(&line.channel);
                            ui.get_window_draw_list()
                                .add_rect(
                                    [start[0] - 3.0, start[1]],
                                    [start[0] + size[0] + 3.0, start[1] + size[1]],
                                    channel_color(&line.channel),
                                )
                                .filled(true)
                                .rounding(6.0)
                                .build();
                            ui.text(&line.channel);
                            ui.same_line_with_pos(tag_width);
                            let message = line.text.split_once(':').map(|(_, m)| m.trim_start()).unwrap_or(&line.text);
                            let _style = ui.push_style_color(StyleColor::Text, line.color);
                            ui.text_wrapped(message);
                        }
                        // Stick to the bottom: wrapped lines change the content height after the
                        // frame they are added in, so keep pinning while the view was at the
                        // bottom; scrolling up lets go, scrolling back down sticks again.
                        let at_bottom = ui.scroll_y() >= ui.scroll_max_y() - 4.0;
                        if !scroll {
                            stick = at_bottom;
                        }
                        if auto_scroll && (scroll || stick) {
                            ui.set_scroll_here_y_with_ratio(1.0);
                        }
                        if let Some(_popup) = ui.begin_popup_context_window() {
                            ui.menu_item_config("Copy").enabled(false).build();
                            if ui.is_item_hovered_with_flags(ItemHoveredFlags::ALLOW_WHEN_DISABLED) {
                                ui.tooltip_text("WIP: not implemented yet.\nSelect and copy console text");
                            }
                            if ui.menu_item("Clear") {
                                clear_log = true;
                            }
                        }
                    });
                    drop(spacing);
                    ui.separator();

                    // Input line on the left; filter, auto scroll and the log button on the right.
                    let filter_width = width * 0.15;
                    let scroll_width = ui.calc_text_size("Auto scroll")[0] + ui.frame_height() + 12.0;
                    let button_width = ui.calc_text_size("Open log")[0] + 16.0;
                    let right = filter_width + scroll_width + button_width + 24.0;
                    ui.set_next_item_width(-right);
                    // Keep the keyboard on the input unless something else is being used.
                    if focus_input || (!ui.is_any_item_active() && !ui.is_mouse_clicked(MouseButton::Left)) {
                        ui.set_keyboard_focus_here();
                    }
                    if ui
                        .input_text("##_Input", &mut input)
                        .enter_returns_true(true)
                        .callback(InputTextCallback::HISTORY | InputTextCallback::COMPLETION, CommandLine { history, position })
                        .build()
                    {
                        command = Some(std::mem::take(&mut input));
                        ui.set_keyboard_focus_here_with_offset(imgui::FocusedWidget::Previous);
                    }
                    ui.same_line();
                    ui.set_next_item_width(filter_width);
                    ui.input_text("##LogFilter", &mut filter).hint("Filter").build();
                    ui.same_line();
                    ui.checkbox("Auto scroll", &mut auto_scroll);
                    ui.same_line();
                    if ui.button("Open log") {
                        open_log = true;
                    }
                });
        }
        // After the console, so it is on top of it (and below the menu bar when that is up).
        draw_overlay(ui, &stats, if open { ui.frame_height() + 6.0 } else { 0.0 });
        let draw_data = self.context.render();
        self.renderer.render(device, draw_data);

        self.input = input;
        self.filter = filter;
        self.auto_scroll = auto_scroll;
        self.stick_to_bottom = stick;
        if open_log {
            let name = if crate::instance::second_client() { "IVMP_Client_cl2.log" } else { "IVMP_Client.log" };
            let _ = std::process::Command::new("notepad").arg(crate::dll_dir().join(name)).spawn();
        }
        if clear_log {
            self.lines.clear();
        }
        if let Some(command) = command {
            self.run_command(&command);
        }
        self.focus_input = open && !capturing_was(open);
    }
}

/// Focus the input line again right after the console opens.
fn capturing_was(open: bool) -> bool {
    static WAS_OPEN: AtomicBool = AtomicBool::new(false);
    WAS_OPEN.swap(open, Ordering::Relaxed)
}

struct Stats {
    fps: f32,
    avg_ms: f32,
    min_ms: f32,
    max_ms: f32,
    game_ms: f32,
}

impl Stats {
    fn collect(frame_ms: &VecDeque<f32>) -> Stats {
        let count = frame_ms.len().max(1) as f32;
        let avg_ms = frame_ms.iter().sum::<f32>() / count;
        Stats {
            fps: if avg_ms > 0.0 { 1000.0 / avg_ms } else { 0.0 },
            avg_ms,
            min_ms: frame_ms.iter().cloned().fold(f32::MAX, f32::min).min(9999.0),
            max_ms: frame_ms.iter().cloned().fold(0.0, f32::max),
            game_ms: GAME_TICK_US.load(Ordering::Relaxed) as f32 / 1000.0,
        }
    }
}

fn draw_overlay(ui: &imgui::Ui, stats: &Stats, top: f32) {
    let (fps, ping, net, perf) = (
        SHOW_FPS.load(Ordering::Relaxed),
        SHOW_PING.load(Ordering::Relaxed),
        SHOW_NET.load(Ordering::Relaxed),
        SHOW_PERF.load(Ordering::Relaxed),
    );
    if !(fps || ping || net || perf) {
        return;
    }
    ui.window("##overlay")
        .position([10.0, 10.0 + top], Condition::Always)
        .flags(
            WindowFlags::NO_DECORATION
                | WindowFlags::NO_INPUTS
                | WindowFlags::NO_MOVE
                | WindowFlags::NO_SAVED_SETTINGS
                | WindowFlags::ALWAYS_AUTO_RESIZE
                | WindowFlags::NO_FOCUS_ON_APPEARING
                | WindowFlags::NO_NAV,
        )
        .bg_alpha(0.4)
        .build(|| {
            if fps {
                ui.text(format!("FPS: {:.0}", stats.fps));
            }
            if ping {
                match ivmp_networking::net::ping_ms() {
                    0 => ui.text("Ping: -"),
                    ms => ui.text(format!("Ping: {ms} ms")),
                }
            }
            if net {
                let (sent, received) = ivmp_networking::net::packet_counts();
                let (mode, slots) = ivmp_networking::net::game_settings();
                ui.text(format!(
                    "Client {:?}  Host {:?}{}",
                    ivmp_networking::net::our_id(),
                    ivmp_networking::net::host_id(),
                    if ivmp_networking::net::is_host() { " (us)" } else { "" }
                ));
                ui.text(format!("Mode {mode}  Slots {slots}"));
                ui.text(format!("Packets sent {sent}  received {received}"));
            }
            if perf {
                ui.text(format!("Frame {:.1} ms (min {:.1}, max {:.1})", stats.avg_ms, stats.min_ms, stats.max_ms));
                ui.text(format!("Game update {:.1} ms", stats.game_ms));
            }
        });
}

/// Draws the UI on the game's device. Call right before Present, on the render thread.
pub fn frame(device: &IDirect3DDevice9, window: HWND) {
    let mut guard = UI.lock().unwrap();
    if guard.0.is_none() {
        match Ui::new(device) {
            Some(ui) => {
                log!("ui: imgui ready");
                guard.0 = Some(ui);
            }
            None => return,
        }
    }
    if let Some(ui) = guard.0.as_mut() {
        ui.frame(device, window);
    }
}

/// The game is about to reset the device: free what lives in the default pool.
pub fn device_lost() {
    if let Some(ui) = UI.lock().unwrap().0.as_mut() {
        ui.renderer.invalidate();
    }
}
