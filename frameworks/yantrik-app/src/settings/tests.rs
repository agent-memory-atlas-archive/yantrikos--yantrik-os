//! The store against real files: revisions, migration, recovery, outside edits, and the mark
//! that keeps it off every file that is not its own.

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

const APP: &str = "t";

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("yantrik-settings-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A file as this framework writes it for `APP`.
fn marked(schema: u32, settings: &str) -> String {
    format!(r#"{{"kind":"{}","app":"{APP}","schema":{schema},"settings":{settings}}}"#, file::KIND)
}

fn open(path: &Path) -> Store<Prefs> {
    Store::<Prefs>::open(path.to_path_buf(), APP)
}

#[test]
fn missing_is_defaults_and_a_save_commits_by_revision() {
    let path = dir("missing").join("app.json");
    let mut store = open(&path);
    assert_eq!(store.get(), &Prefs::default());
    assert!(!store.is_pending());
    let r1 = store.desire(Prefs { step: 2, unit: "c".into() }).unwrap();
    let r2 = store.desire(Prefs { step: 3, unit: "c".into() }).unwrap();
    assert!(r2 > r1);
    let (rev, bytes) = store.bytes_to_write().unwrap();
    assert_eq!(rev, r2);
    assert!(file::is_marked(&bytes, APP), "every write carries the mark");
    write_atomically(&path, &bytes).unwrap();
    store.committed_at(r1);
    assert!(store.is_pending(), "an older write landing changes nothing");
    store.committed_at(r2);
    assert!(!store.is_pending());
    assert_eq!(store.committed().step, 3);
    assert!(!store.changed_outside(), "its own write is not an outside change");
    assert!(matches!(open(&path).get(), Prefs { step: 3, .. }), "and reads back");
}

#[test]
fn an_older_schema_is_migrated_and_owed_a_rewrite() {
    let path = dir("older").join("app.json");
    std::fs::write(&path, marked(1, r#"{"step":5}"#)).unwrap();
    let store = open(&path);
    assert_eq!(store.get(), &Prefs { step: 5, unit: "c".into() });
    assert!(store.is_pending(), "the migrated value is written once at the new schema");
    assert!(!store.in_recovery());
}

#[test]
fn a_corrupt_or_newer_file_is_recovery_and_is_set_aside_only_when_replaced() {
    let d = dir("corrupt");
    let path = d.join("app.json");
    std::fs::write(&path, "{not json").unwrap();
    let mut store = open(&path);
    assert!(store.in_recovery());
    assert_eq!(store.get(), &Prefs::default());
    store.desire(Prefs { step: 9, unit: "f".into() });
    assert!(store.bytes_to_write().is_none(), "nothing is written over an unusable file");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{not json");
    store.take_over();
    assert!(!path.exists(), "set aside");
    let kept: Vec<_> = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.path()).collect();
    assert_eq!(kept.len(), 1);
    assert_eq!(std::fs::read_to_string(&kept[0]).unwrap(), "{not json", "kept, as it was");
    assert!(kept[0].file_name().unwrap().to_string_lossy().starts_with("app.json.set-aside-"));
    assert!(store.bytes_to_write().is_some());
    std::fs::write(&path, marked(9, "{}")).unwrap();
    assert_eq!(open(&path).health(), &Health::Recovery(SettingsError::Newer { found: 9, known: 2 }));
}

#[test]
fn an_outside_edit_is_adopted_or_is_a_conflict_when_an_edit_was_pending() {
    let path = dir("outside").join("app.json");
    let mut store = open(&path);
    std::fs::write(&path, marked(2, r#"{"step":7,"unit":"c"}"#)).unwrap();
    assert!(store.changed_outside());
    let Reloaded::Adopted(adopted) = store.reload() else { panic!("adopted") };
    assert_eq!(adopted.step, 7);
    assert_eq!(store.get().step, 7);
    assert!(!store.changed_outside());
    assert_eq!(store.reload(), Reloaded::Unchanged);
    store.desire(Prefs { step: 8, unit: "c".into() });
    std::fs::write(&path, marked(2, r#"{"step":1,"unit":"c"}"#)).unwrap();
    let Reloaded::Conflict(conflict) = store.reload() else { panic!("conflict") };
    assert_eq!((conflict.outside.step, conflict.mine.step), (1, 8));
    let rev = store.resolve(conflict.mine.clone());
    assert_eq!(store.bytes_to_write().map(|(r, _)| r), Some(rev));
    std::fs::write(&path, "garbage").unwrap();
    assert_eq!(store.reload(), Reloaded::Unusable(SettingsError::NotOurs));
    assert!(store.in_recovery());
    assert_eq!(store.get().step, 8, "recovery keeps the values");
}

/// The app's own write, seen by the watcher before its outcome reached the core, is not an
/// outside change; and an outside edit landing between the write and its outcome is.
#[test]
fn the_apps_own_write_is_known_before_its_outcome_arrives() {
    let path = dir("echo").join("app.json");
    let mut store = open(&path);
    let rev = store.desire(Prefs { step: 4, unit: "c".into() }).unwrap();
    let (_, bytes) = store.bytes_to_write().unwrap();
    write_atomically(&path, &bytes).unwrap();
    // The watcher polls now, before `committed_at`.
    assert_eq!(store.reload(), Reloaded::Unchanged, "its own bytes, in flight");
    assert!(store.is_pending());
    // An outside edit lands before the outcome is handled.
    std::fs::write(&path, marked(2, r#"{"step":6,"unit":"c"}"#)).unwrap();
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
    let mut store = open(&path);
    store.desire(Prefs { step: 9, unit: "f".into() });
    assert!(store.in_recovery() && store.is_pending());
    std::fs::write(&path, marked(2, r#"{"step":2,"unit":"c"}"#)).unwrap();
    let Reloaded::Adopted(repaired) = store.reload() else { panic!("the repair is adopted, not a conflict") };
    assert_eq!(repaired.step, 2);
    assert!(!store.in_recovery() && !store.is_pending());
    assert_eq!(store.get().step, 2);
}

/// A write queued before the file of its own went bad lands over it: the file is the app's again.
#[test]
fn a_write_of_our_own_landing_ends_recovery() {
    let path = dir("unstick").join("app.json");
    let mut store = open(&path);
    let rev = store.desire(Prefs { step: 3, unit: "c".into() }).unwrap();
    let (_, bytes) = store.bytes_to_write().unwrap();
    std::fs::write(&path, marked(2, r#""garbage""#)).unwrap();
    assert!(matches!(store.reload(), Reloaded::Unusable(SettingsError::Malformed(_))));
    assert_eq!(file::may_replace(&path, &bytes), Ok(()), "its own mark: the lane may replace it");
    write_atomically(&path, &bytes).unwrap();
    store.committed_at(rev);
    assert!(!store.in_recovery(), "the file is ours again");
    assert!(!store.changed_outside());
    assert!(store.desire(Prefs { step: 5, unit: "c".into() }).is_some());
    assert!(store.bytes_to_write().is_some(), "saves flow again");
}

#[test]
fn a_malformed_file_is_described_without_its_values() {
    let path = dir("secret").join("app.json");
    std::fs::write(&path, marked(2, r#"{"step":"sk-SECRET-TOKEN","unit":"c"}"#)).unwrap();
    let store = open(&path);
    let Health::Recovery(SettingsError::Malformed(said)) = store.health() else { panic!("recovery") };
    assert!(!said.contains("SECRET"), "{said}");
    assert!(said.contains("wrong kind"), "{said}");
}

/// Another component's file where the settings go (email-service's account list, password and
/// all), a file of another app's, an envelope from before the mark, a link: none is this app's.
/// None is read into the model, none is written over by a save, and nothing of one is said.
#[test]
fn a_file_without_this_apps_mark_is_never_read_or_written_over() {
    let d = dir("not-ours");
    let path = d.join("app.json");
    let account = r#"[{"id":"default","email":"p@example.org","password":"SENTINEL-PASSWORD-1"}]"#;
    let other_app = format!(r#"{{"kind":"{}","app":"other","schema":2,"settings":{{"step":1,"unit":"SENTINEL-PASSWORD-1"}}}}"#, file::KIND);
    let unmarked = r#"{"schema":2,"settings":{"step":1,"unit":"SENTINEL-PASSWORD-1"}}"#;
    for content in [account, other_app.as_str(), unmarked, r#"{"step":1,"unit":"SENTINEL-PASSWORD-1"}"#] {
        std::fs::write(&path, content).unwrap();
        let mut store = open(&path);
        assert_eq!(store.health(), &Health::Recovery(SettingsError::NotOurs), "{content}");
        assert_eq!(store.get(), &Prefs::default(), "nothing of it reaches the model");
        assert!(!store.health_sentence().contains("SENTINEL"));
        store.desire(Prefs { step: 4, unit: "c".into() });
        assert!(store.bytes_to_write().is_none(), "a save does not write over it");
        let ours = render(APP, &Prefs { step: 4, unit: "c".into() }).unwrap();
        let refused = file::may_replace(&path, &ours).unwrap_err();
        assert!(!refused.contains("SENTINEL") && refused.contains("left as it is"), "{refused}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), content, "untouched");
    }
    #[cfg(unix)]
    {
        let target = d.join("email.json");
        std::fs::write(&target, account).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert_eq!(open(&path).health(), &Health::Recovery(SettingsError::NotOurs), "a link is not followed");
        let ours = render(APP, &Prefs::default()).unwrap();
        assert!(file::may_replace(&path, &ours).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), account);
    }
}

/// The mark is looked for and nothing else: a document whose values are not settings at all
/// still answers only "not ours", and the answer is the same whatever the values were.
#[test]
fn looking_for_the_mark_keeps_nothing_of_the_file() {
    assert!(!file::is_marked(br#"[1,2,3]"#, APP));
    assert!(!file::is_marked(br#"{"kind":7}"#, APP));
    assert!(!file::is_marked(b"\x00\x01", APP));
    assert!(file::is_marked(marked(2, r#"{"deep":[[[{"x":"SENTINEL"}]]]}"#).as_bytes(), APP));
    assert!(!file::is_marked(marked(2, "{}").as_bytes(), "another"));
}

impl<T: Settings> Store<T> {
    /// What the app, the log and the problem record are told about the file.
    fn health_sentence(&self) -> String {
        match &self.health {
            Health::Fine => String::new(),
            Health::Recovery(e) => format!("{e} {e:?}"),
        }
    }
}
