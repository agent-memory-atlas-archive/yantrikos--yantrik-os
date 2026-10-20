//! An app's settings: typed, versioned, and never lost (design/app-framework-2026-10-21.md, §6).
//!
//! The store owns the *committed* value: what is durably on disk, by revision. The app's model
//! holds the value it is using. A change goes through a command (`fx.save_settings`), the
//! write goes through the ordered persistence lane, and the result comes back as an event. A
//! failed write keeps the desired value in memory and says so; it never rolls the model back.
//!
//! The file is `{"schema": n, "settings": {…}}`. A missing file is the defaults, unwritten. An
//! older schema is migrated and rewritten once. A file that is malformed, or from a newer
//! schema than this app knows, puts the store in **recovery**: the app runs on the defaults, a
//! problem record is filed, and an ordinary save does not overwrite that file until the person
//! chooses to (`Store::take_over`).

use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

/// What an app's settings must be: plain data with a schema number.
pub trait Settings:
    Serialize + DeserializeOwned + Default + Clone + PartialEq + Send + Sync + 'static
{
    /// Bumped when the shape changes; `migrate` is given every older version.
    const SCHEMA: u32;

    /// The document of an older schema, brought to this one. The default accepts it as is.
    fn migrate(from: u32, raw: Value) -> Result<Value, SettingsError> {
        let _ = from;
        Ok(raw)
    }
}

/// An app with nothing to remember: `type Settings = ();`.
impl Settings for () {
    const SCHEMA: u32 = 0;
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SettingsError {
    #[error("the settings file could not be read: {0}")]
    Unreadable(String),
    #[error("the settings file is not in a shape this app reads: {0}")]
    Malformed(String),
    #[error("the settings file is from a newer version of this app (schema {found}, this app knows {known})")]
    Newer { found: u32, known: u32 },
    #[error("the settings could not be migrated from schema {from}: {reason}")]
    Migration { from: u32, reason: String },
    #[error("the settings could not be written: {0}")]
    Write(String),
}

/// Why the file on disk is left alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Health {
    /// The file is this app's, or there is none yet.
    Fine,
    /// The file could not be used: the app runs on the defaults and will not overwrite it.
    Recovery(SettingsError),
}

/// A revision of the settings: monotonic within a run, so an older write's outcome never
/// replaces a newer state.
pub type Revision = u64;

/// The store: the committed value, the desired one, and what the file is worth.
pub struct Store<T: Settings> {
    path: PathBuf,
    committed: T,
    desired: T,
    revision: Revision,
    committed_revision: Revision,
    health: Health,
    /// The file as last read or written, so an outside change is told from this app's own echo.
    fingerprint: Option<Fingerprint>,
}

/// What the file looked like: its size, modification time and a hash of its bytes. Content is
/// hashed because an edit that keeps the size and the time (an editor that preserves mtime,
/// a coarse filesystem clock) is still an edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    len: u64,
    modified: Option<std::time::SystemTime>,
    hash: u64,
}

impl Fingerprint {
    /// The file's fingerprint now, or `None` when it is absent or unreadable.
    pub fn of(path: &Path) -> Option<Fingerprint> {
        let meta = std::fs::metadata(path).ok()?;
        let bytes = std::fs::read(path).ok()?;
        Some(Fingerprint { len: meta.len(), modified: meta.modified().ok(), hash: hash(&bytes) })
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

/// The on-disk envelope.
#[derive(Serialize, serde::Deserialize)]
struct Envelope {
    schema: u32,
    settings: Value,
}

/// What `open` made of the file.
pub enum Opened<T: Settings> {
    /// The settings as read (migrated if they were older), and whether a rewrite is owed.
    Read { value: T, rewrite: bool },
    /// No file: the defaults, nothing written.
    Missing,
    /// The file is not usable; the defaults, in recovery.
    Recovery(SettingsError),
}

/// Reads and migrates the file at `path`, without touching it.
pub fn read_file<T: Settings>(path: &Path) -> Opened<T> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Opened::Missing,
        Err(e) => return Opened::Recovery(SettingsError::Unreadable(e.to_string())),
    };
    let envelope: Envelope = match serde_json::from_str(&text) {
        Ok(e) => e,
        Err(e) => return Opened::Recovery(SettingsError::Malformed(e.to_string())),
    };
    if envelope.schema > T::SCHEMA {
        return Opened::Recovery(SettingsError::Newer { found: envelope.schema, known: T::SCHEMA });
    }
    let rewrite = envelope.schema < T::SCHEMA;
    let raw = if rewrite {
        match T::migrate(envelope.schema, envelope.settings) {
            Ok(raw) => raw,
            Err(e) => return Opened::Recovery(e),
        }
    } else {
        envelope.settings
    };
    match serde_json::from_value::<T>(raw) {
        Ok(value) => Opened::Read { value, rewrite },
        Err(e) => Opened::Recovery(SettingsError::Malformed(e.to_string())),
    }
}

