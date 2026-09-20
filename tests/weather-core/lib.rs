//! Weather's store, tested without a desktop and without the network.
//!
//! The fault these cover is the one the audit found: `save()` existed, was correct, and was
//! never called, so `new()` read back a file nobody wrote and the round trip only looked whole.
//! A test that goes through a real file and a fresh load is the only shape that would have
//! caught it — an in-memory assertion passes on the broken code too.
//!
//! Nothing here reaches a geocoder. Turning a name into coordinates is the app's job and needs
//! a network; deciding whether that place is already saved, and keeping it, is this module's,
//! and it has to be checkable on a machine with no internet.

#[path = "../../apps/weather/src/state.rs"]
pub mod state;

#[cfg(test)]
mod tests {
    use super::state::{SavedLocation, WeatherState, DEFAULT_LOCATION_NAME};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static ID: AtomicUsize = AtomicUsize::new(0);

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "weather-test-{}-{}",
                std::process::id(),
                ID.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }

        /// The prefs file, under a directory the store has to create for itself — the real one
        /// is `~/.config/yantrik/weather.json` and `~/.config/yantrik` may not exist.
        fn prefs(&self) -> PathBuf {
            self.0.join("config/weather.json")
        }

        fn load(&self) -> WeatherState {
            WeatherState::load(self.prefs())
        }

        fn files_beside_the_prefs(&self) -> Vec<String> {
            let dir = self.prefs().parent().unwrap().to_path_buf();
            let mut names: Vec<String> = std::fs::read_dir(dir)
                .map(|entries| {
                    entries
                        .filter_map(|e| e.ok())
                        .map(|e| e.file_name().to_string_lossy().to_string())
                        .collect()
                })
                .unwrap_or_default();
            names.sort();
            names
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn paris() -> SavedLocation {
        SavedLocation { name: "Paris, France".into(), lat: 48.8566, lon: 2.3522 }
    }

    fn tokyo() -> SavedLocation {
        SavedLocation { name: "Tokyo, Japan".into(), lat: 35.6895, lon: 139.6917 }
    }

    #[test]
    fn a_saved_location_and_the_place_being_shown_survive_a_fresh_load() {
        let f = Fixture::new();
        let first = f.load();
        assert_eq!(first.locations().len(), 1, "a first run starts with the default location");
        assert_eq!(first.locations()[0].name, DEFAULT_LOCATION_NAME);

        first.add_location(paris()).unwrap();
        first.add_location(tokyo()).unwrap();
        first.select_location(1).unwrap();

        // The whole point: a different process reading the same file.
        let second = f.load();
        let names: Vec<String> = second.locations().iter().map(|l| l.name.clone()).collect();
        assert_eq!(names, vec![DEFAULT_LOCATION_NAME, "Paris, France", "Tokyo, Japan"]);
        assert_eq!(second.active_index(), 1);
        assert_eq!(second.active_location().name, "Paris, France");
        assert!((second.active_location().lat - 48.8566).abs() < 1e-9);
    }

    #[test]
    fn the_unit_choice_survives_a_fresh_load() {
        let f = Fixture::new();
        assert!(!f.load().is_fahrenheit(), "celsius is the default");

        f.load().set_fahrenheit(true).unwrap();
        assert!(f.load().is_fahrenheit());

        f.load().set_fahrenheit(false).unwrap();
        assert!(!f.load().is_fahrenheit());
    }

    #[test]
    fn a_missing_or_damaged_file_loads_as_the_defaults_without_panicking() {
        let f = Fixture::new();

        // Nothing there at all.
        let fresh = f.load();
        assert_eq!(fresh.locations().len(), 1);
        assert!(!fresh.is_fahrenheit());

        std::fs::create_dir_all(f.prefs().parent().unwrap()).unwrap();
        for damage in [
            "",
            "{",
            "not json at all",
            "[1, 2, 3]",
            "{\"locations\": \"Paris\"}",
            "\u{0}\u{1}\u{2}",
        ] {
            std::fs::write(f.prefs(), damage).unwrap();
            let s = f.load();
            assert_eq!(s.locations().len(), 1, "damaged as {damage:?}");
            assert_eq!(s.locations()[0].name, DEFAULT_LOCATION_NAME);
            assert_eq!(s.active_index(), 0);
            // Read, not repaired: the file is left as it is until the person makes a choice
            // that is worth writing over it.
            assert_eq!(std::fs::read_to_string(f.prefs()).unwrap(), damage);
        }
    }

    #[test]
    fn an_active_index_past_the_end_of_the_list_does_not_panic() {
        let f = Fixture::new();
        std::fs::create_dir_all(f.prefs().parent().unwrap()).unwrap();
        std::fs::write(
            f.prefs(),
            r#"{"fahrenheit": true, "active": 9,
                "locations": [{"name": "Paris", "lat": 48.85, "lon": 2.35}]}"#,
        )
        .unwrap();

        let s = f.load();
        assert_eq!(s.active_index(), 0);
        assert_eq!(s.active_location().name, "Paris");
        assert!(s.is_fahrenheit());
    }

    #[test]
    fn a_write_into_a_place_that_cannot_hold_it_is_reported_and_changes_nothing() {
        let f = Fixture::new();
        // A regular file where the store needs a directory: `create_dir_all` cannot make one,
        // which is the same shape as a read-only home or a full disk from the caller's side.
        let blocker = f.0.join("blocker");
        std::fs::write(&blocker, "not a directory").unwrap();
        let state = WeatherState::load(blocker.join("weather.json"));

        let refusal = state.add_location(paris()).unwrap_err();
        assert!(!refusal.is_empty(), "a failed save has to name a reason");

        // And the addition was taken back out, so what the app is holding and what is on disk
        // still agree. A city that is listed this session and gone after a restart is exactly
        // the failure this store exists to prevent.
        assert_eq!(state.locations().len(), 1);
        assert_eq!(state.locations()[0].name, DEFAULT_LOCATION_NAME);
        assert_eq!(state.active_index(), 0);

        assert!(state.set_fahrenheit(true).is_err());
        assert!(!state.is_fahrenheit(), "a unit choice that was not written is not held either");

        assert_eq!(std::fs::read_to_string(&blocker).unwrap(), "not a directory");
    }

    #[test]
    fn a_save_leaves_the_prefs_file_and_nothing_else() {
        let f = Fixture::new();
        let state = f.load();
        state.add_location(paris()).unwrap();
        state.set_fahrenheit(true).unwrap();
        state.select_location(0).unwrap();

        // The temp file the atomic write goes through is renamed, never left behind: a stray
        // `.weather.json.1234.tmp` beside the config is litter, and one that survived a crash
        // would be mistaken for the config by anyone reading the directory.
        assert_eq!(f.files_beside_the_prefs(), vec!["weather.json".to_string()]);

        // And what landed is the whole document, not a truncated one.
        let text = std::fs::read_to_string(f.prefs()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["fahrenheit"], true);
        assert_eq!(parsed["locations"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn the_same_place_under_another_name_is_refused_and_the_store_is_untouched() {
        let f = Fixture::new();
        let state = f.load();
        state.add_location(paris()).unwrap();

        // The geocoder answers the same query with "Paris" one time and "Paris, France" the
        // next, so the name is the wrong key; the coordinates are not.
        let again = SavedLocation { name: "Paris".into(), lat: 48.8570, lon: 2.3519 };
        let refusal = state.add_location(again).unwrap_err();
        assert!(refusal.contains("already saved"), "{refusal}");
        assert_eq!(state.locations().len(), 2);
        assert_eq!(f.load().locations().len(), 2, "and nothing was written either");

        // A different city at a nearby-looking latitude is still a different city.
        state.add_location(tokyo()).unwrap();
        assert_eq!(f.load().locations().len(), 3);
    }

    #[test]
    fn removing_a_location_keeps_the_place_being_shown_and_survives_a_load() {
        let f = Fixture::new();
        let state = f.load();
        state.add_location(paris()).unwrap();
        state.add_location(tokyo()).unwrap();
        state.select_location(2).unwrap();
        assert_eq!(state.active_location().name, "Tokyo, Japan");

        // Removing a row above the active one shifts it down; the same place stays on screen.
        let removed = state.remove_location(0).unwrap();
        assert_eq!(removed.name, DEFAULT_LOCATION_NAME);
        assert_eq!(state.active_location().name, "Tokyo, Japan");
        assert_eq!(f.load().active_location().name, "Tokyo, Japan");

        // Removing the active one falls back to a neighbour rather than to nothing.
        state.remove_location(1).unwrap();
        assert_eq!(state.active_location().name, "Paris, France");
        assert_eq!(f.load().locations().len(), 1);
    }

    #[test]
    fn the_last_location_and_a_row_that_is_not_there_are_refused_by_name() {
        let f = Fixture::new();
        let state = f.load();

        let refusal = state.remove_location(0).unwrap_err();
        assert!(refusal.contains("last saved location"), "{refusal}");
        assert_eq!(state.locations().len(), 1);

        let refusal = state.remove_location(7).unwrap_err();
        assert!(refusal.contains("no saved location"), "{refusal}");

        let refusal = state.select_location(7).unwrap_err();
        assert!(refusal.contains("no saved location"), "{refusal}");
        assert_eq!(state.active_index(), 0);
    }
}
