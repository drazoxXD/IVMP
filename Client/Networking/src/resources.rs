//! Resources from the server (docs/scripting-plan.md, stage 3): the server announces the files a
//! resource needs with their hashes, the client fetches what its cache lacks, verifies every file
//! and reports the resource as ready to run. Runs on the network thread; the game thread picks up
//! [`take_changes`].

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use NetProtocol::{ClientMessage, MAX_FILE_BYTES, MAX_RESOURCE_BYTES, file_hash, safe_path};

use crate::log;


/// A resource to start or stop on the game thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Start(String),
    Stop(String),
}

struct FileInfo {
    size: u32,
    hash: [u8; 32],
}

#[derive(Default)]
struct Resource {
    /// Announced files, in the server's order.
    files: Vec<(String, FileInfo)>,
    ready: bool,
    started: bool,
    /// Files that are in the cache and verified.
    done: HashSet<String>,
    /// What has arrived so far of the files being downloaded.
    partial: HashMap<String, Vec<u8>>,
    /// Re-requests per file after a hash mismatch.
    retries: HashMap<String, u8>,
}

#[derive(Default)]
struct State {
    cache: PathBuf,
    resources: HashMap<String, Resource>,
    changes: VecDeque<Change>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    f(STATE.lock().unwrap().get_or_insert_with(State::default))
}

/// Where downloaded resources are kept: `<dir>/<resource>/<path>`.
pub fn set_cache_dir(dir: PathBuf) {
    with(|s| s.cache = dir);
}

/// What changed since the last call; the game thread starts and stops resources accordingly.
pub fn take_changes() -> Vec<Change> {
    with(|s| s.changes.drain(..).collect())
}

fn valid_name(resource: &str) -> bool {
    !resource.is_empty() && !resource.contains('/') && safe_path(resource)
}

/// A new connection: whatever ran for the old server stops.
pub fn reset() {
    with(|s| {
        for (name, resource) in s.resources.drain() {
            if resource.started {
                s.changes.push_back(Change::Stop(name));
            }
        }
    });
}

pub fn on_file(resource: &str, path: &str, size: u32, hash: [u8; 32]) {
    if !valid_name(resource) || !safe_path(path) || size > MAX_FILE_BYTES {
        log!("resources: ignoring the file {resource}/{path} ({size} bytes): not allowed");
        return;
    }
    with(|s| {
        let entry = s.resources.entry(resource.to_string()).or_default();
        // Announced again after it was complete: a restart, which starts from nothing.
        if entry.ready {
            if entry.started {
                s.changes.push_back(Change::Stop(resource.to_string()));
            }
            *entry = Resource::default();
        }
        let total: u64 = entry.files.iter().map(|(_, f)| f.size as u64).sum::<u64>() + size as u64;
        if total > MAX_RESOURCE_BYTES as u64 {
            log!("resources: {resource} is too big, ignoring {path}");
            return;
        }
        entry.files.push((path.to_string(), FileInfo { size, hash }));
    });
}

/// All files of the resource are announced: use what the cache has, ask for the rest.
pub fn on_ready(resource: &str) {
    let cache = with(|s| s.cache.clone());
    let wanted: Vec<(String, u32, [u8; 32])> = with(|s| match s.resources.get_mut(resource) {
        Some(r) => {
            r.ready = true;
            r.files.iter().map(|(p, f)| (p.clone(), f.size, f.hash)).collect()
        }
        None => Vec::new(),
    });
    let mut missing = Vec::new();
    for (path, size, hash) in wanted {
        if cached(&cache, resource, &path, size, &hash) {
            with(|s| {
                if let Some(r) = s.resources.get_mut(resource) {
                    r.done.insert(path);
                }
            });
        } else {
            missing.push(path);
        }
    }
    log!("resources: {resource}: {} file(s) to download", missing.len());
    for path in missing {
        request(resource, &path);
    }
    check_complete(resource);
}

pub fn on_stop(resource: &str) {
    with(|s| {
        if let Some(r) = s.resources.remove(resource) {
            if r.started {
                s.changes.push_back(Change::Stop(resource.to_string()));
            }
        }
    });
}

pub fn on_chunk(resource: &str, path: &str, offset: u32, data: &[u8]) {
    let cache = with(|s| s.cache.clone());
    let outcome = with(|s| {
        let r = s.resources.get_mut(resource)?;
        let (_, info) = r.files.iter().find(|(p, _)| p == path)?;
        if r.done.contains(path) {
            return None;
        }
        let (size, hash) = (info.size as usize, info.hash);
        let buffer = r.partial.entry(path.to_string()).or_default();
        // Chunks arrive in order on the reliable stream; anything else is a stale or bad one.
        if offset as usize != buffer.len() || buffer.len() + data.len() > size {
            r.partial.remove(path);
            return Some(Err(format!("{resource}/{path}: unexpected chunk at {offset}")));
        }
        buffer.extend_from_slice(data);
        if buffer.len() < size {
            return None;
        }
        let complete = r.partial.remove(path).unwrap_or_default();
        if file_hash(&complete) != hash {
            return Some(Err(format!("{resource}/{path}: the downloaded file does not match its hash")));
        }
        Some(Ok(complete))
    });
    match outcome {
        None => {}
        Some(Err(problem)) => {
            log!("resources: {problem}");
            let retry = with(|s| {
                let r = s.resources.get_mut(resource)?;
                let tries = r.retries.entry(path.to_string()).or_insert(0);
                *tries += 1;
                Some(*tries <= 2)
            });
            if retry == Some(true) {
                request(resource, path);
            }
        }
        Some(Ok(bytes)) => {
            let target = cache.join(resource).join(path);
            let written = target
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|_| std::fs::write(&target, &bytes));
            match written {
                Ok(()) => {
                    with(|s| {
                        if let Some(r) = s.resources.get_mut(resource) {
                            r.done.insert(path.to_string());
                        }
                    });
                    check_complete(resource);
                }
                Err(e) => log!("resources: cannot write {}: {e}", target.display()),
            }
        }
    }
}

