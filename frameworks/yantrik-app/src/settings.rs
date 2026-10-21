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

use std::collections::BTreeMap;
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

    /// A file from before the framework: no `schema` envelope, the app's own shape. `Some` is
    /// the settings as this schema reads them; it is imported once, the original backed up
    /// beside it (`<file>.legacy.json`), and the file rewritten in the envelope. The default
    /// recognises nothing, so such a file is recovery.
    fn legacy(raw: Value) -> Option<Value> {
        let _ = raw;
        None
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
    /// The bytes handed to the lane for each revision not yet known to have landed. A file that
    /// reads as one of these is this app's own write seen early (the watcher polled between the
    /// rename and the outcome's arrival), not an outside edit.
    in_flight: BTreeMap<Revision, Fingerprint>,
}

/// What the file's content is: its length and a hash of its bytes. Content, not the modification
/// time: an edit that keeps the size and the time (an editor that preserves mtime, a coarse
/// filesystem clock) is still an edit, and a rewrite with the same bytes is not one. Serialised
/// in a document's draft, as the file the draft was written over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
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
    /// A file from before the framework, read through `Settings::legacy`: owed a backup and a
    /// rewrite in the envelope.
    Imported { value: T },
    /// No file: the defaults, nothing written.
    Missing,
    /// The file is not usable; the defaults, in recovery.
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

/// Reads and migrates the file at `path`, without touching it.
pub fn read_file<T: Settings>(path: &Path) -> Opened<T> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Opened::Missing,
        Err(e) => return Opened::Recovery(SettingsError::Unreadable(e.to_string())),
    };
    let envelope: Envelope = match serde_json::from_str(&text) {
        Ok(e) => e,
        Err(e) => {
            // Not the envelope. The app's own shape from before the framework, perhaps.
            if let Ok(raw) = serde_json::from_str::<Value>(&text) {
                if raw.as_object().is_some_and(|o| !o.contains_key("schema")) {
                    if let Some(legacy) = T::legacy(raw) {
                        if let Ok(value) = serde_json::from_value::<T>(legacy) {
                            return Opened::Imported { value };
                        }
                    }
                }
            }
            return Opened::Recovery(SettingsError::Malformed(describe(&e)));
        }
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
        Err(e) => Opened::Recovery(SettingsError::Malformed(describe(&e))),
    }
}

