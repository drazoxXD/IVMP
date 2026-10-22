//! Mods folder: `mods\` next to IVMP_Client.dll mirrors the game folder, so mods work without
//! being installed into GTA IV itself.
//!
//! * Files: when the game opens or checks a file inside its own folder and the same relative
//!   path exists under `mods\`, it gets the `mods\` copy instead (e.g. `mods\update\...`).
//! * Plugins: with `mods\dinput8.dll` (Ultimate ASI Loader) present, that loader is loaded and
//!   runs the ASI mods itself (from `mods\plugins`, plus its update folder support); otherwise
//!   every `.asi` (FusionFix and other ASI mods) in `mods\plugins` and in the game folder (its
//!   `plugins`, `scripts` and the folder itself) is loaded directly.
//!
//! Other DLL replacements (d3d9.dll, vulkan.dll, ...) can't be overlaid: Windows loads those
//! before we are injected.

use std::ffi::c_void;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use windows::Win32::System::LibraryLoader::LoadLibraryW;
use windows::core::{HSTRING, PCSTR, PCWSTR};

use crate::{log, memory};

/// Import slots (Ghidra addresses) of the file functions the game uses.
const CREATE_FILE_W: usize = 0x00E7_3154;
const CREATE_FILE_A: usize = 0x00E7_31FC;
const GET_FILE_ATTRIBUTES_W: usize = 0x00E7_3298;
const GET_FILE_ATTRIBUTES_A: usize = 0x00E7_3238;
const GET_FILE_ATTRIBUTES_EX_W: usize = 0x00E7_3084;
const FIND_FIRST_FILE_W: usize = 0x00E7_3290;
const FIND_FIRST_FILE_A: usize = 0x00E7_3248;
const FIND_FIRST_FILE_EX_W: usize = 0x00E7_3088;
const FIND_FIRST_FILE_EX_A: usize = 0x00E7_312C;

/// How many redirections to log (the game opens thousands of files).
const LOG_REDIRECTS: u32 = 50;

struct Roots {
    /// The game folder, lower-case, with a trailing backslash.
    game: String,
    mods: PathBuf,
}

static ROOTS: OnceLock<Roots> = OnceLock::new();
static REDIRECTS: AtomicU32 = AtomicU32::new(0);
/// Plugins (.asi) loaded, by us or by the ASI loader; the web page shows the count.
static LOADED_PLUGINS: AtomicUsize = AtomicUsize::new(0);

/// How many plugins (.asi) were loaded.
pub fn loaded_count() -> usize {
    LOADED_PLUGINS.load(Ordering::Relaxed)
}

/// The .asi files in `folders`, each name once (the first folder holding it wins).
fn plugin_files(folders: &[PathBuf]) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    let mut files = Vec::new();
    for folder in folders {
        let Ok(entries) = std::fs::read_dir(folder) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("asi"))
                && seen.insert(entry.file_name().to_string_lossy().to_lowercase())
            {
                files.push(path);
            }
        }
    }
    files
}

static ORIGINALS: [AtomicUsize; 9] = [const { AtomicUsize::new(0) }; 9];
const I_CREATE_W: usize = 0;
const I_CREATE_A: usize = 1;
const I_ATTR_W: usize = 2;
const I_ATTR_A: usize = 3;
const I_ATTR_EX_W: usize = 4;
const I_FIND_W: usize = 5;
const I_FIND_A: usize = 6;
const I_FIND_EX_W: usize = 7;
const I_FIND_EX_A: usize = 8;

fn original<T>(index: usize) -> T {
    let address = ORIGINALS[index].load(Ordering::SeqCst);
    unsafe { std::mem::transmute_copy(&address) }
}

/// The `mods\` path for a game path, if that file or folder exists there.
fn redirect(path: &Path) -> Option<PathBuf> {
    let roots = ROOTS.get()?;
    let absolute = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir().ok()?.join(path) };
    let text = absolute.to_string_lossy().replace('/', "\\");
    let lower = text.to_lowercase();
    if !lower.starts_with(&roots.game) {
        return None;
    }
    let relative = &text[roots.game.len()..];
    // Wildcard searches ("update\*.*"): redirect when the folder exists in mods.
    let candidate = roots.mods.join(relative);
    let exists = if relative.contains('*') || relative.contains('?') {
        candidate.parent().is_some_and(|p| p.is_dir())
    } else {
        candidate.exists()
    };
    if !exists {
        return None;
    }
    if REDIRECTS.fetch_add(1, Ordering::Relaxed) < LOG_REDIRECTS {
        log!("mods: {relative} -> {}", candidate.display());
    }
    Some(candidate)
}