fn request(resource: &str, path: &str) {
    crate::net::send_reliable(&ClientMessage::FileRequest { resource: resource.to_string(), path: path.to_string() });
}

fn cached(cache: &Path, resource: &str, path: &str, size: u32, hash: &[u8; 32]) -> bool {
    std::fs::read(cache.join(resource).join(path)).is_ok_and(|data| data.len() as u32 == size && file_hash(&data) == *hash)
}

/// Once every announced file is in the cache the resource can start.
fn check_complete(resource: &str) {
    with(|s| {
        let Some(r) = s.resources.get_mut(resource) else { return };
        if r.ready && !r.started && r.files.iter().all(|(p, _)| r.done.contains(p)) {
            r.started = true;
            log!("resources: {resource} is complete");
            s.changes.push_back(Change::Start(resource.to_string()));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The state is global: tests take turns.
    static TURN: Mutex<()> = Mutex::new(());

    fn reset_all() -> std::sync::MutexGuard<'static, ()> {
        let turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
        *STATE.lock().unwrap() = None;
        turn
    }

    #[test]
    fn a_cached_resource_starts_without_downloads() {
        let _turn = reset_all();
        let dir = std::env::temp_dir().join(format!("ivmp_client_cache_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("r")).unwrap();
        std::fs::write(dir.join("r/ivmp.lua"), "client_script 'c.lua'").unwrap();
        std::fs::write(dir.join("r/c.lua"), "print(1)").unwrap();
        set_cache_dir(dir.clone());
        on_file("r", "ivmp.lua", 21, file_hash(b"client_script 'c.lua'"));
        on_file("r", "c.lua", 8, file_hash(b"print(1)"));
        on_ready("r");
        assert_eq!(take_changes(), vec![Change::Start("r".into())]);
        on_stop("r");
        assert_eq!(take_changes(), vec![Change::Stop("r".into())]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn chunks_are_verified_and_written_to_the_cache() {
        let _turn = reset_all();
        let dir = std::env::temp_dir().join(format!("ivmp_client_chunks_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        set_cache_dir(dir.clone());
        let content = b"hello world".to_vec();
        on_file("r", "a/b.lua", content.len() as u32, file_hash(&content));
        on_ready("r"); // nothing cached: requests are sent (no connection in a test, so they go nowhere)
        assert!(take_changes().is_empty());
        on_chunk("r", "a/b.lua", 0, &content[..5]);
        assert!(take_changes().is_empty());
        on_chunk("r", "a/b.lua", 5, &content[5..]);
        assert_eq!(take_changes(), vec![Change::Start("r".into())]);
        assert_eq!(std::fs::read(dir.join("r/a/b.lua")).unwrap(), content);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_wrong_hash_or_a_bad_path_is_not_accepted() {
        let _turn = reset_all();
        let dir = std::env::temp_dir().join(format!("ivmp_client_bad_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        set_cache_dir(dir.clone());
        on_file("r", "../escape.lua", 3, [0; 32]);
        on_file("../r", "x.lua", 3, [0; 32]);
        on_file("r", "c.lua", 3, file_hash(b"abc"));
        on_ready("r");
        on_chunk("r", "c.lua", 0, b"xyz");
        assert!(take_changes().is_empty(), "the content does not match the announced hash");
        assert!(!dir.join("r/c.lua").exists());
        on_chunk("r", "c.lua", 0, b"abc");
        assert_eq!(take_changes(), vec![Change::Start("r".into())]);
        assert!(!dir.join("escape.lua").exists() && !dir.parent().unwrap().join("escape.lua").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn restarting_stops_and_starts_again() {
        let _turn = reset_all();
        let dir = std::env::temp_dir().join(format!("ivmp_client_restart_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        set_cache_dir(dir.clone());
        on_file("r", "c.lua", 3, file_hash(b"abc"));
        on_ready("r");
        on_chunk("r", "c.lua", 0, b"abc");
        assert_eq!(take_changes(), vec![Change::Start("r".into())]);
        // The server restarts it: announced again without a stop first.
        on_file("r", "c.lua", 3, file_hash(b"abc"));
        on_ready("r");
        assert_eq!(take_changes(), vec![Change::Stop("r".into()), Change::Start("r".into())]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
