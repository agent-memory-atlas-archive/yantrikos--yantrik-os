//! Quitting: the ways an app is asked to end while its windows are open, and who asked.
//!
//! Closing the last window ends the process (`ExitReason::LastWindowClosed`). Two more ways reach
//! the app as `Event::ExitRequested`, and its reason says whether anyone is there to answer:
//!
//! - **A person** (`ExitReason::Person`): the app's `fx.request_quit()`, which it raises from
//!   Ctrl+Q or a Quit menu. The app's close flow runs over every window and may ask about unsaved
//!   work, once; the person may cancel, and the app runs on. Nothing ends until the flow runs
//!   `fx.exit()`. An app that takes the default (`None`) ends at once.
//! - **The session or the system** (`ExitReason::Logout`): SIGTERM (a shutdown, a logout, a
//!   kill) or SIGHUP (the session hung up). No one is there to answer, and the process will be
//!   killed after a grace period whatever the app does. The app saves without asking (a draft
//!   of every unsaved edit, say) and runs `fx.exit()`. It is asked once: a second signal while
//!   the first waits is not raised again, so nothing stacks. If the app has not ended within
//!   [`SIGNAL_DEADLINE`], the framework ends it, and the shutdown's own grace (`runner::GRACE`)
//!   still finishes the writes already handed to the persistence lane.
//!
//! The signal handler is async-signal-safe: `signal-hook`'s self-pipe, whose handler only writes
//! a byte. A thread of its own reads the pipe and posts to the core's inbox on the reserved lane,
//! so a full inbox of commands cannot lose a shutdown, and the post wakes the UI thread, where
//! the event is raised. SIGINT keeps its default: Ctrl+C in a terminal ends the app as it always
//! did.

use std::time::Duration;

use crate::app::App;
use crate::cx::Inbound;
use crate::queue::Post;

/// How long the app has, after the first signal, to save and end before the framework ends it.
/// The shutdown that follows waits up to `runner::GRACE` more for the writes, so the whole stays
/// within the few seconds a session or a service manager gives before it kills.
pub const SIGNAL_DEADLINE: Duration = Duration::from_secs(3);

/// A signal that asks the process to end, as the core hears it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// SIGTERM: a shutdown, a logout, a kill.
    Terminate,
    /// SIGHUP: the session hung up.
    HangUp,
}

impl Signal {
    /// The signal's usual name, for the log.
    pub fn name(self) -> &'static str {
        match self {
            Signal::Terminate => "SIGTERM",
            Signal::HangUp => "SIGHUP",
        }
    }

    /// The signal of this number, if it is one of these.
    #[cfg(unix)]
    pub fn from_raw(raw: i32) -> Option<Signal> {
        match raw {
            signal_hook::consts::SIGTERM => Some(Signal::Terminate),
            signal_hook::consts::SIGHUP => Some(Signal::HangUp),
            _ => None,
        }
    }
}

/// What the signal thread tells the core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Heard {
    /// A signal arrived.
    Signal(Signal),
    /// `deadline` has passed since the first signal.
    Overdue(Signal),
}

/// Hears SIGTERM and SIGHUP for the rest of the process and posts each to the core behind `post`,
/// with an `Overdue` once `deadline` has passed since the first. The runner calls this once,
/// before its event loop runs, with [`SIGNAL_DEADLINE`]; a test that raises a signal at its own
/// process may call it over its app's post. Until then, and on a platform without signals, the
/// signals keep their default.
#[cfg(unix)]
pub fn listen<A: App>(post: Post<Inbound<A>>, deadline: Duration) -> std::io::Result<()> {
    use signal_hook::consts::{SIGHUP, SIGTERM};
    let mut signals = signal_hook::iterator::Signals::new([SIGTERM, SIGHUP])?;
    std::thread::Builder::new()
        .name(format!("{}-signals", A::ID))
        .spawn(move || {
            let heard = signals.forever().filter_map(Signal::from_raw);
            forward(heard, deadline, move |heard| {
                post.post_control(match heard {
                    Heard::Signal(signal) => Inbound::Signal(signal),
                    Heard::Overdue(signal) => Inbound::Overdue(signal),
                })
                .is_ok()
            });
        })?;
    Ok(())
}

#[cfg(not(unix))]
pub fn listen<A: App>(post: Post<Inbound<A>>, deadline: Duration) -> std::io::Result<()> {
    let _ = (post, deadline);
    Ok(())
}

/// Every signal heard, passed on in order; the first also arms the deadline, once, on a thread
/// of its own, so the signals go on being heard while it waits. `post` says whether the core
/// still takes posts: once it does not (the app is shutting down), the rest are not passed on.
/// A signal then is ignored, so the shutdown's bounded writes are not cut short.
pub(crate) fn forward<P>(heard: impl IntoIterator<Item = Signal>, deadline: Duration, post: P)
where
    P: Fn(Heard) -> bool + Clone + Send + 'static,
{
    let mut armed = false;
    for signal in heard {
        if !post(Heard::Signal(signal)) {
            return;
        }
        if !std::mem::replace(&mut armed, true) {
            let overdue = post.clone();
            let spawned = std::thread::Builder::new()
                .name("quit-deadline".into())
                .spawn(move || {
                    std::thread::sleep(deadline);
                    overdue(Heard::Overdue(signal));
                });
            if let Err(e) = spawned {
                // Without the backstop the app still hears the signal; the system's kill is the
                // end then, as it always was.
                tracing::warn!("no thread for the quit deadline: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn recording() -> (impl Fn(Heard) -> bool + Clone + Send + 'static, mpsc::Receiver<Heard>) {
        let (tx, rx) = mpsc::channel();
        let tx = std::sync::Arc::new(std::sync::Mutex::new(tx));
        (move |h| tx.lock().unwrap().send(h).is_ok(), rx)
    }

    #[test]
    fn every_signal_is_passed_on_and_the_deadline_is_armed_once() {
        let (post, rx) = recording();
        let signals = [Signal::Terminate, Signal::Terminate, Signal::HangUp];
        forward(signals, Duration::from_millis(200), post);
        let wait = Duration::from_secs(5);
        let mut got: Vec<Heard> = (0..3).map(|_| rx.recv_timeout(wait).unwrap()).collect();
        got.push(rx.recv_timeout(wait).expect("the deadline came"));
        assert_eq!(
            got,
            vec![
                Heard::Signal(Signal::Terminate),
                Heard::Signal(Signal::Terminate),
                Heard::Signal(Signal::HangUp),
                Heard::Overdue(Signal::Terminate),
            ]
        );
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err(), "one deadline only");
    }

    #[test]
    fn nothing_more_is_passed_on_once_the_core_takes_no_posts() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let s = seen.clone();
        forward([Signal::HangUp, Signal::Terminate], Duration::from_secs(60), move |h| {
            s.lock().unwrap().push(h);
            false
        });
        assert_eq!(*seen.lock().unwrap(), vec![Heard::Signal(Signal::HangUp)]);
    }

    #[cfg(unix)]
    #[test]
    fn only_sigterm_and_sighup_are_heard() {
        assert_eq!(Signal::from_raw(signal_hook::consts::SIGTERM), Some(Signal::Terminate));
        assert_eq!(Signal::from_raw(signal_hook::consts::SIGHUP), Some(Signal::HangUp));
        assert_eq!(Signal::from_raw(signal_hook::consts::SIGINT), None);
        assert_eq!(Signal::Terminate.name(), "SIGTERM");
    }
}