fn wide_path(p: PCWSTR) -> Option<PathBuf> {
    if p.is_null() {
        return None;
    }
    Some(PathBuf::from(std::ffi::OsString::from_wide(unsafe { p.as_wide() })))
}

fn ansi_path(p: PCSTR) -> Option<PathBuf> {
    if p.is_null() {
        return None;
    }
    Some(PathBuf::from(String::from_utf8_lossy(unsafe { p.as_bytes() }).into_owned()))
}

fn to_wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain([0]).collect()
}

fn to_ansi(path: &Path) -> Vec<u8> {
    path.to_string_lossy().bytes().chain([0]).collect()
}

type CreateFileW = unsafe extern "system" fn(PCWSTR, u32, u32, *const c_void, u32, u32, isize) -> isize;
type CreateFileA = unsafe extern "system" fn(PCSTR, u32, u32, *const c_void, u32, u32, isize) -> isize;
type AttributesW = unsafe extern "system" fn(PCWSTR) -> u32;
type AttributesA = unsafe extern "system" fn(PCSTR) -> u32;
type AttributesExW = unsafe extern "system" fn(PCWSTR, i32, *mut c_void) -> i32;
type FindW = unsafe extern "system" fn(PCWSTR, *mut c_void) -> isize;
type FindA = unsafe extern "system" fn(PCSTR, *mut c_void) -> isize;
type FindExW = unsafe extern "system" fn(PCWSTR, i32, *mut c_void, i32, *const c_void, u32) -> isize;
type FindExA = unsafe extern "system" fn(PCSTR, i32, *mut c_void, i32, *const c_void, u32) -> isize;

unsafe extern "system" fn create_file_w(name: PCWSTR, a: u32, s: u32, sa: *const c_void, c: u32, f: u32, t: isize) -> isize {
    let f_: CreateFileW = original(I_CREATE_W);
    match wide_path(name).and_then(|p| redirect(&p)) {
        Some(new) => unsafe { f_(PCWSTR(to_wide(&new).as_ptr()), a, s, sa, c, f, t) },
        None => unsafe { f_(name, a, s, sa, c, f, t) },
    }
}

unsafe extern "system" fn create_file_a(name: PCSTR, a: u32, s: u32, sa: *const c_void, c: u32, f: u32, t: isize) -> isize {
    let f_: CreateFileA = original(I_CREATE_A);
    match ansi_path(name).and_then(|p| redirect(&p)) {
        Some(new) => unsafe { f_(PCSTR(to_ansi(&new).as_ptr()), a, s, sa, c, f, t) },
        None => unsafe { f_(name, a, s, sa, c, f, t) },
    }
}

unsafe extern "system" fn attributes_w(name: PCWSTR) -> u32 {
    let f_: AttributesW = original(I_ATTR_W);
    match wide_path(name).and_then(|p| redirect(&p)) {
        Some(new) => unsafe { f_(PCWSTR(to_wide(&new).as_ptr())) },
        None => unsafe { f_(name) },
    }
}

unsafe extern "system" fn attributes_a(name: PCSTR) -> u32 {
    let f_: AttributesA = original(I_ATTR_A);
    match ansi_path(name).and_then(|p| redirect(&p)) {
        Some(new) => unsafe { f_(PCSTR(to_ansi(&new).as_ptr())) },
        None => unsafe { f_(name) },
    }
}

unsafe extern "system" fn attributes_ex_w(name: PCWSTR, level: i32, info: *mut c_void) -> i32 {
    let f_: AttributesExW = original(I_ATTR_EX_W);
    match wide_path(name).and_then(|p| redirect(&p)) {
        Some(new) => unsafe { f_(PCWSTR(to_wide(&new).as_ptr()), level, info) },
        None => unsafe { f_(name, level, info) },
    }
}

