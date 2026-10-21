//! How the core hears a quit while windows are open (`crate::quit`): a person's, raised by the
//! app's `fx.request_quit()`, and the session's or the system's, posted by the signal thread or
//! seen by the runner when the display went.

use std::time::{Duration, Instant};

use crate::app::{App, Event, ExitReason};
use crate::quit::{LoggedOut, Signal};

use super::{Core, Inbound};

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
    /// again while a prompt is up is the app's flow to answer, as a second close is. Not during a
    /// logout: no prompt is put up while the session ends, and the logout is already the end.
    pub(super) fn quit_requested(&self) {
        if self.exit.get().is_some() {
            return;
        }
        if self.logout.get().is_some() {
            tracing::info!(app = self.id, "a quit asked during the logout; the logout answers it");
            return;
        }
        self.event(Event::ExitRequested(ExitReason::Person));
    }

    /// A signal asked the process to end: the logout, on the UI thread (`logout`).
    pub(super) fn signalled(&self, signal: Signal) {
        self.logout(Some(signal));
    }

    /// The display went (the compositor exited) before anything asked the app to end: the
    /// runner calls this after the event loop, and before `shutdown`. No one is there, so the
    /// app is owed its logout as for a signal; its writes need no display.
    pub fn display_gone(&self) {
        self.logout(None);
    }

    /// Raised once as `Event::ExitRequested(Logout)`, whichever asked first. One asked while the
    /// first is still being answered is not raised again, so a second SIGTERM, or the display
    /// going after one, stacks nothing; one after the app ended is too late to matter.
    fn logout(&self, signal: Option<Signal>) {
        let said = signal.map_or("the display went", Signal::name);
        if self.exit.get().is_some() {
            return;
        }
        if let Some(first) = self.logout.get() {
            let first = first.signal.map_or("the display going", Signal::name);
            tracing::info!(app = self.id, "{said} while the app answers {first}; not asked again");
            return;
        }
        self.logout.set(Some(LoggedOut { signal, at: Instant::now() }));
        tracing::info!(app = self.id, "{said}: the app saves without asking and ends");
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

    /// At shutdown, before the inbox closes: a signal still queued, because the event loop ended
    /// first (the display went at the same moment), is heard, and its logout applied, so its
    /// writes are handed to the lane. Only while the app has not ended for its own reason; the
    /// rest of what is queued is dropped, as the closing inbox always dropped it.
    pub(super) fn hear_last_signals(&self) {
        if self.exit.get().is_some() {
            return;
        }
        let queued = self.inbox.drain();
        let signal = queued.iter().find_map(|inbound| match inbound {
            Inbound::Signal(signal) => Some(*signal),
            _ => None,
        });
        if let Some(signal) = signal {
            self.signalled(signal);
        }
    }

    /// How long the shutdown's writes may take: after a signal, what is left of the deadline.
    pub(super) fn lane_grace(&self, grace: Duration) -> Duration {
        let since = self.logout.get().filter(|l| l.signal.is_some()).map(|l| l.at.elapsed());
        crate::quit::lane_grace(grace, since)
    }
}
