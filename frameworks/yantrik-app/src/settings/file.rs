//! The settings file on disk: the mark that makes it this framework's, reading it, and the bytes
//! a save writes (design/app-framework-2026-10-21.md, §6).
//!
//! The file is `{"kind": "yantrik-app-settings", "app": "<id>", "schema": n, "settings": {…}}`.
//! The `kind` and the `app` are the mark. A file without them, or with another app's, is not this
//! app's, whatever else it holds: the store does not read it past the mark, does not write over
//! it, and says only its path. Looking for the mark materialises nothing else (every other value
//! is skipped unread by serde), so a secret in such a file never reaches the model, the log, a
//! problem record or the window. A link is never followed: the framework never makes one.

use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Settings, SettingsError};

/// The mark's `kind`.
pub const KIND: &str = "yantrik-app-settings";

/// No settings file is this large; a larger one is not read at all.
pub const MAX_BYTES: u64 = 1 << 20;

/// What a save writes.
#[derive(Serialize)]
struct Envelope<'a> {
    kind: &'a str,
    app: &'a str,
    schema: u32,
    settings: Value,
}

/// The mark alone. Every other field of the document is skipped, not kept.
#[derive(Deserialize)]
struct Mark {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    app: Option<String>,
}

/// The rest, read only once the mark is this app's.
#[derive(Deserialize)]
struct Stored {
    schema: u32,
    settings: Value,
}

/// What the file's content is: its length and a hash of its bytes. Content, not the modification
/// time: an edit that keeps the size and the time (an editor that preserves mtime, a coarse
/// filesystem clock) is still an edit, and a rewrite with the same bytes is not one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fingerprint {
    len: u64,
    hash: u64,
}

impl Fingerprint {
    /// The file's fingerprint now, or `None` when it is absent or unreadable.
    pub fn of(path: &Path) -> Option<Fingerprint> {
        let bytes = std::fs::read(path).ok()?;
        Some(Fingerprint::of_bytes(&bytes))
    }

    /// The fingerprint the file will have once these bytes are in it.
    pub fn of_bytes(bytes: &[u8]) -> Fingerprint {
        Fingerprint { len: bytes.len() as u64, hash: hash(bytes) }
    }
}

fn hash(bytes: &[u8]) -> u64 {
    // FNV-1a: small, dependency-free, and this is a change detector, not a security hash.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// What `read_file` made of the file.
pub enum Opened<T: Settings> {
    /// The settings as read (migrated if they were older), and whether a rewrite is owed.
    Read { value: T, rewrite: bool },
    /// No file: the defaults, nothing written.
    Missing,
    /// The file is not usable, or not this app's; the defaults, in recovery.
    Recovery(SettingsError),
}

/// A serde error without the values it quotes: a settings file may hold a token, and this
/// sentence goes to the log, a problem record and the window. The kind and the place stay.
pub fn describe(e: &serde_json::Error) -> String {
    use serde_json::error::Category;
    let kind = match e.classify() {
        Category::Io => "could not be read",
        Category::Syntax => "is not valid JSON",
        Category::Eof => "ends early",
        Category::Data => "has a value of the wrong kind",
    };
    if e.line() > 0 {
        format!("{kind} (line {}, column {})", e.line(), e.column())
    } else {
        kind.to_string()
    }
}

/// Whether `bytes` carry `app`'s mark. Nothing but the mark is kept from them.
pub fn is_marked(bytes: &[u8], app: &str) -> bool {
    serde_json::from_slice::<Mark>(bytes)
        .is_ok_and(|m| m.kind.as_deref() == Some(KIND) && m.app.as_deref() == Some(app))
}

/// The bytes at `path`: `Ok(None)` when there is nothing there, `Err(NotOurs)` for a link, a
/// directory or a file too large to be settings, none of which is read.
pub(crate) fn read_bounded(path: &Path) -> Result<Option<Vec<u8>>, SettingsError> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(SettingsError::Unreadable(e.to_string())),
    };
    if !meta.is_file() || meta.len() > MAX_BYTES {
        return Err(SettingsError::NotOurs);
    }
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(SettingsError::Unreadable(e.to_string())),
    }
}