unsafe extern "system" fn find_w(name: PCWSTR, data: *mut c_void) -> isize {
    let f_: FindW = original(I_FIND_W);
    match wide_path(name).and_then(|p| redirect(&p)) {
        Some(new) => unsafe { f_(PCWSTR(to_wide(&new).as_ptr()), data) },
        None => unsafe { f_(name, data) },
    }
}

unsafe extern "system" fn find_a(name: PCSTR, data: *mut c_void) -> isize {
    let f_: FindA = original(I_FIND_A);
    match ansi_path(name).and_then(|p| redirect(&p)) {
        Some(new) => unsafe { f_(PCSTR(to_ansi(&new).as_ptr()), data) },
        None => unsafe { f_(name, data) },
    }
}

unsafe extern "system" fn find_ex_w(name: PCWSTR, l: i32, d: *mut c_void, o: i32, filter: *const c_void, flags: u32) -> isize {
    let f_: FindExW = original(I_FIND_EX_W);
    match wide_path(name).and_then(|p| redirect(&p)) {
        Some(new) => unsafe { f_(PCWSTR(to_wide(&new).as_ptr()), l, d, o, filter, flags) },
        None => unsafe { f_(name, l, d, o, filter, flags) },
    }
}

unsafe extern "system" fn find_ex_a(name: PCSTR, l: i32, d: *mut c_void, o: i32, filter: *const c_void, flags: u32) -> isize {
    let f_: FindExA = original(I_FIND_EX_A);
    match ansi_path(name).and_then(|p| redirect(&p)) {
        Some(new) => unsafe { f_(PCSTR(to_ansi(&new).as_ptr()), l, d, o, filter, flags) },
        None => unsafe { f_(name, l, d, o, filter, flags) },
    }
}

fn hook_files() {
    let hooks: [(usize, usize, usize); 9] = [
        (CREATE_FILE_W, create_file_w as usize, I_CREATE_W),
        (CREATE_FILE_A, create_file_a as usize, I_CREATE_A),
        (GET_FILE_ATTRIBUTES_W, attributes_w as usize, I_ATTR_W),
        (GET_FILE_ATTRIBUTES_A, attributes_a as usize, I_ATTR_A),
        (GET_FILE_ATTRIBUTES_EX_W, attributes_ex_w as usize, I_ATTR_EX_W),
        (FIND_FIRST_FILE_W, find_w as usize, I_FIND_W),
        (FIND_FIRST_FILE_A, find_a as usize, I_FIND_A),
        (FIND_FIRST_FILE_EX_W, find_ex_w as usize, I_FIND_EX_W),
        (FIND_FIRST_FILE_EX_A, find_ex_a as usize, I_FIND_EX_A),
    ];
    for (slot, hook, index) in hooks {
        let slot = memory::from_ghidra(slot);
        ORIGINALS[index].store(unsafe { std::ptr::read_unaligned(slot as *const usize) }, Ordering::SeqCst);
        if let Err(e) = unsafe { memory::patch(slot, &(hook as u32).to_le_bytes()) } {
            log!("mods: file hook at {slot:#x} failed: {e}");
        }
    }
}

/// Logs the loaded modules that export `IsUltimateASILoader` (what FusionFix looks for), and the
/// ones named like a loader, so a "incompatible version of ASI Loader" popup can be explained.
fn log_asi_loaders(when: &str) {
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::System::LibraryLoader::{GetModuleFileNameW, GetProcAddress};
    use windows::Win32::System::ProcessStatus::EnumProcessModules;
    use windows::Win32::System::Threading::GetCurrentProcess;
    use windows::core::s;

    let mut modules = [HMODULE::default(); 1024];
    let mut needed = 0u32;
    let size = std::mem::size_of_val(&modules) as u32;
    if unsafe { EnumProcessModules(GetCurrentProcess(), modules.as_mut_ptr(), size, &mut needed) }.is_err() {
        log!("mods: {when}: could not list the modules");
        return;
    }
    let count = (needed as usize / std::mem::size_of::<HMODULE>()).min(modules.len());
    let mut found = 0;
    for &module in &modules[..count] {
        let mut name = [0u16; 512];
        let len = unsafe { GetModuleFileNameW(Some(module), &mut name) } as usize;
        let name = String::from_utf16_lossy(&name[..len]);
        let exports = unsafe { GetProcAddress(module, s!("IsUltimateASILoader")) }.is_some();
        let lower = name.to_lowercase();
        let loader_name = ["dinput8.dll", "dsound.dll", "xlive.dll", ".asi"].iter().any(|n| lower.ends_with(n));
        if exports || loader_name {
            log!("mods: {when}: {name} exports IsUltimateASILoader: {exports}");
        }
        found += exports as u32;
    }
    log!("mods: {when}: {count} modules loaded, {found} export IsUltimateASILoader");
}