/// The bytes the file holds for `value`.
pub fn render<T: Settings>(value: &T) -> Result<Vec<u8>, SettingsError> {
    let envelope = Envelope { schema: T::SCHEMA, settings: serde_json::to_value(value).map_err(|e| SettingsError::Write(e.to_string()))? };
    serde_json::to_vec_pretty(&envelope).map_err(|e| SettingsError::Write(e.to_string()))
}

impl<T: Settings> Store<T> {
    /// Opens the store at `path`. Never fails: a file that cannot be used leaves the store in
    /// recovery, which `health` says.
    pub fn open(path: PathBuf) -> Store<T> {
        let (committed, health, rewrite) = match read_file::<T>(&path) {
            Opened::Read { value, rewrite } => (value, Health::Fine, rewrite),
            Opened::Missing => (T::default(), Health::Fine, false),
            Opened::Recovery(e) => (T::default(), Health::Recovery(e), false),
        };
        let fingerprint = Fingerprint::of(&path);
        let mut store = Store {
            path,
            desired: committed.clone(),
            committed,
            revision: 0,
            committed_revision: 0,
            health,
            fingerprint,
        };
        if rewrite {
            // An older schema, migrated: owed one write at the new schema.
            store.revision = 1;
        }
        store
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// What the app should use: the latest value asked for, committed or not.
    pub fn get(&self) -> &T {
        &self.desired
    }

    pub fn committed(&self) -> &T {
        &self.committed
    }

    pub fn health(&self) -> &Health {
        &self.health
    }

    pub fn in_recovery(&self) -> bool {
        matches!(self.health, Health::Recovery(_))
    }

    /// Whether a desired value has not reached the disk.
    pub fn is_pending(&self) -> bool {
        self.revision != self.committed_revision
    }

    /// A new desired value. Returns the revision a write of it carries, or `None` when nothing
    /// changed. In recovery, the value is held in memory and not written (see `take_over`).
    pub fn desire(&mut self, value: T) -> Option<Revision> {
        if value == self.desired && !self.is_pending() {
            return None;
        }
        self.desired = value;
        self.revision += 1;
        Some(self.revision)
    }

    /// The bytes to write for the desired value at `revision`, unless recovery forbids it.
    pub fn bytes_to_write(&self) -> Option<(Revision, Vec<u8>)> {
        if self.in_recovery() || !self.is_pending() {
            return None;
        }
        render(&self.desired).ok().map(|bytes| (self.revision, bytes))
    }

    /// The write of `revision` landed. Only the newest revision moves the committed value; an
    /// older write's success is nothing.
    pub fn committed_at(&mut self, revision: Revision) {
        if revision > self.committed_revision {
            self.committed_revision = revision;
            if revision == self.revision {
                self.committed = self.desired.clone();
            }
            self.fingerprint = Fingerprint::of(&self.path);
        }
    }

    /// The person chose to replace an unusable file with this app's settings: recovery ends and
    /// the desired value is owed a write.
    pub fn take_over(&mut self) {
        if self.in_recovery() {
            self.health = Health::Fine;
            self.revision += 1;
        }
    }

    /// Whether the file changed under this app since it last read or wrote it.
    pub fn changed_outside(&self) -> bool {
        Fingerprint::of(&self.path) != self.fingerprint
    }

    /// Re-reads a file that changed outside this app.
    pub fn reload(&mut self) -> Reloaded<T> {
        let now = Fingerprint::of(&self.path);
        if now == self.fingerprint {
            return Reloaded::Unchanged;
        }
        let outside = match read_file::<T>(&self.path) {
            Opened::Read { value, .. } => value,
            Opened::Missing => T::default(),
            Opened::Recovery(e) => {
                self.health = Health::Recovery(e.clone());
                self.fingerprint = now;
                return Reloaded::Unusable(e);
            }
        };
        self.fingerprint = now;
        self.health = Health::Fine;
        if self.is_pending() && outside != self.desired {
            return Reloaded::Conflict(Conflict { outside, mine: self.desired.clone() });
        }
        self.committed = outside.clone();
        self.committed_revision = self.revision;
        self.desired = outside.clone();
        Reloaded::Adopted(outside)
    }

    /// Resolves a conflict the app's way: `value` becomes desired and is owed a write.
    pub fn resolve(&mut self, value: T) -> Revision {
        self.desired = value;
        self.revision += 1;
        self.revision
    }
}

/// An outside edit landed while this app had a pending edit of its own.
#[derive(Debug, Clone, PartialEq)]
pub struct Conflict<T> {
    pub outside: T,
    pub mine: T,
}

/// What a re-read of the file came to.
#[derive(Debug, Clone, PartialEq)]
pub enum Reloaded<T> {
    /// The file is as this app last read or wrote it.
    Unchanged,
    /// The outside value, now committed and desired.
    Adopted(T),
    /// This app had a pending edit of its own; nothing was changed, the app decides.
    Conflict(Conflict<T>),
    /// The file is no longer usable; the store is in recovery and keeps its values.
    Unusable(SettingsError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use yantrik_foundation::json_store::write_atomically;

    #[derive(Debug, Clone, PartialEq, Default, Serialize, serde::Deserialize)]
    struct Prefs {
        step: i32,
        #[serde(default)]
        unit: String,
    }
    impl Settings for Prefs {
        const SCHEMA: u32 = 2;
        fn migrate(from: u32, mut raw: Value) -> Result<Value, SettingsError> {
            if from == 1 {
                raw["unit"] = Value::String("c".into());
            }
            Ok(raw)
        }
    }

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("yantrik-settings-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn missing_is_defaults_and_a_save_commits_by_revision() {
        let path = dir("missing").join("app.json");
        let mut store = Store::<Prefs>::open(path.clone());
        assert_eq!(store.get(), &Prefs::default());
        assert!(!store.is_pending());
        let r1 = store.desire(Prefs { step: 2, unit: "c".into() }).unwrap();
        let r2 = store.desire(Prefs { step: 3, unit: "c".into() }).unwrap();
        assert!(r2 > r1);
        let (rev, bytes) = store.bytes_to_write().unwrap();
        assert_eq!(rev, r2);
        write_atomically(&path, &bytes).unwrap();
        store.committed_at(r1);
        assert!(store.is_pending(), "an older write landing changes nothing");
        store.committed_at(r2);
        assert!(!store.is_pending());
        assert_eq!(store.committed().step, 3);
        assert!(!store.changed_outside(), "its own write is not an outside change");
    }

    #[test]
    fn an_older_schema_is_migrated_and_owed_a_rewrite() {
        let path = dir("older").join("app.json");
        std::fs::write(&path, r#"{"schema":1,"settings":{"step":5}}"#).unwrap();
        let store = Store::<Prefs>::open(path);
        assert_eq!(store.get(), &Prefs { step: 5, unit: "c".into() });
        assert!(store.is_pending(), "the migrated value is written once at the new schema");
        assert!(!store.in_recovery());
    }

    #[test]
    fn a_corrupt_or_newer_file_is_recovery_and_is_not_overwritten() {
        let path = dir("corrupt").join("app.json");
        std::fs::write(&path, "{not json").unwrap();
        let mut store = Store::<Prefs>::open(path.clone());
        assert!(store.in_recovery());
        assert_eq!(store.get(), &Prefs::default());
        store.desire(Prefs { step: 9, unit: "f".into() });
        assert!(store.bytes_to_write().is_none(), "nothing is written over an unusable file");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{not json");
        store.take_over();
        assert!(store.bytes_to_write().is_some());
        std::fs::write(&path, r#"{"schema":9,"settings":{}}"#).unwrap();
        let store = Store::<Prefs>::open(path);
        assert_eq!(store.health(), &Health::Recovery(SettingsError::Newer { found: 9, known: 2 }));
    }

    #[test]
    fn an_outside_edit_is_adopted_or_is_a_conflict_when_an_edit_was_pending() {
        let path = dir("outside").join("app.json");
        let mut store = Store::<Prefs>::open(path.clone());
        std::fs::write(&path, r#"{"schema":2,"settings":{"step":7,"unit":"c"}}"#).unwrap();
        assert!(store.changed_outside());
        let Reloaded::Adopted(adopted) = store.reload() else { panic!("adopted") };
        assert_eq!(adopted.step, 7);
        assert_eq!(store.get().step, 7);
        assert!(!store.changed_outside());
        assert_eq!(store.reload(), Reloaded::Unchanged);
        store.desire(Prefs { step: 8, unit: "c".into() });
        std::fs::write(&path, r#"{"schema":2,"settings":{"step":1,"unit":"c"}}"#).unwrap();
        let Reloaded::Conflict(conflict) = store.reload() else { panic!("conflict") };
        assert_eq!((conflict.outside.step, conflict.mine.step), (1, 8));
        let rev = store.resolve(conflict.mine.clone());
        assert_eq!(store.bytes_to_write().map(|(r, _)| r), Some(rev));
        std::fs::write(&path, "garbage").unwrap();
        assert!(matches!(store.reload(), Reloaded::Unusable(SettingsError::Malformed(_))));
        assert!(store.in_recovery());
        assert_eq!(store.get().step, 8, "recovery keeps the values");
    }
}
