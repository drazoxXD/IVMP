//! In-game UI drawn with dear imgui on the game's render thread: the F1 console and the stats
//! overlay (FPS, ping, network, client performance). `render.rs` calls `frame` before every
//! Present; `input` feeds it the window messages the game would otherwise get.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use imgui::{Condition, Context, ItemHoveredFlags, Key, MouseButton, StyleColor, TreeNodeFlags, WindowFlags};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Direct3D9::*;
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::imgui_dx9::Renderer;
use crate::log;

/// The console is open: the game gets no keyboard or mouse input.
static CONSOLE_OPEN: AtomicBool = AtomicBool::new(false);

pub fn capturing() -> bool {
    CONSOLE_OPEN.load(Ordering::Relaxed)
}

pub fn toggle_console() {
    let open = !CONSOLE_OPEN.load(Ordering::Relaxed);
    CONSOLE_OPEN.store(open, Ordering::Relaxed);
}

/// Overlay panels, toggled from the console.
static SHOW_FPS: AtomicBool = AtomicBool::new(false);
static SHOW_PING: AtomicBool = AtomicBool::new(false);
static SHOW_NET: AtomicBool = AtomicBool::new(false);
static SHOW_PERF: AtomicBool = AtomicBool::new(false);

/// Microseconds the game's own update took last frame (set by `game_process` in boot.rs).
pub static GAME_TICK_US: AtomicU32 = AtomicU32::new(0);
/// Round trip to the server in milliseconds (0 = unknown); set by the networking.
pub static PING_MS: AtomicU32 = AtomicU32::new(0);

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
            if lparam & (1 << 30) == 0 {
                toggle_console();
            }
            return true;
        }
        _ => {}
    }
    if !capturing() {
        return false;
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
    ("Console", "nui_devtools", "Dev tools for the web UI (needs the web engine, stage 2)"),
    ("Scripting", "script_debug", "Script engine debugger: native calls, running scripts (needs the native hook)"),
];

