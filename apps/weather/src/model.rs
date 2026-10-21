//! What Weather is: the model every command changes and every render reads, and the commands.

use std::sync::Arc;
use std::time::Instant;

use yantrik_app::mind::{AskError, Mind};
use yantrik_app::rail::{Rail, RailCommand};
use yantrik_services::weather::WeatherApi;

use crate::forecast::{Fetched, Reading, Source};
use crate::prefs::{Prefs, SavedLocation};

/// The parts of the model `render` redraws by name.
pub const PART_READING: &str = "reading";
pub const PART_PLACES: &str = "places";
pub const PART_NOTICE: &str = "notice";
pub const PART_UPDATED: &str = "updated";
pub const PART_AI: &str = "ai";

/// The first words of the strip the fetch path writes, so that a later good reading can clear
/// its own message without wiping one that is still true.
pub const DEGRADED_NOTICE: &str = "Readings came straight from Open-Meteo";

/// The AI panel: the one question about today's conditions.
#[derive(Debug, Clone, Default)]
pub struct Ai {
    pub working: bool,
    pub response: String,
    pub panel_open: bool,
}

pub struct Model {
    pub prefs: Prefs,
    pub reading: Reading,
    /// A fetch is in flight for the shown place.
    pub loading: bool,
    pub source: Source,
    /// Why the service was not used for the reading on screen, when it was not.
    pub degraded: Option<String>,
    /// What could not be done, in one line under the header.
    pub notice: String,
    pub last_fetch: Option<Instant>,
    pub ai: Ai,
    pub rail: Rail,
    pub weather: Arc<dyn WeatherApi>,
    pub mind: Arc<dyn Mind>,
    /// Whether Open-Meteo may be read directly when the service does not answer. Never under
    /// a test: a test's app reaches no internet.
    pub fall_back: bool,
    /// Where the prefs live, as `describe` and the intents say it.
    pub config_file: String,
}

impl Model {
    pub fn active(&self) -> SavedLocation {
        self.prefs.active_location()
    }

    /// "Updated 3 min ago", as the header's corner says it; empty before the first reading.
    pub fn last_updated_text(&self) -> String {
        let Some(t) = self.last_fetch else {
            return String::new();
        };
        let elapsed = t.elapsed().as_secs();
        if elapsed < 60 {
            "Updated just now".to_string()
        } else if elapsed < 3600 {
            match elapsed / 60 {
                1 => "Updated 1 min ago".to_string(),
                mins => format!("Updated {mins} min ago"),
            }
        } else {
            match elapsed / 3600 {
                1 => "Updated 1 hr ago".to_string(),
                hours => format!("Updated {hours} hr ago"),
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum Command {
    /// Fetch the shown place again.
    Refresh,
    /// A fetch landed: for this place, in these units. One for another is stale and dropped.
    Fetched {
        place: SavedLocation,
        fahrenheit: bool,
        fetched: Fetched,
    },
    /// The fetch ended without a reading (a panic, the pool's bound).
    FetchFailed(String),
    /// Show the saved place at this row.
    SelectLocation(usize),
    /// Show the saved place whose name contains this (the `show_location` intent).
    ShowLocationNamed(String),
    /// Look a place up; `Added` carries what the geocoder said.
    AddLocation(String),
    Added {
        query: String,
        outcome: Result<SavedLocation, String>,
    },
    RemoveLocation(usize),
    SetUnits(bool),
    /// The AI panel's question.
    Explain,
    Explained(Result<String, AskError>),
    DismissAi,
    /// The "Updated … ago" corner, re-said.
    TickUpdated,
    /// The prefs as the file says now (edited outside this app).
    PrefsFromFile(Prefs),
    /// A line for the notice strip (what could not be kept).
    Note(String),
    Rail(RailCommand),
}
