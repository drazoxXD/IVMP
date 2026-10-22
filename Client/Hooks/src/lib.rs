//! IVMP_Client.dll (x86), injected into GTAIV.exe by the launcher.
//!
//! This crate is the DLL and the game-side glue: boot straight into online, crash / hang logging,
//! focus handling. Networking lives in the Networking crate (`ivmp_networking`), natives in
//! NativeInvoker (`ivmp_natives`), memory and logging helpers in Core (`ivmp_core`).

// The DLL is named IVMP_Client.
#![allow(non_snake_case)]

mod boot;
mod crash;
mod dump;
mod enginelog;
mod events;
mod imgui_dx9;
mod render;
mod ui;
mod web;
mod focus;
mod hang;
mod memstats;
mod luanatives;
mod luaui;
mod mods;
mod scripts;
mod window;

/// Counts the client's heap for the Memory overlay.
#[global_allocator]
static ALLOCATOR: memstats::Counting = memstats::Counting;

pub(crate) use ivmp_core::{log, memory};
use ivmp_natives::nativelog;
use ivmp_networking::{instance, lobby, net, peer};

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::OnceLock;

use ivmp_shared::LaunchConfig;
use windows::Win32::Foundation::{HINSTANCE, HMODULE, TRUE};
use windows::Win32::System::LibraryLoader::{DisableThreadLibraryCalls, GetModuleFileNameW};
use windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use windows::core::BOOL;

static DLL_DIR: OnceLock<PathBuf> = OnceLock::new();
static CONFIG: OnceLock<LaunchConfig> = OnceLock::new();

/// Folder the DLL was loaded from (the launcher's folder).
pub fn dll_dir() -> &'static PathBuf {
    DLL_DIR.get_or_init(|| PathBuf::from("."))
}

/// Settings the launcher wrote before injecting us.
pub fn config() -> &'static LaunchConfig {
    CONFIG.get_or_init(|| LaunchConfig::load(dll_dir(), instance::second_client()))
}

#[unsafe(no_mangle)]
pub extern "system" fn DllMain(instance: HINSTANCE, reason: u32, _reserved: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        let module = HMODULE(instance.0);
        unsafe {
            let _ = DisableThreadLibraryCalls(module);
        }
        let _ = DLL_DIR.set(module_dir(module));
        on_attach();
    }
    TRUE
}

fn on_attach() {
    let log_file = if instance::second_client() { ivmp_shared::HOOKS_LOG_FILE_CL2 } else { ivmp_shared::HOOKS_LOG_FILE };
    log::init(&dll_dir().join(log_file));
    crash::install();
    log!("IVMP_Client attached, game {}", memory::game_version().unwrap_or_else(|| "version unknown".into()));
    log!("command line: {}", command_line());
    log!("IVMP_Client.dll loaded at {:#x}", on_attach as usize & !0xFFFF);
    let config = config();
    log!("player {:?}, server {:?}, direct online: {}", config.nickname, config.server, config.direct_online);
    if instance::second_client() {
        log!("running as the second client ({})", ivmp_shared::SECOND_CLIENT_ARG);
    }
    log!("build: {}", window::title());
    // Only Win32 window calls: no need to wait for the game code to unpack.
    window::install();
    // We are injected while the copy protection may still be unpacking the game. Scanning or
    // patching during that killed the game at random, so wait until it is done, off the loader lock.
    std::thread::spawn(|| {
        if !focus::wait_for_unpack() {
            log!("game code never finished unpacking, no patches applied");
            return;
        }
        // Mods first, so FusionFix and the file overlay are in before the game loads its data.
        // The second client (-cl2) runs without mods.
        if !instance::second_client() {
            mods::install(dll_dir());
        }
        crash::install_exit_logging();
        focus::install();
        render::install();
        web::install();
        // Let another copy of the game start next to this one.
        instance::release_single_instance_mutex();
        let config = crate::config();
        if config.direct_online {
            boot::install();
        }
        if config.network_test {
            let engine_log = if instance::second_client() { "IVMP_Engine_cl2.log" } else { "IVMP_Engine.log" };
            enginelog::install(&dll_dir().join(engine_log));
            nativelog::install();
            nativelog::install_script_names();
            lobby::install_host_override();
            ivmp_networking::hostsys::install_lobby_natives();
            peer::install();
            instance::install_bind_hook();
            ivmp_networking::jointrace::install();
            ivmp_networking::jointrace::install_add_gamer_trace();
            ivmp_networking::jointrace::install_clone_trace();
            ivmp_networking::controltrace::install();
            ivmp_networking::synctrace::install();
            ivmp_networking::netobjects::install();
            ivmp_networking::crypt::install();
            scripts::init();
            net::start(&config.server, &config.nickname);
        }
        boot::hooks_installed();
    });
}

fn module_dir(module: HMODULE) -> PathBuf {
    let mut buf = [0u16; 1024];
    let len = unsafe { GetModuleFileNameW(Some(module), &mut buf) } as usize;
    let path = PathBuf::from(String::from_utf16_lossy(&buf[..len]));
    path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."))
}

/// The game's command line, with anything that looks like a ticket or token blanked out.
fn command_line() -> String {
    let raw = unsafe { windows::Win32::System::Environment::GetCommandLineW().to_string() }.unwrap_or_default();
    raw.split_whitespace()
        .map(|arg| {
            let value = arg.split_once('=').map_or(arg, |(_, v)| v);
            let secret = value.len() > 24 && !value.contains('\\') &&!value.contains('/');
            if secret { "<redacted>" } else { arg }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
