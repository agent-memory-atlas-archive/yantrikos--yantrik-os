//! The app's windows, as a test sees them and as the window manager would treat them: which are
//! open, a second launch, a close button, and which window has the keyboard.

use yantrik_app::app::{App, Event, WindowId};

use crate::TestApp;

impl<A: App> TestApp<A> {
    /// The windows open, the first one first.
    pub fn windows(&self) -> Vec<WindowId> {
        self.with(|u| u.core.windows_open())
    }

    /// Reads an open window (`None` once it is closed), as `ui` reads the first.
    pub fn window<R: Send + 'static>(
        &self,
        id: WindowId,
        f: impl FnOnce(&A::Ui) -> R + Send + 'static,
    ) -> Option<R> {
        self.with(move |u| u.core.window(id).map(|ui| f(&ui)))
    }

    /// A second launch asked for the app (`present`).
    pub fn present(&self) -> Result<(), String> {
        self.with(|u| u.core.present())
    }

    /// The person pressed a window's close button: `Event::CloseRequested` for it.
    pub fn close_requested(&self, id: WindowId) {
        self.event(Event::CloseRequested(id));
    }

    /// The window manager gave `id` the keyboard, as winit's `Focused(true)` says it under the
    /// runner: `Event::Activated` for it, unless it is already the active window or is closed.
    /// With this crate's `testing` feature.
    #[cfg(feature = "testing")]
    pub fn activate(&self, id: WindowId) {
        self.with(move |u| u.core.focus_for_test(id, true));
    }

    /// The window manager took the keyboard from `id` (another window, or another app's): no
    /// event, but its next [`TestApp::activate`] raises one again. With this crate's `testing`
    /// feature.
    #[cfg(feature = "testing")]
    pub fn deactivate(&self, id: WindowId) {
        self.with(move |u| u.core.focus_for_test(id, false));
    }
}