/// Keeps the file from before the framework beside the new one, once: `<file>.legacy.json`,
/// never overwritten, never deleted (design §6: imported once, backed up).
fn back_up_legacy(path: &Path) -> bool {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".legacy.json");
    let backup = path.with_file_name(name);
    if backup.exists() {
        return true;
    }
    match std::fs::copy(path, &backup) {
        Ok(_) => {
            tracing::info!(file = %path.display(), backup = %backup.display(), "settings from before the framework imported; the original is kept beside them");
            true
        }
        Err(e) => {
            tracing::warn!(file = %path.display(), error = %e, "the settings from before the framework could not be backed up; they are read, and the file is left as it is");
            false
        }
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
        // A temporary a killed process left beside the file is swept; nothing else would.
        yantrik_foundation::json_store::sweep_stale_temps(&path, std::time::Duration::from_secs(600));
        let (committed, health, rewrite) = match read_file::<T>(&path) {
            Opened::Read { value, rewrite } => (value, Health::Fine, rewrite),
            Opened::Imported { value } => {
                // The rewrite in the envelope is owed only once the original is kept beside it
                // (design §6: imported once, backed up); without the backup the bare file stays.
                let backed_up = back_up_legacy(&path);
                (value, Health::Fine, backed_up)
            }
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
            in_flight: BTreeMap::new(),
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

    /// The bytes to write for the desired value at `revision`, unless recovery forbids it. The
    /// bytes are remembered as in flight, so the file reading as them is this app's own write.
    pub fn bytes_to_write(&mut self) -> Option<(Revision, Vec<u8>)> {
        if self.in_recovery() || !self.is_pending() {
            return None;
        }
        let bytes = render(&self.desired).ok()?;
        self.in_flight.insert(self.revision, Fingerprint::of_bytes(&bytes));
        Some((self.revision, bytes))
    }

    /// The write of `revision` landed. Only the newest revision moves the committed value; an
    /// older write's success is nothing. The file is this app's now, whatever it was before: a
    /// write queued before the file went bad has replaced it with a good one, so recovery ends.
    pub fn committed_at(&mut self, revision: Revision) {
        let written = self.in_flight.remove(&revision);
        if revision > self.committed_revision {
            self.committed_revision = revision;
            if revision == self.revision {
                self.committed = self.desired.clone();
            }
            // What was written is known without reading the file back, so an outside edit that
            // lands between the write and this is not mistaken for the app's own.
            self.fingerprint = written.or_else(|| Fingerprint::of(&self.path));
            if self.in_recovery() {
                tracing::info!("a write of this app's own landed over the unusable settings file; recovery ends");
                self.health = Health::Fine;
            }
        }
        self.in_flight.retain(|r, _| *r > self.committed_revision);
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
        // The app's own write, seen by the watcher before its outcome reached the core.
        if now.as_ref().is_some_and(|n| self.in_flight.values().any(|f| f == n)) {
            return Reloaded::Unchanged;
        }
        let was_unusable = self.in_recovery();
        let outside = match read_file::<T>(&self.path) {
            Opened::Read { value, .. } | Opened::Imported { value } => value,
            Opened::Missing => T::default(),
            Opened::Recovery(e) => {
                self.health = Health::Recovery(e.clone());
                self.fingerprint = now;
                return Reloaded::Unusable(e);
            }
        };
        self.fingerprint = now;
        self.health = Health::Fine;
        // A file repaired by hand while the app held a value it could not save: the repair is
        // the person's choice (§6), and the held value gives way to it.
        if self.is_pending() && outside != self.desired && !was_unusable {
            return Reloaded::Conflict(Conflict { outside, mine: self.desired.clone() });
        }
        self.committed = outside.clone();
        self.committed_revision = self.revision;
        self.desired = outside.clone();
        self.in_flight.clear();
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

    /// The app's own write, seen by the watcher before its outcome reached the core, is not an
    /// outside change; and an outside edit landing between the write and its outcome is.
    #[test]
    fn the_apps_own_write_is_known_before_its_outcome_arrives() {
        let path = dir("echo").join("app.json");
        let mut store = Store::<Prefs>::open(path.clone());
        let rev = store.desire(Prefs { step: 4, unit: "c".into() }).unwrap();
        let (_, bytes) = store.bytes_to_write().unwrap();
        write_atomically(&path, &bytes).unwrap();
        // The watcher polls now, before `committed_at`.
        assert_eq!(store.reload(), Reloaded::Unchanged, "its own bytes, in flight");
        assert!(store.is_pending());
        // An outside edit lands before the outcome is handled.
        std::fs::write(&path, r#"{"schema":2,"settings":{"step":6,"unit":"c"}}"#).unwrap();
        store.committed_at(rev);
        assert!(store.changed_outside(), "the fingerprint is the bytes written, not the file read back");
        let Reloaded::Adopted(outside) = store.reload() else { panic!("adopted") };
        assert_eq!(outside.step, 6);
    }

    /// A file repaired by hand while the app held a value it could not save: the repair is the
    /// person's choice and is adopted; the held value gives way.
    #[test]
    fn a_repaired_file_is_adopted_over_a_value_held_in_recovery() {
        let path = dir("repair").join("app.json");
        std::fs::write(&path, "{not json").unwrap();
        let mut store = Store::<Prefs>::open(path.clone());
        store.desire(Prefs { step: 9, unit: "f".into() });
        assert!(store.in_recovery() && store.is_pending());
        std::fs::write(&path, r#"{"schema":2,"settings":{"step":2,"unit":"c"}}"#).unwrap();
        let Reloaded::Adopted(repaired) = store.reload() else { panic!("the repair is adopted, not a conflict") };
        assert_eq!(repaired.step, 2);
        assert!(!store.in_recovery() && !store.is_pending());
        assert_eq!(store.get().step, 2);
    }

    /// A write queued before the file went bad lands over it: the file is the app's again.
    #[test]
    fn a_write_of_our_own_landing_ends_recovery() {
        let path = dir("unstick").join("app.json");
        let mut store = Store::<Prefs>::open(path.clone());
        let rev = store.desire(Prefs { step: 3, unit: "c".into() }).unwrap();
        let (_, bytes) = store.bytes_to_write().unwrap();
        std::fs::write(&path, "garbage").unwrap();
        assert!(matches!(store.reload(), Reloaded::Unusable(_)));
        write_atomically(&path, &bytes).unwrap();
        store.committed_at(rev);
        assert!(!store.in_recovery(), "the file is ours again");
        assert!(!store.changed_outside());
        assert!(store.desire(Prefs { step: 5, unit: "c".into() }).is_some());
        assert!(store.bytes_to_write().is_some(), "saves flow again");
    }

    /// A file from before the framework (the app's own shape, no envelope) is imported once,
    /// backed up, and rewritten in the envelope.
    #[test]
    fn a_file_from_before_the_framework_is_imported_once_and_kept() {
        #[derive(Debug, Clone, PartialEq, Default, Serialize, serde::Deserialize)]
        struct Old {
            n: i32,
        }
        impl Settings for Old {
            const SCHEMA: u32 = 1;
            fn legacy(raw: Value) -> Option<Value> {
                raw.get("n").is_some().then_some(raw)
            }
        }
        let path = dir("legacy").join("app.json");
        std::fs::write(&path, r#"{"n": 7}"#).unwrap();
        let mut store = Store::<Old>::open(path.clone());
        assert_eq!(store.get(), &Old { n: 7 });
        assert!(!store.in_recovery());
        assert!(store.is_pending(), "owed the rewrite in the envelope");
        let backup = path.with_file_name("app.json.legacy.json");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), r#"{"n": 7}"#);
        let (rev, bytes) = store.bytes_to_write().unwrap();
        write_atomically(&path, &bytes).unwrap();
        store.committed_at(rev);
        assert!(std::fs::read_to_string(&path).unwrap().contains("\"schema\""));
        // A file of another shape without the envelope is still recovery.
        std::fs::write(&path, r#"{"other": 1}"#).unwrap();
        assert!(Store::<Old>::open(path).in_recovery());
    }

    #[test]
    fn a_malformed_file_is_described_without_its_values() {
        let path = dir("secret").join("app.json");
        std::fs::write(&path, r#"{"schema":2,"settings":{"step":"sk-SECRET-TOKEN","unit":"c"}}"#).unwrap();
        let store = Store::<Prefs>::open(path);
        let Health::Recovery(SettingsError::Malformed(said)) = store.health() else { panic!("recovery") };
        assert!(!said.contains("SECRET"), "{said}");
        assert!(said.contains("wrong kind"), "{said}");
    }
}