/// Reads and migrates `app`'s file at `path`, without touching it. A file without `app`'s mark
/// is `NotOurs`, and nothing of it past the mark is read.
pub fn read_file<T: Settings>(path: &Path, app: &str) -> Opened<T> {
    let bytes = match read_bounded(path) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return Opened::Missing,
        Err(e) => return Opened::Recovery(e),
    };
    if !is_marked(&bytes, app) {
        return Opened::Recovery(SettingsError::NotOurs);
    }
    let stored: Stored = match serde_json::from_slice(&bytes) {
        Ok(stored) => stored,
        Err(e) => return Opened::Recovery(SettingsError::Malformed(describe(&e))),
    };
    if stored.schema > T::SCHEMA {
        return Opened::Recovery(SettingsError::Newer { found: stored.schema, known: T::SCHEMA });
    }
    let rewrite = stored.schema < T::SCHEMA;
    let raw = if rewrite {
        match T::migrate(stored.schema, stored.settings) {
            Ok(raw) => raw,
            Err(e) => return Opened::Recovery(e),
        }
    } else {
        stored.settings
    };
    match parse::<T>(raw) {
        Ok(value) => Opened::Read { value, rewrite },
        Err(e) => Opened::Recovery(e),
    }
}

fn parse<T: DeserializeOwned>(raw: Value) -> Result<T, SettingsError> {
    serde_json::from_value::<T>(raw).map_err(|e| SettingsError::Malformed(describe(&e)))
}

/// The bytes the file holds for `app`'s `value`.
pub fn render<T: Settings>(app: &str, value: &T) -> Result<Vec<u8>, SettingsError> {
    let settings = serde_json::to_value(value).map_err(|e| SettingsError::Write(e.to_string()))?;
    render_raw(app, T::SCHEMA, settings)
}

/// The bytes for settings already in their document form, at `schema` (an import from before).
pub(crate) fn render_raw(app: &str, schema: u32, settings: Value) -> Result<Vec<u8>, SettingsError> {
    let envelope = Envelope { kind: KIND, app, schema, settings };
    serde_json::to_vec_pretty(&envelope).map_err(|e| SettingsError::Write(e.to_string()))
}

/// Whether a save of `bytes` may replace what is at `path`: nothing, or a file with the same mark
/// the bytes carry. The lane asks just before it writes, so a file that arrived after the save
/// was queued, and is not this app's, is still not written over. The answer is a sentence with
/// the path and nothing of the file.
pub fn may_replace(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let refused = || format!("{} is not a settings file this app wrote; it is left as it is", path.display());
    // Only bytes the store rendered, with a mark of their own, are a settings save.
    let app = match serde_json::from_slice::<Mark>(bytes) {
        Ok(Mark { kind: Some(kind), app: Some(app) }) if kind == KIND && !app.is_empty() => app,
        _ => return Err(refused()),
    };
    match read_bounded(path) {
        Ok(None) => Ok(()),
        Ok(Some(existing)) if is_marked(&existing, &app) => Ok(()),
        Ok(Some(_)) | Err(SettingsError::NotOurs) => Err(refused()),
        Err(e) => Err(e.to_string()),
    }
}

/// Moves the file at `path` aside, to `<file>.set-aside-<seconds>.json` beside it, so a save can
/// take its place: kept, never deleted, never overwritten. `Ok(None)` when there is nothing there.
pub fn set_aside(path: &Path) -> std::io::Result<Option<PathBuf>> {
    if std::fs::symlink_metadata(path).is_err() {
        return Ok(None);
    }
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    for n in 0..1000u32 {
        let suffix = if n == 0 { String::new() } else { format!("-{n}") };
        let kept = path.with_file_name(format!("{name}.set-aside-{secs}{suffix}.json"));
        if std::fs::symlink_metadata(&kept).is_ok() {
            continue;
        }
        std::fs::rename(path, &kept)?;
        return Ok(Some(kept));
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "no free name to set the file aside under"))
}
