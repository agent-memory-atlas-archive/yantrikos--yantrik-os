//! What Weather keeps between runs, and the rules for changing it.
//!
//! This was a round trip with one end missing. `new()` read `~/.config/yantrik/weather.json`
//! and `save()` wrote it, so both halves were here and the format was right — but nothing in
//! the app ever called `save()`. Every city the person added and every choice of degrees lived
//! exactly as long as the process. Because the reading half worked, the app looked like it kept
//! them right up until the next launch, which is when the person found out it had not.
//!
//! Kept free of Slint and of the network on purpose: `tests/weather-core` includes this file
//! directly and exercises the store against a temp directory, with no desktop and no API.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};

pub const DEFAULT_LAT: f64 = 51.5074;
pub const DEFAULT_LON: f64 = -0.1278;
pub const DEFAULT_LOCATION_NAME: &str = "London";

/// How close two saved places have to be before they are the same place.
///
/// Names are the wrong key: the geocoder answers "paris" with "Paris" on one run and
/// "Paris, France" on another, so a name comparison would let the same city in twice.
/// A hundredth of a degree is roughly a kilometre, which no two cities share.
const SAME_PLACE: f64 = 0.01;

/// Where the readings currently on screen came from. `describe` publishes it, because a caller
/// reading temperatures is entitled to know whether the weather service produced them.
pub const FROM_SERVICE: &str = "service";
pub const FROM_DIRECT: &str = "direct";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedLocation {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
}

impl SavedLocation {
    pub fn is_same_place(&self, other: &SavedLocation) -> bool {
        (self.lat - other.lat).abs() < SAME_PLACE && (self.lon - other.lon).abs() < SAME_PLACE
    }
}

/// Prefs live next to the shell's settings (`~/.config/yantrik/weather.json`) — the user's
/// locations and unit choice should survive a restart, not a process.
#[derive(Default, Serialize, Deserialize)]
pub struct Prefs {
    #[serde(default)]
    pub fahrenheit: bool,
    #[serde(default)]
    pub active: usize,
    #[serde(default)]
    pub locations: Vec<SavedLocation>,
}

#[derive(Clone)]
pub struct WeatherState {
    /// Held rather than recomputed, so a test can point the whole store at a temp directory
    /// and so a failure can name the file it could not write.
    path: Arc<PathBuf>,
    locations: Arc<Mutex<Vec<SavedLocation>>>,
    active_index: Arc<Mutex<usize>>,
    use_fahrenheit: Arc<Mutex<bool>>,
    last_fetch_time: Arc<Mutex<Option<Instant>>>,
    reading_source: Arc<Mutex<&'static str>>,
}

impl WeatherState {
    /// `~/.config/yantrik/weather.json`, beside the shell's own settings.
    pub fn default_path() -> PathBuf {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join(".config/yantrik/weather.json")
    }

