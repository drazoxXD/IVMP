//! Shared between the 64-bit launcher and the 32-bit hooks DLL.
//!
//! Both binaries live in the same folder, so the launcher writes `ivmp.json`
//! next to itself and the DLL reads it from its own folder once it is loaded
//! into GTAIV.exe.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Steam app id of Grand Theft Auto IV: The Complete Edition.
pub const STEAM_APP_ID: u32 = 12210;
/// Process name of the actual game (Steam may start a launcher first).
pub const GAME_EXE: &str = "GTAIV.exe";
/// File name of the hooks DLL injected into the game.
pub const HOOKS_DLL: &str = "IVMP_Client.dll";
pub const CONFIG_FILE: &str = "ivmp.json";
pub const HOOKS_LOG_FILE: &str = "IVMP_Client.log";
/// Second client on the same PC (like FiveM's `-cl2`): passed to the launcher and to GTAIV.exe.
pub const SECOND_CLIENT_ARG: &str = "-cl2";
pub const CONFIG_FILE_CL2: &str = "ivmp_cl2.json";
pub const HOOKS_LOG_FILE_CL2: &str = "IVMP_Client_cl2.log";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LaunchConfig {
    pub nickname: String,
    /// `host:port` of the server to join once the game is up.
    pub server: String,
    /// Skip the story mode boot (intro, menus, startup script) and go straight online.
    pub direct_online: bool,
    /// Experiment: once in the world, ask the engine to switch to its own network game mode.
    /// Not shown in the launcher; set it by hand in ivmp.json.
    pub network_test: bool,
    /// GTAIV.exe to start for the second client (FiveM's `PathCL2`), e.g. a second copy of the
    /// game folder. Empty: use the exe of the copy that is already running.
    pub game_path: String,
}

impl Default for LaunchConfig {
    fn default() -> Self {
        Self {
            nickname: "Player".into(),
            server: "127.0.0.1:9999".into(),
            direct_online: true,
            network_test: false,
            game_path: String::new(),
        }
    }
}

impl LaunchConfig {
    /// The second client keeps its own settings file, so the two copies can use different names.
    pub fn path_in(dir: &Path, second_client: bool) -> PathBuf {
        dir.join(if second_client { CONFIG_FILE_CL2 } else { CONFIG_FILE })
    }

    fn read(path: &Path) -> Option<Self> {
        std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok())
    }

    /// Without its own file yet, the second client starts from the first one's settings
    /// with "2" added to the nickname.
    pub fn load(dir: &Path, second_client: bool) -> Self {
        if let Some(config) = Self::read(&Self::path_in(dir, second_client)) {
            return config;
        }
        let mut config = Self::read(&Self::path_in(dir, false)).unwrap_or_default();
        if second_client {
            config.nickname.push('2');
        }
        config
    }

    pub fn save(&self, dir: &Path, second_client: bool) -> std::io::Result<()> {
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(Self::path_in(dir, second_client), json)
    }
}
