//! IVMP launcher (x64). Native Win32 UI via native-windows-gui.
//!
//! Steam owns the game licence, so there's no login here: we start the Steam
//! copy of GTA IV: Complete Edition, wait for GTAIV.exe and inject
//! `IVMP_Client.dll` (x86), which takes over the boot and loads straight into online.

// No console window, debug builds too: the launcher reports through its own window.
#![windows_subsystem = "windows"]

mod game;
mod inject;
mod process;

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};

use ivmp_shared::{HOOKS_DLL, LaunchConfig};
use native_windows_derive::NwgUi;
use native_windows_gui as nwg;
use nwg::NativeUi;

#[derive(Default, NwgUi)]
pub struct Launcher {
    #[nwg_resource]
    embed: nwg::EmbedResource,

    // Icons/logo.ico, embedded by build.rs as resource id 1.
    #[nwg_resource(source_embed: Some(&data.embed), source_embed_id: 1)]
    icon: nwg::Icon,

    #[nwg_control(size: (460, 400), center: true, title: "IVMP Launcher", icon: Some(&data.icon), flags: "WINDOW|VISIBLE")]
    #[nwg_events(OnWindowClose: [Launcher::on_close], OnInit: [Launcher::on_init])]
    window: nwg::Window,

    #[nwg_layout(parent: window, spacing: 6, margin: [10, 10, 10, 10])]
    grid: nwg::GridLayout,

    #[nwg_control(text: "Nickname")]
    #[nwg_layout_item(layout: grid, row: 0, col: 0)]
    nickname_label: nwg::Label,

    #[nwg_control(limit: 24)]
    #[nwg_layout_item(layout: grid, row: 0, col: 1, col_span: 3)]
    nickname: nwg::TextInput,

    #[nwg_control(text: "Server")]
    #[nwg_layout_item(layout: grid, row: 1, col: 0)]
    server_label: nwg::Label,

    #[nwg_control]
    #[nwg_layout_item(layout: grid, row: 1, col: 1, col_span: 3)]
    server: nwg::TextInput,

    #[nwg_control(text: "Play")]
    #[nwg_layout_item(layout: grid, row: 2, col: 3)]
    #[nwg_events(OnButtonClick: [Launcher::on_play])]
    play: nwg::Button,

    #[nwg_control(text: "Ready")]
    #[nwg_layout_item(layout: grid, row: 2, col: 0, col_span: 3)]
    status: nwg::Label,

    #[nwg_control]
    #[nwg_layout_item(layout: grid, row: 3, col: 0, col_span: 4, row_span: 6)]
    log: nwg::ListBox<String>,

    #[nwg_control]
    #[nwg_events(OnNotice: [Launcher::on_worker_event])]
    notice: nwg::Notice,

    events: RefCell<Option<Receiver<game::Event>>>,
}

impl Launcher {
    fn on_init(&self) {
        if second_client() {
            self.window.set_text("IVMP Launcher (second client)");
            self.play.set_text("Play as second client");
        }
        let config = LaunchConfig::load(&exe_dir(), second_client());
        self.nickname.set_text(&config.nickname);
        self.server.set_text(&config.server);
    }

    fn on_play(&self) {
        let nickname = self.nickname.text().trim().to_string();
        if nickname.is_empty() {
            nwg::modal_error_message(&self.window, "IVMP", "Pick a nickname first.");
            return;
        }
        let config = LaunchConfig {
            nickname,
            server: self.server.text().trim().to_string(),
            // IVMP always boots straight into online (no option for the story mode boot).
            direct_online: true,
            // Keep settings the launcher has no UI for.
            ..LaunchConfig::load(&exe_dir(), second_client())
        };
        // The hooks DLL reads this from the same folder once it's inside the game.
        if let Err(e) = config.save(&exe_dir(), second_client()) {
            self.push_log(format!("Could not save settings: {e}"));
            return;
        }

        self.play.set_enabled(false);
        let (tx, rx) = channel();
        *self.events.borrow_mut() = Some(rx);
        let notice = self.notice.sender();
        let dll = exe_dir().join(HOOKS_DLL);
        let second = second_client().then(|| config.game_path.clone());
        std::thread::spawn(move || {
            game::run(dll, second, |event| {
                let _ = tx.send(event);
                notice.notice();
            });
        });
    }

    fn on_worker_event(&self) {
        let events: Vec<game::Event> = match self.events.borrow().as_ref() {
            Some(rx) => rx.try_iter().collect(),
            None => return,
        };
        for event in events {
            match event {
                game::Event::Log(line) => self.push_log(line),
                game::Event::Status(text) => self.status.set_text(&text),
                game::Event::Attached => self.push_log("Hooks attached".into()),
                game::Event::Finished(error) => {
                    match error {
                        Some(e) => {
                            self.status.set_text("Failed");
                            self.push_log(format!("Error: {e}"));
                            nwg::modal_error_message(&self.window, "IVMP", &e);
                        }
                        None => self.status.set_text("Ready"),
                    }
                    self.play.set_enabled(true);
                }
            }
        }
    }

    fn push_log(&self, line: String) {
        self.log.push(line);
        let len = self.log.len();
        if len > 0 {
            self.log.set_selection(Some(len - 1));
        }
    }

    fn on_close(&self) {
        nwg::stop_thread_dispatch();
    }
}

/// `ivmp_launcher.exe -cl2`: start a second copy of the game next to the running one.
fn second_client() -> bool {
    std::env::args().skip(1).any(|arg| arg.eq_ignore_ascii_case(ivmp_shared::SECOND_CLIENT_ARG))
}

fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn main() {
    nwg::init().expect("Failed to init Native Windows GUI");
    let _ = nwg::Font::set_global_family("Segoe UI");
    let _ui = Launcher::build_ui(Default::default()).expect("Failed to build UI");
    nwg::dispatch_thread_events();
}
