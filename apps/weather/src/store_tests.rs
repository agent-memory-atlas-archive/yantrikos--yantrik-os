//! The places and the unit, through a real file and a fresh start (ported from the retired
//! `tests/weather-core`, which tested the old `state.rs` the framework replaced).
//!
//! The fault these cover is the one the audit found: a save that existed, was correct, and was
//! never called, so a fresh load read back a file nobody wrote and the round trip only looked
//! whole. An in-memory assertion passes on that broken code too; only a real file read by a
//! fresh start would have caught it. So every test here starts the app, changes something, shuts
//! it down the way the runner does, and starts it again at the same paths.
//!
//! Nothing here reaches a geocoder: a place arrives as the lookup's answer (`Command::Added`).

use std::sync::Arc;

use yantrik_app::mind::fake::FakeMind;
use yantrik_app::{App as _, Paths, Services};
use yantrik_testing::TestApp;

use crate::prefs::{Prefs, SavedLocation};
use crate::{places, Command, Weather};

fn start(paths: &Paths) -> TestApp<Weather> {
    TestApp::<Weather>::start_at_with(paths.clone(), Vec::new(), Arc::new(FakeMind::new()), Services::fakes())
}

/// The app's whole life at `paths`: what was saved is written by the shutdown, as the runner's is.
fn restart(app: TestApp<Weather>, paths: &Paths) -> TestApp<Weather> {
    let _ = app.shutdown();
    drop(app);
    start(paths)
}

fn place(name: &str, lat: f64, lon: f64) -> SavedLocation {
    SavedLocation { name: name.into(), lat, lon }
}

fn add(app: &TestApp<Weather>, loc: SavedLocation) {
    app.run(Command::Added { query: loc.name.clone(), outcome: Ok(loc) });
}

fn prefs(app: &TestApp<Weather>) -> Prefs {
    app.model(|m| m.prefs.clone())
}

fn names(p: &Prefs) -> Vec<String> {
    p.locations.iter().map(|l| l.name.clone()).collect()
}

#[test]
fn a_saved_place_and_the_place_being_shown_survive_a_fresh_start() {
    let paths = Paths::temporary(Weather::ID);
    let app = start(&paths);
    assert_eq!(names(&prefs(&app)), vec![SavedLocation::london().name], "a first run starts with the default place");
    add(&app, place("Paris, France", 48.8566, 2.3522));
    add(&app, place("Tokyo, Japan", 35.6895, 139.6917));
    app.run(Command::SelectLocation(1));
    let app = restart(app, &paths);
    let p = prefs(&app);
    assert_eq!(names(&p), vec![SavedLocation::london().name, "Paris, France".into(), "Tokyo, Japan".into()]);
    assert_eq!(p.active, 1);
    assert_eq!(p.active_location().name, "Paris, France");
    assert!((p.active_location().lat - 48.8566).abs() < 1e-9);
}

#[test]
fn the_unit_choice_survives_a_fresh_start() {
    let paths = Paths::temporary(Weather::ID);
    let app = start(&paths);
    assert!(!prefs(&app).fahrenheit, "celsius is the default");
    app.run(Command::SetUnits(true));
    let app = restart(app, &paths);
    assert!(prefs(&app).fahrenheit);
    app.run(Command::SetUnits(false));
    let app = restart(app, &paths);
    assert!(!prefs(&app).fahrenheit);
}

#[test]
fn a_missing_or_damaged_file_starts_on_the_defaults_and_is_left_as_it_is() {
    let paths = Paths::temporary(Weather::ID);
    let fresh = start(&paths);
    assert_eq!(names(&prefs(&fresh)), vec![SavedLocation::london().name]);
    assert!(!prefs(&fresh).fahrenheit);
    drop(fresh);
    let file = paths.config_file().to_path_buf();
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    for damage in ["", "{", "not json at all", "[1, 2, 3]", "{\"locations\": \"Paris\"}", "\u{0}\u{1}\u{2}"] {
        std::fs::write(&file, damage).unwrap();
        let app = start(&paths);
        let p = prefs(&app);
        assert_eq!(names(&p), vec![SavedLocation::london().name], "damaged as {damage:?}");
        assert_eq!(p.active, 0);
        // Read, not repaired: a change is held in memory and the file is not written over until
        // the person chooses to replace it.
        app.run(Command::SetUnits(true));
        app.run_writes();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), damage, "damaged as {damage:?}");
        let _ = app.shutdown();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), damage, "not even at shutdown: {damage:?}");
    }
}

