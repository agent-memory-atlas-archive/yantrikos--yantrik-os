//! What mpv last reported, and a condition to wait on for the next change.

use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use super::client::Event;
use super::engine::REDIRECTED;

/// The last value mpv reported for every observed property, and the last file that ended.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Observed {
    pub time_pos: Option<f64>,
    pub duration: Option<f64>,
    pub paused: bool,
    pub volume: Option<f64>,
    pub muted: bool,
    pub idle: bool,
    pub path: Option<String>,
    /// How many files have loaded since the engine started; a waiter compares it.
    pub loads: u64,
    /// The last `end-file` with an error: what mpv said.
    pub last_error: Option<String>,
    /// How many files have failed since the engine started; a waiter compares it, so an old
    /// failure is never read as the answer to a new load.
    pub errors: u64,
    pub gone: bool,
}

impl Observed {
    pub fn apply(&mut self, event: &Event) {
        match event {
            Event::Property(name, v) => match name.as_str() {
                "time-pos" => self.time_pos = v.as_f64(),
                "duration" => self.duration = v.as_f64(),
                "pause" => self.paused = v.as_bool().unwrap_or(false),
                "volume" => self.volume = v.as_f64(),
                "mute" => self.muted = v.as_bool().unwrap_or(false),
                "idle-active" => self.idle = v.as_bool().unwrap_or(false),
                "path" => self.path = v.as_str().map(str::to_string),
                _ => {}
            },
            Event::FileLoaded => {
                self.loads += 1;
                self.last_error = None;
            }
            Event::EndFile { reason, error } => match reason.as_str() {
                "error" => {
                    self.errors += 1;
                    self.last_error = Some(error.clone().unwrap_or_else(|| "it could not be played".into()));
                }
                // The file turned out to be a playlist and mpv went on to what it lists. That is
                // a refusal: what was asked for did not play.
                "redirect" => {
                    self.errors += 1;
                    self.last_error = Some(REDIRECTED.into());
                }
                _ => {}
            },
            Event::Gone => self.gone = true,
        }
    }

    /// Playing means a file is loaded and not paused, by mpv's account.
    pub fn playing(&self) -> bool {
        !self.gone && !self.idle && self.path.is_some() && !self.paused
    }
}

/// [`Observed`] behind a lock, with a condition to wait on for the next change.
#[derive(Default)]
pub struct Shared {
    state: Mutex<Observed>,
    changed: Condvar,
}

impl Shared {
    pub fn apply(&self, event: &Event) {
        self.state.lock().unwrap().apply(event);
        self.changed.notify_all();
    }

    pub fn snapshot(&self) -> Observed {
        self.state.lock().unwrap().clone()
    }

    /// Wait up to `limit` for `done` to hold of what mpv has reported, and return the last
    /// report either way.
    pub fn wait_for(&self, limit: Duration, done: impl Fn(&Observed) -> bool) -> (bool, Observed) {
        let deadline = Instant::now() + limit;
        let mut state = self.state.lock().unwrap();
        loop {
            if done(&state) {
                return (true, state.clone());
            }
            let now = Instant::now();
            if now >= deadline {
                return (false, state.clone());
            }
            state = self.changed.wait_timeout(state, deadline - now).unwrap().0;
        }
    }
}
