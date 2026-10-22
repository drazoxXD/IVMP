//! Background worker: start GTA IV through Steam, wait for GTAIV.exe, inject the hooks.
//!
//! The second client (`-cl2`) starts another GTAIV.exe directly, next to the one already running.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use ivmp_shared::{GAME_EXE, HOOKS_DLL, SECOND_CLIENT_ARG, STEAM_APP_ID};
use windows::Win32::System::Threading::WaitForSingleObject;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowThreadProcessId, SW_SHOWNORMAL};
use windows::core::BOOL;
use windows::core::{HSTRING, w};

use crate::inject;

/// How long to wait for GTAIV.exe after asking Steam to start it.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(300);
/// If the game exits this soon after injection, assume a launcher restarted it and wait again.
const RELAUNCH_WINDOW: Duration = Duration::from_secs(20);
const MAX_RELAUNCHES: u32 = 3;
/// The Rockstar Games Launcher's first start of the game sometimes quits at once (exit code
/// 0xC0E90002); Retry in that launcher starts it again, so we wait for the next one.
const MAX_FAILED_STARTS: u32 = 5;

pub enum Event {
    Log(String),
    Status(String),
    /// Hooks are in; the game is running.
    Attached,
    /// The session ended (game closed or an error). `Some` carries the error.
    Finished(Option<String>),
}

/// Runs one play session; `send` delivers progress to the UI.
/// `second_client` carries the exe to start for `-cl2` (empty: copy the running one).
pub fn run(dll_path: PathBuf, second_client: Option<String>, send: impl Fn(Event)) {
    let result = session(&dll_path, second_client, &send);
    send(Event::Finished(result.err()));
}

/// Starts another GTAIV.exe with `-cl2`: the `game_path` from ivmp_cl2.json (like FiveM's
/// `PathCL2`), or else the same exe as the copy that is already running.
///
/// The Steam edition quits silently (exit code 0) unless the Rockstar Games Launcher started it,
/// and that launcher won't start a second copy. So we start it ourselves with the Rockstar
/// launcher (the first copy's parent) set as its parent, and with the arguments it passes.
fn start_second_client(game_path: &str, log: &dyn Fn(String)) -> inject::Result<u32> {
    let first = find_game().ok_or("Start the first client before the second one.")?;
    let exe = if game_path.is_empty() {
        crate::process::image_path(first).ok_or("Could not find where GTAIV.exe is installed.")?
    } else {
        PathBuf::from(game_path)
    };
    let dir = exe.parent().map(|d| d.to_path_buf()).unwrap_or_default();
    let rockstar_launcher =
        crate::process::parent_of(first).ok_or("Could not find the Rockstar Games Launcher that started the game.")?;
    let args = format!(
        "-skipPatcherCheck -steamLocation \"{}\" -windowed -width 1280 -height 720 -useSteam -steamAppId={STEAM_APP_ID} -fromRGL -rglLanguage=en-US {SECOND_CLIENT_ARG}",
        dir.display()
    );
    let pid = crate::process::spawn_with_parent(&exe, &args, &dir, rockstar_launcher)?;
    log(format!("Started a second {GAME_EXE} (pid {pid}) with {SECOND_CLIENT_ARG}, parent pid {rockstar_launcher}"));
    Ok(pid)
}

