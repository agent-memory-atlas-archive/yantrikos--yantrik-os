//! The files an app's open documents live in, as the housekeeping thread watches them.
//!
//! The core adds and removes entries (`fx.watch`, `fx.unwatch`) on the UI thread; the watch
//! thread looks at every entry each tick and reports what changed. Each entry remembers what was
//! last seen, so only a change is reported, and an entry added with the document's baseline does
//! not report the file it was just read from.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::{Fingerprint, Observed};

/// Past this size a file is fingerprinted by its length and modification time, not its bytes:
/// hashing a large file every two seconds would cost more than the edit it might catch.
pub const READ_LIMIT: u64 = 16 * 1024 * 1024;

/// What is at `path` now: the file's fingerprint, or `Gone`. `None` when it could not be looked
/// at (a permission or I/O error): not a change, so the entry keeps what it last saw.
pub fn look(path: &Path) -> Option<Observed> {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(Observed::Gone),
        Err(_) => return None,
    };
    if !meta.is_file() {
        return Some(Observed::Gone);
    }
    if meta.len() > READ_LIMIT {
        let stamp = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos() as u64);
        return Some(Observed::File(Fingerprint::of_metadata(meta.len(), stamp)));
    }
    match std::fs::read(path) {
        Ok(bytes) => Some(Observed::File(Fingerprint::of_bytes(&bytes))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(Observed::Gone),
        Err(_) => None,
    }
}

#[derive(Debug, Clone)]
struct Entry {
    path: PathBuf,
    seen: Option<Observed>,
}

/// The table, shared by the core and the watch thread.
#[derive(Debug, Clone, Default)]
pub struct Watched(Arc<Mutex<BTreeMap<String, Entry>>>);

impl Watched {
    pub fn new() -> Watched {
        Watched::default()
    }

    fn table(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Entry>> {
        // A poisoned table is still a table: a panic elsewhere cannot stop the watch.
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Watches `path` under `key`. `seen` is what the app knows is there (the document's
    /// baseline), so only a change from it is reported; `None` reports the first look.
    pub fn add(&self, key: &str, path: PathBuf, seen: Option<Observed>) {
        self.table().insert(key.to_string(), Entry { path, seen });
    }

    pub fn remove(&self, key: &str) {
        self.table().remove(key);
    }

    /// The keys watched, with their paths.
    pub fn entries(&self) -> Vec<(String, PathBuf)> {
        self.table().iter().map(|(k, e)| (k.clone(), e.path.clone())).collect()
    }

    /// Looks at every file once and returns what changed. The files are read outside the lock,
    /// so the UI thread never waits on a disk; an entry removed or re-added meanwhile is left as
    /// the core set it.
    pub fn poll(&self) -> Vec<(String, Observed)> {
        let snapshot: Vec<(String, PathBuf)> = self.entries();
        let looks: Vec<(String, PathBuf, Observed)> = snapshot
            .into_iter()
            .filter_map(|(key, path)| look(&path).map(|seen| (key, path, seen)))
            .collect();
        let mut table = self.table();
        let mut changed = Vec::new();
        for (key, path, seen) in looks {
            let Some(entry) = table.get_mut(&key) else { continue };
            if entry.path != path || entry.seen.as_ref() == Some(&seen) {
                continue;
            }
            entry.seen = Some(seen.clone());
            changed.push((key, seen));
        }
        changed
    }
}
