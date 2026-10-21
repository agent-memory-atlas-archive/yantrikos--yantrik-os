//! How the core hears a quit while windows are open (`crate::quit`): a person's, raised by the
//! app's `fx.request_quit()`, and the session's or the system's, posted by the signal thread.

use crate::app::{App, Event, ExitReason};
use crate::quit::Signal;

use super::Core;

impl<A: App> Core<A> {
    /// Why the process is ending, once something asked it to.
    pub fn exit_reason(&self) -> Option<ExitReason> {
        self.exit.get()
    }

    /// The end, asked for: the first reason is the one kept, and the event loop is told to stop.
    pub(super) fn request_exit(&self, reason: ExitReason) {
        if self.exit.get().is_none() {
            self.exit.set(Some(reason));
            let _ = slint::quit_event_loop();
        }
    }

    /// `fx.request_quit()`, once the command that asked has had its other effects applied: the
    /// app's own close flow runs from `Event::ExitRequested(ExitReason::Person)`, or, for an app
    /// that takes the default, the process ends now. Every request is raised: a person who asks
    /// again while a prompt is up is the app's flow to answer, as a second close is.
    pub(super) fn quit_requested(&self) {
        if self.exit.get().is_none() {
            self.event(Event::ExitRequested(ExitReason::Person));
        }
    }

    /// A signal asked the process to end: raised once as `Event::ExitRequested(Logout)`, on the
    /// UI thread. One heard while the first is still being answered is not raised again, so a
    /// second SIGTERM stacks nothing; one heard after the app ended is too late to matter.
    pub(super) fn signalled(&self, signal: Signal) {
        if self.exit.get().is_some() {
            return;
        }
        if let Some(first) = self.signalled.get() {
            tracing::info!(
                app = self.id,
                "{} while the app answers {}; it is not asked again",
                signal.name(),
                first.name()
            );
            return;
        }
        self.signalled.set(Some(signal));
        tracing::info!(app = self.id, "{}: the app saves without asking and ends", signal.name());
        self.event(Event::ExitRequested(ExitReason::Logout));
    }

    /// `quit::SIGNAL_DEADLINE` has passed since the first signal and the app has not ended: the
    /// framework ends it, and the shutdown's grace finishes the writes already handed over.
    pub(super) fn overdue(&self, signal: Signal) {
        if self.exit.get().is_some() {
            return;
        }
        tracing::warn!(app = self.id, "the app had not ended in time after {}; ending it now", signal.name());
        self.request_exit(ExitReason::Logout);
    }
}
