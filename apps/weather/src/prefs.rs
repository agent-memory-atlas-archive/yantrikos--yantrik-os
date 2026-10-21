//! What Weather remembers between runs: the unit, the saved places and which is shown.
//!
//! The file is the one it always was, `~/.config/yantrik/weather.json`. Before the framework it
//! held these three fields bare; the framework's store wraps them in a schema envelope, and a
//! bare file is imported once (`Settings::legacy`) and backed up beside the new one.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use yantrik_app::settings::{Settings, SettingsError};

pub const DEFAULT_LAT: f64 = 51.5074;
pub const DEFAULT_LON: f64 = -0.1278;
pub const DEFAULT_LOCATION_NAME: &str = "London";

/// Two places this close are the same place: "Paris" and "Paris, France" from two geocoders.
const SAME_PLACE: f64 = 0.01;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedLocation {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
}

impl SavedLocation {
    pub fn london() -> SavedLocation {
        SavedLocation {
            name: DEFAULT_LOCATION_NAME.to_string(),
            lat: DEFAULT_LAT,
            lon: DEFAULT_LON,
        }
    }

    pub fn is_same_place(&self, other: &SavedLocation) -> bool {
        (self.lat - other.lat).abs() < SAME_PLACE && (self.lon - other.lon).abs() < SAME_PLACE
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Prefs {
    #[serde(default)]
    pub fahrenheit: bool,
    #[serde(default)]
    pub active: usize,
    #[serde(default)]
    pub locations: Vec<SavedLocation>,
}

impl Default for Prefs {
    fn default() -> Prefs {
        Prefs {
            fahrenheit: false,
            active: 0,
            locations: vec![SavedLocation::london()],
        }
    }
}

impl Prefs {
    /// A file may hold no places, or an active index past the end (a place removed by hand):
    /// London, or the last place, as the app always read it.
    pub fn normalised(mut self) -> Prefs {
        if self.locations.is_empty() {
            self.locations.push(SavedLocation::london());
        }
        self.active = self.active.min(self.locations.len() - 1);
        self
    }

    pub fn active_location(&self) -> SavedLocation {
        self.locations
            .get(self.active)
            .cloned()
            .unwrap_or_else(SavedLocation::london)
    }
}

impl Settings for Prefs {
    const SCHEMA: u32 = 1;

    /// The file as it was before the framework: the three fields, no envelope.
    fn legacy(raw: Value) -> Option<Value> {
        let object = raw.as_object()?;
        let known = ["fahrenheit", "active", "locations"];
        object.keys().all(|k| known.contains(&k.as_str())).then_some(raw)
    }

    fn migrate(_from: u32, raw: Value) -> Result<Value, SettingsError> {
        Ok(raw)
    }
}

#[cfg(test)]
impl SavedLocation {
    pub fn new_test(name: &str, lat: f64, lon: f64) -> SavedLocation {
        SavedLocation { name: name.into(), lat, lon }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_or_odd_file_still_shows_a_place() {
        let p = Prefs { fahrenheit: true, active: 7, locations: vec![] }.normalised();
        assert_eq!(p.locations, vec![SavedLocation::london()]);
        assert_eq!(p.active, 0);
        let p = Prefs {
            fahrenheit: false,
            active: 9,
            locations: vec![SavedLocation::london(), SavedLocation::new_test("Oslo", 59.9, 10.7)],
        }
        .normalised();
        assert_eq!(p.active, 1, "past the end is the last place");
    }

    #[test]
    fn the_old_bare_file_is_recognised() {
        let bare = serde_json::json!({ "fahrenheit": true, "active": 0, "locations": [] });
        assert!(Prefs::legacy(bare).is_some());
        assert!(Prefs::legacy(serde_json::json!({ "something": "else" })).is_none());
        assert!(Prefs::legacy(serde_json::json!(3)).is_none());
    }
}