    /// Build from the prefs at `path` when they are readable; otherwise the default location.
    ///
    /// A file that is missing, unreadable or not the JSON we wrote is one situation from here:
    /// there is nothing to restore. It loads as the defaults rather than refusing to start,
    /// because a damaged prefs file is not a reason to have no weather — and it is not deleted
    /// either. The next change the person makes overwrites it, and by then there is something
    /// worth keeping in its place.
    pub fn load(path: PathBuf) -> Self {
        let prefs: Prefs = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();

        let (locations, active) = if prefs.locations.is_empty() {
            (
                vec![SavedLocation {
                    name: DEFAULT_LOCATION_NAME.to_string(),
                    lat: DEFAULT_LAT,
                    lon: DEFAULT_LON,
                }],
                0,
            )
        } else {
            let active = prefs.active.min(prefs.locations.len() - 1);
            (prefs.locations, active)
        };

        Self {
            path: Arc::new(path),
            locations: Arc::new(Mutex::new(locations)),
            active_index: Arc::new(Mutex::new(active)),
            use_fahrenheit: Arc::new(Mutex::new(prefs.fahrenheit)),
            last_fetch_time: Arc::new(Mutex::new(None)),
            reading_source: Arc::new(Mutex::new("")),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Write the configuration where a restart will find it.
    ///
    /// Returns a reason rather than swallowing one: the two `let _ =` that used to be here
    /// discarded both the directory creation and the write, so a full disk or a read-only home
    /// looked exactly like a successful save.
    pub fn save(&self) -> Result<(), String> {
        let prefs = Prefs {
            fahrenheit: *self.use_fahrenheit.lock().unwrap(),
            active: *self.active_index.lock().unwrap(),
            locations: self.locations.lock().unwrap().clone(),
        };
        let text = serde_json::to_string_pretty(&prefs)
            .map_err(|e| format!("the configuration could not be written as JSON: {e}"))?;
        write_atomically(&self.path, &text)
    }

    pub fn locations(&self) -> Vec<SavedLocation> {
        self.locations.lock().unwrap().clone()
    }

    pub fn active_index(&self) -> usize {
        *self.active_index.lock().unwrap()
    }

    pub fn active_location(&self) -> SavedLocation {
        let locs = self.locations.lock().unwrap();
        let idx = *self.active_index.lock().unwrap();
        locs.get(idx).cloned().unwrap_or(SavedLocation {
            name: DEFAULT_LOCATION_NAME.to_string(),
            lat: DEFAULT_LAT,
            lon: DEFAULT_LON,
        })
    }

    pub fn is_fahrenheit(&self) -> bool {
        *self.use_fahrenheit.lock().unwrap()
    }

    /// Keep a place, make it the one being shown, and write that down.
    ///
    /// Every mutator below follows the same shape: change what is in memory, save, and on a
    /// failed save put memory back the way it was. What is on screen and what is on disk then
    /// cannot disagree, and a caller that is told the change was made can act on that — a city
    /// that is listed this session and gone after a restart is the fault this module exists for.
    pub fn add_location(&self, loc: SavedLocation) -> Result<usize, String> {
        let index;
        let previous_active;
        {
            let mut locs = self.locations.lock().unwrap();
            if let Some(existing) = locs.iter().find(|l| l.is_same_place(&loc)) {
                return Err(format!(
                    "\"{}\" is already saved, as \"{}\"",
                    loc.name, existing.name
                ));
            }
            locs.push(loc);
            index = locs.len() - 1;
            let mut active = self.active_index.lock().unwrap();
            previous_active = *active;
            *active = index;
        }
        if let Err(e) = self.save() {
            let mut locs = self.locations.lock().unwrap();
            locs.remove(index);
            *self.active_index.lock().unwrap() = previous_active;
            return Err(e);
        }
        Ok(index)
    }

    pub fn remove_location(&self, idx: usize) -> Result<SavedLocation, String> {
        let removed;
        let previous_active;
        {
            let mut locs = self.locations.lock().unwrap();
            if idx >= locs.len() {
                return Err(format!(
                    "there is no saved location {idx}; there {}",
                    if locs.len() == 1 {
                        "is 1".to_string()
                    } else {
                        format!("are {}", locs.len())
                    }
                ));
            }
            if locs.len() == 1 {
                return Err("the last saved location cannot be removed; add another first".into());
            }
            removed = locs.remove(idx);
            let mut active = self.active_index.lock().unwrap();
            previous_active = *active;
            // Follow the same place rather than the same row: removing something above it
            // shifts it down, and removing the active one falls back to its neighbour.
            *active = if *active > idx {
                *active - 1
            } else {
                (*active).min(locs.len() - 1)
            };
        }
        if let Err(e) = self.save() {
            let mut locs = self.locations.lock().unwrap();
            locs.insert(idx, removed);
            *self.active_index.lock().unwrap() = previous_active;
            return Err(e);
        }
        Ok(removed)
    }

    /// Show one of the saved places. Persisted because the app reads `active` back on start —
    /// the index was already in the file and already restored, and until now nothing wrote it.
    pub fn select_location(&self, idx: usize) -> Result<SavedLocation, String> {
        let previous_active;
        {
            let locs = self.locations.lock().unwrap();
            if idx >= locs.len() {
                return Err(format!("there is no saved location {idx}"));
            }
            let mut active = self.active_index.lock().unwrap();
            previous_active = *active;
            *active = idx;
        }
        if let Err(e) = self.save() {
            *self.active_index.lock().unwrap() = previous_active;
            return Err(e);
        }
        Ok(self.active_location())
    }

    pub fn set_fahrenheit(&self, v: bool) -> Result<bool, String> {
        let previous = {
            let mut f = self.use_fahrenheit.lock().unwrap();
            let p = *f;
            *f = v;
            p
        };
        if let Err(e) = self.save() {
            *self.use_fahrenheit.lock().unwrap() = previous;
            return Err(e);
        }
        Ok(v)
    }

    pub fn record_fetch_time(&self) {
        *self.last_fetch_time.lock().unwrap() = Some(Instant::now());
    }

    /// Where the reading on screen came from: [`FROM_SERVICE`], [`FROM_DIRECT`], or empty
    /// before anything has been fetched.
    pub fn reading_source(&self) -> &'static str {
        *self.reading_source.lock().unwrap()
    }

    pub fn set_reading_source(&self, source: &'static str) {
        *self.reading_source.lock().unwrap() = source;
    }

    pub fn last_updated_text(&self) -> String {
        let guard = self.last_fetch_time.lock().unwrap();
        match *guard {
            None => String::new(),
            Some(t) => {
                let elapsed = t.elapsed().as_secs();
                if elapsed < 60 {
                    "Updated just now".to_string()
                } else if elapsed < 3600 {
                    let mins = elapsed / 60;
                    if mins == 1 {
                        "Updated 1 min ago".to_string()
                    } else {
                        format!("Updated {} min ago", mins)
                    }
                } else {
                    let hours = elapsed / 3600;
                    if hours == 1 {
                        "Updated 1 hr ago".to_string()
                    } else {
                        format!("Updated {} hr ago", hours)
                    }
                }
            }
        }
    }
}

/// Write the whole file or none of it.
///
/// `fs::write` truncates before it writes, so a crash, a full disk or a killed process between
/// the truncate and the last byte leaves a prefs file that parses as nothing — and every city
/// the person saved is gone although nothing deleted it. The temp file is made in the same
/// directory as the target so the rename stays within one filesystem, which is the condition
/// under which it is atomic; a rename across a mount point is a copy and gives none of this.
/// The temp name carries the process id so two apps saving at once cannot tread on each other,
/// and it is removed on either failure rather than left beside the config.
fn write_atomically(path: &Path, text: &str) -> Result<(), String> {
    let dir = path
        .parent()
        .ok_or_else(|| format!("{} has no directory to write into", path.display()))?;
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("could not make {}: {e}", dir.display()))?;

    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "weather.json".to_string());
    let tmp = dir.join(format!(".{}.{}.tmp", name, std::process::id()));

    if let Err(e) = std::fs::write(&tmp, text) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("could not write {}: {e}", tmp.display()));
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("could not save {}: {e}", path.display()));
    }
    Ok(())
}
