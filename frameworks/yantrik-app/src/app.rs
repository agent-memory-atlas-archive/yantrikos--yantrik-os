//! What an app is to the framework: one trait, and the runner drives it
//! (design/app-framework-2026-10-21.md, §2).
//!
//! There is no base struct to inherit from. The app implements [`App`]; the runner makes the
//! window, keeps the model on the UI thread, applies every command through [`App::update`],
//! draws the result through [`App::render`], and ends the process in order. Step 2 of the
//! design: the model, commands, effects, jobs and timers. The surface's intents (step 3), the
//! manifest, paths and settings (step 4) and the clients (step 5) are added beside these.

use std::ffi::OsString;

use crate::control::View;
use crate::cx::Cx;
use crate::effects::Effects;
use crate::intents::Intents;
use crate::refused::Refused;
use crate::window::Window;

/// One window of the app. `PRIMARY` is the one the runner makes at start; the design allows
/// more, and nothing here assumes there is only one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WindowId(pub u32);

impl WindowId {
    pub const PRIMARY: WindowId = WindowId(0);
}

/// Why the process was asked to end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    /// The last window closed.
    LastWindowClosed,
    /// The app asked (`fx.exit()`).
    App,
    /// The session is ending: a bounded chance to save, not a completed interactive save.
    Logout,
}

/// Something the framework saw that the app may turn into a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The person asked a window to close. `None` from [`App::event`] closes it now; a command
    /// keeps it open until something runs `fx.close(window)`, so "Save changes?" is a flow.
    CloseRequested(WindowId),
    /// The process is asked to end. `None` ends it now; a command may save first and then
    /// `fx.exit()`.
    ExitRequested(ExitReason),
    /// A window became the active one.
    Activated(WindowId),
}

/// What the runner knows before any window exists.
#[derive(Debug, Clone, Default)]
pub struct Boot {
    /// The command line after the program name. Parsed into a `Launch` in step 4.
    pub args: Vec<OsString>,
}

/// Why the app could not start. Printed as a sentence, filed as a problem, exit 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Startup {
    pub sentence: String,
}

impl Startup {
    pub fn new(sentence: impl Into<String>) -> Startup {
        Startup {
            sentence: sentence.into(),
        }
    }
}

impl std::fmt::Display for Startup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.sentence)
    }
}

/// Which parts of the model a command changed, so `render` redraws those. A part is a feature's
/// name (`forecast::PART`). All parts unless an `update` narrowed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Changed {
    All,
    Parts(Vec<&'static str>),
}

impl Changed {
    pub fn all() -> Changed {
        Changed::All
    }

    pub fn none() -> Changed {
        Changed::Parts(Vec::new())
    }

    pub fn has(&self, part: &str) -> bool {
        match self {
            Changed::All => true,
            Changed::Parts(parts) => parts.contains(&part),
        }
    }

    pub fn is_empty(&self) -> bool {
        matches!(self, Changed::Parts(p) if p.is_empty())
    }

    pub(crate) fn add(&mut self, part: &'static str) {
        if let Changed::Parts(parts) = self {
            if !parts.contains(&part) {
                parts.push(part);
            }
        }
    }
}

/// An app, as the runner drives it.
pub trait App: Sized + 'static {
    /// The primary window's generated component. `yantrik_app::window!(X)` makes it a `Window`.
    type Ui: Window;
    /// Plain Rust, composed of feature models. Never holds a Slint handle or a list model.
    type Model: 'static;
    /// Plain data, `Send` and `Clone`: it crosses from worker threads, timers (which make one
    /// each time they fire) and the surface's thread.
    type Command: Send + Clone + 'static;

    /// The app's one id: the instance claim, the socket, the tracing target. The manifest
    /// (step 4) supplies it from `yantrik.toml`.
    const ID: &'static str;

    /// Build the model before any window exists. No UI.
    fn model(boot: &Boot) -> Result<Self::Model, Startup>;

    /// Bind the window's callbacks to commands. Once, on the UI thread, after the window is made
    /// and themed.
    fn wire(cx: &Cx<Self>);

    /// The first effects: an initial fetch, timers. After `wire` and the first `render`.
    fn start(model: &Self::Model, fx: &mut Effects<Self>) {
        let _ = (model, fx);
    }

    /// The only function that changes the model. Validate before changing anything: a
    /// `Refused` must leave the model as it was, and no effect it requested is run.
    fn update(
        model: &mut Self::Model,
        command: Self::Command,
        fx: &mut Effects<Self>,
    ) -> Result<(), Refused>;

    /// The only function that writes Slint properties. After every applied command, with the
    /// parts that changed; once at start with `Changed::All`.
    fn render(ui: &Self::Ui, window: WindowId, model: &Self::Model, changed: &Changed);

    /// The Mind's view of the app: a projection of the committed model, never the model itself
    /// (no caches, no credentials). Runs on every `describe` and around every `act`.
    fn describe(model: &Self::Model) -> View {
        let _ = model;
        View::new(Self::ID)
    }

    /// The commands the Mind, keys and menus may raise by name (`intents::Intent`).
    fn intents(table: &mut Intents<Self>) {
        let _ = table;
    }

    /// A framework event the app may turn into a command. `None` takes the default.
    fn event(event: Event, model: &Self::Model) -> Option<Self::Command> {
        let _ = (event, model);
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_parts_accumulate_and_all_covers_everything() {
        let mut c = Changed::none();
        assert!(c.is_empty());
        c.add("forecast");
        c.add("forecast");
        assert_eq!(c, Changed::Parts(vec!["forecast"]));
        assert!(c.has("forecast") && !c.has("places"));
        let mut all = Changed::all();
        all.add("x");
        assert!(all.has("anything"));
    }
}