fn session(dll_path: &PathBuf, second_client: Option<String>, send: &dyn Fn(Event)) -> inject::Result<()> {
    let log = |s: String| send(Event::Log(s));
    let status = |s: &str| send(Event::Status(s.to_string()));

    if !dll_path.exists() {
        return Err(format!("{} is missing next to the launcher.", dll_path.display()));
    }

    let mut second_pid = None;
    if let Some(game_path) = &second_client {
        status("Starting the second client...");
        second_pid = Some(start_second_client(game_path, &log)?);
    } else if find_game().is_none() {
        status("Starting GTA IV through Steam...");
        let url = HSTRING::from(format!("steam://rungameid/{STEAM_APP_ID}"));
        let result = unsafe { ShellExecuteW(None, w!("open"), &url, None, None, SW_SHOWNORMAL) };
        // ShellExecuteW reports success with a value greater than 32.
        if result.0 as usize <= 32 {
            return Err("Could not open the steam:// link. Is Steam installed?".into());
        }
        log(format!("Asked Steam to run app {STEAM_APP_ID}"));
    } else {
        log(format!("{GAME_EXE} is already running, attaching to it"));
    }

    let mut relaunches = 0;
    let mut failed_starts = 0;
    loop {
        status("Waiting for the game to start...");
        let pid = match second_pid {
            Some(pid) => pid,
            None => wait_for_game(LAUNCH_TIMEOUT)?,
        };
        log(format!("Found {GAME_EXE} (pid {pid})"));

        // Touch the process as little as possible until the game has opened its window: the copy
        // protection crashes at random (0x824B16AD) if the process is read or injected while it
        // is still unpacking. The window only exists once the unpacked game code runs.
        status("Waiting for the game window...");
        if !wait_for_window(pid, LAUNCH_TIMEOUT)? {
            failed_starts += 1;
            if second_pid.is_some() || failed_starts > MAX_FAILED_STARTS {
                return Err(format!("{GAME_EXE} exited before opening its window."));
            }
            log(format!("{GAME_EXE} quit while starting; press Retry in the Rockstar Games Launcher"));
            status("Game quit while starting: press Retry in the Rockstar Games Launcher");
            continue;
        }
        log("Game window is up".into());

        let game = inject::open_game(pid)?;
        let kernel32 = inject::wait_for_kernel32(game.0, Duration::from_secs(30))?;
        log(format!("32-bit kernel32 at {kernel32:#010x}"));
        let unpacked = inject::wait_for_unpack(game.0, GAME_EXE, Duration::from_secs(60))?;
        log(format!("Game code unpacked after {} ms", unpacked.as_millis()));

        if inject::is_loaded(game.0, HOOKS_DLL) {
            log("Hooks are already loaded in this game process".into());
        } else {
            status("Injecting hooks...");
            let base = inject::inject(game.0, kernel32, dll_path)?;
            log(format!("Loaded {HOOKS_DLL} at {base:#010x}"));
        }
        send(Event::Attached);
        status("In game");

        let attached_at = Instant::now();
        unsafe { WaitForSingleObject(game.0, u32::MAX) };
        let lived = attached_at.elapsed();

        // A second client we started ourselves is never relaunched by Steam.
        if second_pid.is_none() && lived < RELAUNCH_WINDOW && relaunches < MAX_RELAUNCHES {
            relaunches += 1;
            log(format!("Game exited after {}s, waiting for it to relaunch", lived.as_secs()));
            continue;
        }
        log("Game closed".into());
        return Ok(());
    }
}

/// Waits until `pid` owns a top-level window: `true`, or `false` if it exited first. Uses only
/// window APIs, never opens the process.
fn wait_for_window(pid: u32, timeout: Duration) -> inject::Result<bool> {
    unsafe extern "system" fn check(window: HWND, found: LPARAM) -> BOOL {
        let (pid, found) = unsafe { &mut *(found.0 as *mut (u32, bool)) };
        let mut owner = 0u32;
        unsafe { GetWindowThreadProcessId(window, Some(&mut owner)) };
        if owner == *pid {
            *found = true;
            return BOOL(0);
        }
        BOOL(1)
    }
    let start = Instant::now();
    loop {
        let mut state = (pid, false);
        unsafe {
            let _ = EnumWindows(Some(check), LPARAM(&mut state as *mut _ as isize));
        }
        if state.1 {
            return Ok(true);
        }
        if !crate::process::find_processes(GAME_EXE).contains(&pid) {
            return Ok(false);
        }
        if start.elapsed() > timeout {
            return Err("The game window did not appear.".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The folder of the running game, to write its `commandline.txt` into.
pub fn game_dir() -> Option<PathBuf> {
    find_game().and_then(crate::process::image_path).and_then(|exe| exe.parent().map(|d| d.to_path_buf()))
}

/// The resolutions the main display offers, biggest first, as `(width, height)`.
pub fn resolutions() -> Vec<(u32, u32)> {
    use windows::Win32::Graphics::Gdi::{DEVMODEW, EnumDisplaySettingsW, ENUM_CURRENT_SETTINGS};
    let mut found = Vec::new();
    let mut mode = DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..Default::default() };
    let mut index = 0;
    while unsafe { EnumDisplaySettingsW(None, windows::Win32::Graphics::Gdi::ENUM_DISPLAY_SETTINGS_MODE(index), &mut mode) }.as_bool() {
        let size = (mode.dmPelsWidth, mode.dmPelsHeight);
        if size.0 >= 800 && size.1 >= 600 && !found.contains(&size) {
            found.push(size);
        }
        index += 1;
    }
    let _ = ENUM_CURRENT_SETTINGS;
    found.sort_by(|a, b| b.cmp(a));
    found
}

fn find_game() -> Option<u32> {
    crate::process::find_process(GAME_EXE)
}

fn wait_for_game(timeout: Duration) -> inject::Result<u32> {
    let start = Instant::now();
    loop {
        if let Some(pid) = find_game() {
            return Ok(pid);
        }
        if start.elapsed() > timeout {
            return Err(format!("{GAME_EXE} did not start within {} seconds.", timeout.as_secs()));
        }
        // Poll fast so the hooks land before the engine gets far into its startup.
        std::thread::sleep(Duration::from_millis(50));
    }
}
