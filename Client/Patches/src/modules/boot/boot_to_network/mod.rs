use std::sync::atomic::{AtomicBool, Ordering};

use ivmp_core::{log, memory};

use crate::modules::{
    boot::{force_ep0, game_mode_request, skip_save, streaming_memory_kick},
    multiplayer::{net_game_config, net_game_ep0, network_no_mode_check, pause_world},
    title_screen::{title_action, title_wait},
};
use crate::modules::boot::startup_scripts_skip;

static SESSION_LOADING: AtomicBool = AtomicBool::new(false);

pub fn session_loading() -> bool {
    SESSION_LOADING.load(Ordering::SeqCst)
}

pub fn mark_session_loading() {
    SESSION_LOADING.store(true, Ordering::SeqCst);
}

pub fn use_game_lobby(mode: i32) {
    use ivmp_networking::net;
    if mode != net::LOBBY_MODE || net::our_id().is_none() || net::our_id() != net::host_id() {
        return;
    }
    if session_loading() {
        log!("boot: lobby request arrived after session loading");
        return;
    }
    let Some(site) = memory::find(startup_scripts_skip::STARTUP_PATTERN) else {
        log!("boot: startup script site not found, no lobby");
        return;
    };
    if startup_scripts_skip::restore_story_only(site) {
        log!("boot: gamemode -1: network_startup runs, story startup remains skipped");
    }
}

pub fn install_direct_boot() {
    title_action::apply_patch();
    title_wait::apply_patch();
    skip_save::apply_patch();
    force_ep0::apply_patch();
    pause_world::apply_patch();
    crate::modules::multiplayer::script_stop::apply_patch();
}

pub fn install_network_boot() {
    network_no_mode_check::apply_patch();
    net_game_ep0::apply_patch();
    streaming_memory_kick::apply_patch();
    game_mode_request::install();
    net_game_config::install();
}