#[test]
fn a_shown_place_past_the_end_of_the_list_does_not_panic() {
    let paths = Paths::temporary(Weather::ID);
    let file = paths.config_file().to_path_buf();
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(
        &file,
        r#"{"fahrenheit": true, "active": 9, "locations": [{"name": "Paris", "lat": 48.85, "lon": 2.35}]}"#,
    )
    .unwrap();
    let app = start(&paths);
    let p = prefs(&app);
    assert_eq!(p.active, 0);
    assert_eq!(p.active_location().name, "Paris");
    assert!(p.fahrenheit);
}

#[test]
fn a_save_into_a_place_that_cannot_hold_it_is_said_and_never_lands() {
    let paths = Paths::temporary(Weather::ID);
    // A regular file where the store needs a directory: the same shape, from the app's side, as
    // a read-only home or a full disk.
    let dir = paths.config_file().parent().unwrap().to_path_buf();
    std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
    std::fs::write(&dir, "not a directory").unwrap();
    let app = start(&paths);
    app.run(Command::SetUnits(true));
    app.run_writes();
    assert!(
        app.model(|m| m.notice.starts_with("Could not keep the change")),
        "a failed save is said: {:?}",
        app.model(|m| m.notice.clone())
    );
    assert_eq!(std::fs::read_to_string(&dir).unwrap(), "not a directory", "nothing was written over");
    // What was not written is not there after a restart: the app does not pretend.
    let app = restart(app, &paths);
    assert!(!prefs(&app).fahrenheit);
    let _ = std::fs::remove_file(&dir);
}

#[test]
fn a_save_leaves_the_prefs_file_and_nothing_else() {
    let paths = Paths::temporary(Weather::ID);
    let app = start(&paths);
    add(&app, place("Paris, France", 48.8566, 2.3522));
    app.run(Command::SetUnits(true));
    app.run(Command::SelectLocation(0));
    app.run_writes();
    // The atomic write's temp file is renamed, never left behind: a stray one beside the config
    // is litter, and one that survived a crash would be mistaken for the config.
    let dir = paths.config_file().parent().unwrap();
    let mut beside: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    beside.sort();
    assert_eq!(beside, vec!["weather.json".to_string()]);
    // And what landed is the whole document, not a truncated one.
    let text = std::fs::read_to_string(paths.config_file()).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(doc["settings"]["fahrenheit"], true);
    assert_eq!(doc["settings"]["locations"].as_array().unwrap().len(), 2);
}

#[test]
fn the_same_place_under_another_name_is_refused_and_nothing_is_written() {
    let paths = Paths::temporary(Weather::ID);
    let app = start(&paths);
    add(&app, place("Paris, France", 48.8566, 2.3522));
    // The geocoder answers the same query with "Paris" one time and "Paris, France" the next,
    // so the name is the wrong key; the coordinates are not.
    add(&app, place("Paris", 48.8570, 2.3519));
    assert!(app.model(|m| m.notice.contains("already saved")), "{}", app.model(|m| m.notice.clone()));
    assert_eq!(prefs(&app).locations.len(), 2);
    let app = restart(app, &paths);
    assert_eq!(prefs(&app).locations.len(), 2, "and nothing was written either");
    // A different city is still a different city.
    add(&app, place("Tokyo, Japan", 35.6895, 139.6917));
    let app = restart(app, &paths);
    assert_eq!(prefs(&app).locations.len(), 3);
}

#[test]
fn removing_a_place_keeps_the_place_being_shown_and_survives_a_fresh_start() {
    let paths = Paths::temporary(Weather::ID);
    let app = start(&paths);
    add(&app, place("Paris, France", 48.8566, 2.3522));
    add(&app, place("Tokyo, Japan", 35.6895, 139.6917));
    app.run(Command::SelectLocation(2));
    // Removing a row above the shown one shifts it down; the same place stays on screen.
    app.run(Command::RemoveLocation(0));
    assert_eq!(prefs(&app).active_location().name, "Tokyo, Japan");
    let app = restart(app, &paths);
    assert_eq!(prefs(&app).active_location().name, "Tokyo, Japan");
    // Removing the shown one falls back to a neighbour rather than to nothing.
    app.run(Command::RemoveLocation(1));
    assert_eq!(prefs(&app).active_location().name, "Paris, France");
    let app = restart(app, &paths);
    assert_eq!(prefs(&app).locations.len(), 1);
}

#[test]
fn the_last_place_and_a_row_that_is_not_there_are_refused_by_name() {
    let mut p = Prefs::default();
    let e = places::remove(&mut p, 0).unwrap_err();
    assert!(e.sentence.contains("last saved location"), "{}", e.sentence);
    assert_eq!(p.locations.len(), 1);
    let e = places::remove(&mut p, 7).unwrap_err();
    assert!(e.sentence.contains("no saved location"), "{}", e.sentence);
    let e = places::select(&mut p, 7).unwrap_err();
    assert!(e.sentence.contains("no saved location"), "{}", e.sentence);
    assert_eq!(p.active, 0);
}
