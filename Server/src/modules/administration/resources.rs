//! What clients get of a resource (docs/scripting-plan.md, stage 3): the files they need, and a
//! queue that sends those files in paced chunks.

use std::collections::{HashSet, VecDeque};
use std::path::Path;

use ivmp_scripting::manifest::{self, Manifest};
use NetProtocol::{FILE_CHUNK, MAX_FILE_BYTES, MAX_RESOURCE_BYTES, file_hash, safe_path};

/// One file a client downloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub path: String,
    pub size: u32,
    pub hash: [u8; 32],
}

/// The files of a resource clients need, or `None` if it has nothing for clients (only server
/// scripts). The manifest (`ivmp.lua`, or a FiveM `fxmanifest.lua` / `__resource.lua`) is always
/// one of them, so the client can start the resource like the server does. An entry of `files`
/// that doesn't exist is left out (FiveM resources list optional ones, e.g. the chat's config.js);
/// a missing script or ui_page is an error.
pub fn offer(dir: &Path, manifest: &Manifest) -> Result<Option<Vec<FileEntry>>, String> {
    let mut wanted: Vec<&str> = Vec::new();
    wanted.extend(manifest.client_scripts.iter().map(String::as_str));
    wanted.extend(manifest.files.iter().map(String::as_str));
    wanted.extend(manifest.ui_page.as_deref());
    if wanted.is_empty() {
        return Ok(None);
    }
    let mut paths = vec![manifest::find(dir).unwrap_or(manifest::FILE)];
    paths.extend(wanted);
    let mut seen = HashSet::new();
    let mut files = Vec::new();
    let mut total = 0u64;
    for path in paths {
        if !seen.insert(path) {
            continue;
        }
        if !safe_path(path) {
            return Err(format!("'{path}' is not a plain relative path"));
        }
        let optional = manifest.files.iter().any(|f| f == path)
            && !manifest.client_scripts.iter().any(|f| f == path)
            && manifest.ui_page.as_deref() != Some(path);
        if optional && !dir.join(path).is_file() {
            continue;
        }
        let data = std::fs::read(dir.join(path)).map_err(|e| format!("cannot read {path}: {e}"))?;
        if data.len() as u64 > MAX_FILE_BYTES as u64 {
            return Err(format!("{path} is too big ({} bytes, at most {MAX_FILE_BYTES})", data.len()));
        }
        total += data.len() as u64;
        if total > MAX_RESOURCE_BYTES as u64 {
            return Err(format!("the files for clients are too big together (at most {MAX_RESOURCE_BYTES} bytes)"));
        }
        files.push(FileEntry { path: path.to_string(), size: data.len() as u32, hash: file_hash(&data) });
    }
    Ok(Some(files))
}

/// A file being sent to one client.
pub struct Transfer {
    pub resource: String,
    pub path: String,
    data: Vec<u8>,
    offset: usize,
    done: bool,
}

impl Transfer {
    pub fn new(resource: String, path: String, data: Vec<u8>) -> Transfer {
        Transfer { resource, path, data, offset: 0, done: false }
    }

    /// The next chunk as (offset, bytes); None once everything was handed out. An empty file sends
    /// one empty chunk, so the client learns it is complete.
    pub fn next_chunk(&mut self) -> Option<(u32, Vec<u8>)> {
        if self.done {
            return None;
        }
        let end = (self.offset + FILE_CHUNK).min(self.data.len());
        let chunk = (self.offset as u32, self.data[self.offset..end].to_vec());
        self.offset = end;
        self.done = end >= self.data.len();
        Some(chunk)
    }
}

/// What one client still has to receive.
pub type Queue = VecDeque<Transfer>;

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ivmp_offer_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("ui")).unwrap();
        dir
    }

    #[test]
    fn a_server_only_resource_offers_nothing() {
        let manifest = Manifest { server_scripts: vec!["s.lua".into()], ..Manifest::default() };
        assert_eq!(offer(Path::new("."), &manifest), Ok(None));
    }

    #[test]
    fn the_offer_lists_the_manifest_and_what_clients_need_once() {
        let dir = temp("list");
        std::fs::write(dir.join("ivmp.lua"), "client_script 'c.lua'").unwrap();
        std::fs::write(dir.join("c.lua"), "print('hi')").unwrap();
        std::fs::write(dir.join("ui/index.html"), "<html>").unwrap();
        let manifest = Manifest {
            client_scripts: vec!["c.lua".into()],
            files: vec!["ui/index.html".into(), "c.lua".into()],
            ui_page: Some("ui/index.html".into()),
            ..Manifest::default()
        };
        let files = offer(&dir, &manifest).unwrap().unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["ivmp.lua", "c.lua", "ui/index.html"]);
        assert_eq!(files[1].size, 11);
        assert_eq!(files[1].hash, file_hash(b"print('hi')"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unsafe_and_missing_files_are_refused() {
        let dir = temp("bad");
        std::fs::write(dir.join("ivmp.lua"), "").unwrap();
        let escape = Manifest { client_scripts: vec!["../secret.lua".into()], ..Manifest::default() };
        assert!(offer(&dir, &escape).is_err());
        let missing = Manifest { client_scripts: vec!["nope.lua".into()], ..Manifest::default() };
        assert!(offer(&dir, &missing).unwrap_err().contains("nope.lua"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_fivem_resource_sends_its_manifest_and_skips_missing_extra_files() {
        let dir = temp("fivem");
        std::fs::write(dir.join("__resource.lua"), "client_script 'c.lua'").unwrap();
        std::fs::write(dir.join("c.lua"), "").unwrap();
        let manifest = Manifest { client_scripts: vec!["c.lua".into()], files: vec!["config.js".into()], ..Manifest::default() };
        let files = offer(&dir, &manifest).unwrap().unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["__resource.lua", "c.lua"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_file_goes_out_in_chunks_that_add_up() {
        let data: Vec<u8> = (0..2000u32).map(|i| i as u8).collect();
        let mut transfer = Transfer::new("r".into(), "f".into(), data.clone());
        let mut got = Vec::new();
        while let Some((offset, chunk)) = transfer.next_chunk() {
            assert_eq!(offset as usize, got.len());
            assert!(chunk.len() <= FILE_CHUNK);
            got.extend(chunk);
        }
        assert_eq!(got, data);
        let mut empty = Transfer::new("r".into(), "f".into(), Vec::new());
        assert_eq!(empty.next_chunk(), Some((0, Vec::new())), "an empty file still sends one chunk");
        assert_eq!(empty.next_chunk(), None);
    }
}
