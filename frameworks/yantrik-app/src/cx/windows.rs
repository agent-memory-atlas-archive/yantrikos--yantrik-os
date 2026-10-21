//! The app's windows (design/notes-gate-2026-10-21.md §2, "Views and windows"). Part of
//! [`Core`]; here so the core's own file stays readable.
//!
//! The runner makes the first window; `fx.open_window()` makes more, each themed, given its
//! chrome, wired by `App::wire_window`, rendered whole and shown. Every render reaches every
//! open window, which draws itself from the one model. No window is privileged: closing any of
//! them, the first included, closes only it, and the app ends when none is left.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, Ordering};

use slint::ComponentHandle as _;

use super::{Core, Cx, Inbound};
use crate::app::{App, Changed, Event, ExitReason, WindowId};
use crate::problems;
use crate::theme::{self, ThemeSettings};
use crate::window::Window as _;

/// Window ids, unique in the process. The first window is `WindowId::PRIMARY` (0); an id handed
/// out by a command that is then refused is simply never used.
static NEXT_WINDOW: AtomicU32 = AtomicU32::new(1);

pub(crate) fn next_window_id() -> WindowId {
    WindowId(NEXT_WINDOW.fetch_add(1, Ordering::Relaxed))
}

/// The windows beyond the first, and what every new one is made with.
pub(crate) struct Windows<U> {
    /// The first window is open. It is kept, hidden, once closed: the event loop and a second
    /// launch's `present` hold it.
    first_open: bool,
    more: BTreeMap<WindowId, U>,
    /// The theme the windows wear now, so a window opened later wears it too.
    theme: Option<ThemeSettings>,
}

impl<U> Windows<U> {
    pub(crate) fn new() -> Windows<U> {
        Windows { first_open: true, more: BTreeMap::new(), theme: None }
    }
}

impl<A: App> Core<A> {
    /// The windows open now, the first one first.
    pub fn windows_open(&self) -> Vec<WindowId> {
        let windows = self.windows.borrow();
        let first = windows.first_open.then_some(WindowId::PRIMARY);
        first.into_iter().chain(windows.more.keys().copied()).collect()
    }

    /// An open window, by id: a strong handle to it.
    pub fn window(&self, id: WindowId) -> Option<A::Ui> {
        let windows = self.windows.borrow();
        if id == WindowId::PRIMARY {
            return windows.first_open.then(|| self.ui.clone_strong());
        }
        windows.more.get(&id).map(|ui| ui.clone_strong())
    }

    /// The theme the runner started the first window with, for the windows opened after it.
    pub fn set_theme(&self, theme: ThemeSettings) {
        self.windows.borrow_mut().theme = Some(theme);
    }

    /// Every open window draws what changed.
    pub(super) fn render_windows(&self, changed: &Changed) {
        // A snapshot: a window drawing may fire a callback, and nothing may hold the table then.
        let more: Vec<(WindowId, A::Ui)> = {
            let windows = self.windows.borrow();
            windows.more.iter().map(|(id, ui)| (*id, ui.clone_strong())).collect()
        };
        let model = self.model.borrow();
        // The first window is drawn even once closed: a second launch's `present` may show it
        // again, and it must show the app as it is, not as it was.
        A::render(&self.ui, WindowId::PRIMARY, &model, changed);
        for (id, ui) in more {
            A::render(&ui, id, &model, changed);
        }
    }

    /// The new theme, on every window.
    pub(super) fn apply_theme(&self, theme: &ThemeSettings) {
        self.windows.borrow_mut().theme = Some(*theme);
        self.ui.apply_theme(theme);
        let more: Vec<A::Ui> = self.windows.borrow().more.values().map(|ui| ui.clone_strong()).collect();
        for ui in more {
            ui.apply_theme(theme);
        }
    }

    /// `fx.open_window()`: the window made, themed, wired, drawn whole and shown.
    pub(super) fn open_window(&self, id: WindowId) {
        let ui = match A::Ui::make() {
            Ok(ui) => ui,
            Err(e) => {
                let why = format!("a window could not be made: {e}");
                tracing::warn!(app = self.id, "{why}");
                problems::record_failure(self.id, "window", &why);
                return;
            }
        };
        let theme = self.windows.borrow().theme.unwrap_or_else(theme::load);
        ui.apply_theme(&theme);
        ui.connect_chrome();
        let post = self.post();
        ui.window().on_close_requested(move || {
            // As the first window's: the app decides, a close now or a flow ending in `fx.close`.
            let _ = post.post(Inbound::Event(Event::CloseRequested(id)));
            crate::runner::drain_on_ui_thread();
            slint::CloseRequestResponse::KeepWindowShown
        });
        self.windows.borrow_mut().more.insert(id, ui.clone_strong());
        if let Some(core) = self.me.upgrade() {
            A::wire_window(&Cx::new(core), &ui, id);
        }
        A::render(&ui, id, &self.model.borrow(), &Changed::all());
        if let Err(e) = ui.show() {
            tracing::warn!(app = self.id, "a window could not be shown: {e}");
        }
    }

    /// A window closed: only it. The last one closing ends the process.
    pub(super) fn close_window(&self, window: WindowId) {
        let closed = if window == WindowId::PRIMARY {
            // Hidden, and counted closed, whether or not it was open: a second launch may have
            // shown it again since.
            let _ = self.ui.hide();
            self.windows.borrow_mut().first_open = false;
            true
        } else {
            let removed = self.windows.borrow_mut().more.remove(&window);
            removed.map(|ui| ui.hide()).is_some()
        };
        if closed && self.windows_open().is_empty() {
            self.request_exit(ExitReason::LastWindowClosed);
        }
    }
}
