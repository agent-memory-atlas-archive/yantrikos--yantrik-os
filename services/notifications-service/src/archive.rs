//! Where acknowledged notices go instead of being deleted, and the two disk calls that make
//! "stored" mean "on the disk".
//!
//! The live store drops a dismissed notification a week after it was made, and the oldest past
//! its cap. For a mind's notice that would delete something the OS answered `stored` for
//! (design/initiative-inbox-2026-10-16.md: "archive, never delete"). So a notice leaving the live
//! file is appended here first, one JSON record per line, and flushed before it goes.

use std::io::Write;
use std::path::{Path, PathBuf};

use yantrik_ipc_contracts::notifications::Notification;

/// `notices-archive.jsonl`, beside the store.
pub fn path_beside(store: &Path) -> PathBuf {
    store.with_file_name("notices-archive.jsonl")
}

/// Append these records, one per line, and flush them to the disk. Nothing to append is not a
/// write at all.
pub fn append<'a>(path: &Path, records: impl Iterator<Item = &'a Notification>) -> std::io::Result<()> {
    let mut lines = String::new();
    for record in records {
        let line = serde_json::to_string(record).map_err(std::io::Error::other)?;
        lines.push_str(&line);
        lines.push('\n');
    }
    if lines.is_empty() {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(lines.as_bytes())?;
    file.sync_all()?;
    sync_dir(path);
    Ok(())
}

/// Write `bytes` to `path` and flush them to the disk before answering.
pub fn write_synced(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = std::fs::File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Flush the directory holding `path`, so a rename or a new file in it is on the disk too. Best
/// effort: not every filesystem lets a directory be opened for this.
pub fn sync_dir(path: &Path) {
    if let Some(dir) = path.parent() {
        if let Ok(handle) = std::fs::File::open(dir) {
            let _ = handle.sync_all();
        }
    }
}