/// A disabled checkbox whose tooltip says it's not implemented.
fn wip_item(ui: &imgui::Ui, name: &str, about: &str) {
    ui.disabled(true, || {
        let mut off = false;
        ui.checkbox(name, &mut off);
    });
    if ui.is_item_hovered_with_flags(ItemHoveredFlags::ALLOW_WHEN_DISABLED) {
        ui.tooltip_text(format!("WIP: not implemented yet.
{about}"));
    }
}

/// A checkbox for a panel flag.
fn flag_item(ui: &imgui::Ui, label: &str, flag: &AtomicBool, about: &str) {
    let mut value = flag.load(Ordering::Relaxed);
    if ui.checkbox(label, &mut value) {
        flag.store(value, Ordering::Relaxed);
    }
    if ui.is_item_hovered() {
        ui.tooltip_text(about);
    }
}

fn tools_panel(ui: &imgui::Ui) {
    if !ui.collapsing_header("Overlays and debug tools", TreeNodeFlags::empty()) {
        return;
    }
    ui.text_disabled("Working");
    flag_item(ui, "FPS", &SHOW_FPS, "overlay fps: frames per second");
    ui.same_line();
    flag_item(ui, "Ping", &SHOW_PING, "overlay ping: round trip to the server");
    ui.same_line();
    flag_item(ui, "Network info", &SHOW_NET, "overlay net: ids, mode, packets");
    ui.same_line();
    flag_item(ui, "Client performance", &SHOW_PERF, "overlay perf: frame and game update times");
    let mut group = "";
    for (g, name, about) in WIP_TOOLS {
        if *g != group {
            group = g;
            ui.separator();
            ui.text_disabled(format!("{g} (WIP)"));
        } else {
            ui.same_line();
        }
        wip_item(ui, name, about);
    }
    ui.separator();
}

struct Line {
    text: String,
    color: [f32; 4],
}

const LOG_COLOR: [f32; 4] = [0.62, 0.64, 0.68, 1.0];
const INPUT_COLOR: [f32; 4] = [0.55, 0.8, 1.0, 1.0];
const OUTPUT_COLOR: [f32; 4] = [0.9, 0.9, 0.9, 1.0];
const ERROR_COLOR: [f32; 4] = [1.0, 0.45, 0.4, 1.0];

struct Ui {
    context: Context,
    renderer: Renderer,
    lines: VecDeque<Line>,
    log_seen: u64,
    input: String,
    history: Vec<String>,
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
            if self.lines.len() == MAX_LINES {
                self.lines.pop_front();
            }
            self.lines.push_back(Line { text: line.to_string(), color });
        }
        self.scroll_to_end = true;
    }

    fn run_command(&mut self, command: &str) {
        let command = command.trim();
        if command.is_empty() {
            return;
        }
        self.print(INPUT_COLOR, format!("> {command}"));
        self.history.push(command.to_string());
        let mut words = command.split_whitespace();
        let name = words.next().unwrap_or("").to_lowercase();
        let args: Vec<&str> = words.collect();
        match name.as_str() {
            "help" => self.print(
                OUTPUT_COLOR,
                "help                          this list\n\
                 clear                         clear the console\n\
                 overlay <fps|ping|net|perf|all> [on|off]   show or hide a stats panel\n\
                 status                        network and game state\n\
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

        // New log lines.
        let (next, lines) = log::tail(self.log_seen);
        self.log_seen = next;
        for line in lines {
            self.print(LOG_COLOR, line);
        }

        // Display size: the back buffer's viewport.
        let mut viewport = D3DVIEWPORT9::default();
        if unsafe { device.GetViewport(&mut viewport) }.is_err() || viewport.Width == 0 {
            return;
        }
        let size = [viewport.Width as f32, viewport.Height as f32];
        let open = capturing();

        // Input.
        let io = self.context.io_mut();
        io.display_size = size;
        io.delta_time = delta;
        if open {
            let mut cursor = POINT::default();
            let mut client = RECT::default();
            if unsafe { GetCursorPos(&mut cursor) }.is_ok() && unsafe { ScreenToClient(window, &mut cursor) }.as_bool() {
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
        self.context.io_mut().mouse_draw_cursor = open;

        // The panels are built into locals first: `context.frame()` borrows the context.
        let stats = Stats::collect(&self.frame_ms);
        let mut command: Option<String> = None;
        let mut input = std::mem::take(&mut self.input);
        let focus_input = std::mem::take(&mut self.focus_input);
        let scroll = std::mem::take(&mut self.scroll_to_end);
        let lines = &self.lines;

        let ui = self.context.frame();
        draw_overlay(ui, &stats);
        if open {
            let [width, height] = size;
            ui.window("IVMP console")
                .position([0.0, 0.0], Condition::Always)
                .size([width, height * 0.45], Condition::Always)
                .flags(WindowFlags::NO_MOVE | WindowFlags::NO_RESIZE | WindowFlags::NO_COLLAPSE)
                .bg_alpha(0.85)
                .build(|| {
                    tools_panel(ui);
                    let footer = ui.frame_height_with_spacing() + 4.0;
                    ui.child_window("log").size([0.0, -footer]).build(|| {
                        for line in lines {
                            let _style = ui.push_style_color(StyleColor::Text, line.color);
                            ui.text_wrapped(&line.text);
                        }
                        if scroll {
                            ui.set_scroll_here_y_with_ratio(1.0);
                        }
                    });
                    ui.set_next_item_width(-1.0);
                    if focus_input {
                        ui.set_keyboard_focus_here();
                    }
                    if ui.input_text("##command", &mut input).enter_returns_true(true).build() {
                        command = Some(std::mem::take(&mut input));
                        // Keep typing after Enter.
                        ui.set_keyboard_focus_here_with_offset(imgui::FocusedWidget::Previous);
                    }
                });
        }
        let draw_data = self.context.render();
        self.renderer.render(device, draw_data);

        self.input = input;
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

fn draw_overlay(ui: &imgui::Ui, stats: &Stats) {
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
        .position([10.0, 10.0], Condition::Always)
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
                match PING_MS.load(Ordering::Relaxed) {
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