/// Loads every `.asi` in `folders` (the first folder holding a file name wins, so the same plugin
/// isn't loaded twice). Returns how many were loaded.
fn load_plugins(folders: &[PathBuf]) -> usize {
    let mut loaded = 0;
    for path in plugin_files(folders) {
        match unsafe { LoadLibraryW(&HSTRING::from(path.as_os_str())) } {
            Ok(_) => {
                loaded += 1;
                log!("mods: loaded {}", path.display());
            }
            Err(e) => log!("mods: could not load {}: {e}", path.display()),
        }
    }
    LOADED_PLUGINS.store(loaded, Ordering::Relaxed);
    loaded
}

/// Sets up the file overlay (when there is a `mods\` folder) and loads the plugins. Call once,
/// as early as possible.
pub fn install(dll_dir: &Path) {
    let mut exe = [0u16; 1024];
    let len = unsafe { windows::Win32::System::LibraryLoader::GetModuleFileNameW(None, &mut exe) } as usize;
    let exe = PathBuf::from(std::ffi::OsString::from_wide(&exe[..len]));
    let Some(game_dir) = exe.parent().map(Path::to_path_buf) else { return };

    let mods = dll_dir.join("mods");
    let have_mods = mods.is_dir();
    if have_mods {
        let mut game = game_dir.to_string_lossy().to_lowercase().replace('/', "\\");
        if !game.ends_with('\\') {
            game.push('\\');
        }
        log!("mods: overlaying {} on {game}", mods.display());
        let _ = ROOTS.set(Roots { game, mods: mods.clone() });
        hook_files();
    }

    // Plugins come from `mods\plugins`, then the game folder's own plugins (`plugins`, `scripts`
    // and the folder itself), so an existing FusionFix install works without copying it.
    let mut folders = vec![mods.join("plugins"), game_dir.join("plugins"), game_dir.join("scripts"), game_dir.clone()];
    if !have_mods {
        folders.remove(0);
    }

    // The ASI loader (Ultimate ASI Loader as dinput8.dll) loads the plugins itself, which is what
    // FusionFix expects: it refuses to run when something else loaded it.
    let asi_loader = mods.join("dinput8.dll");
    if have_mods && asi_loader.exists() {
        match unsafe { LoadLibraryW(&HSTRING::from(asi_loader.as_os_str())) } {
            Ok(_) => {
                log!("mods: loaded the ASI loader {}, it loads the plugins", asi_loader.display());
                // It loads from its own folder, `scripts` and `plugins` next to it.
                let plugins = plugin_files(&[mods.clone(), mods.join("scripts"), mods.join("plugins")]);
                LOADED_PLUGINS.store(plugins.len(), Ordering::Relaxed);
                log_asi_loaders("after loading the ASI loader");
                return;
            }
            Err(e) => log!("mods: could not load {}: {e}; loading the plugins directly", asi_loader.display()),
        }
    }
    let loaded = load_plugins(&folders);
    if loaded > 0 {
        log!(
            "mods: loaded {loaded} plugin(s) directly; FusionFix needs the latest Ultimate ASI Loader \
             (https://github.com/ThirteenAG/Ultimate-ASI-Loader/releases/latest) as mods\\dinput8.dll \
             and says \"incompatible version of ASI Loader\" without it"
        );
    }
    if have_mods {
        for proxy in ["d3d9.dll", "dsound.dll", "winmm.dll", "vulkan.dll"] {
            if mods.join(proxy).exists() {
                log!("mods: {proxy} can't be loaded from mods (Windows loads it before us); put it in the game folder");
            }
        }
    }
}
