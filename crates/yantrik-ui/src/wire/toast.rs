//! Drawing toasts. Not deciding what a toast is.
//!
//! ## What this used to be
//!
//! `push_toast` was the shell's whole notification system: it appended to a Slint model AND
//! wrote into the shell's own private JSON store, from six call sites — screenshots, focus mode,
//! the companion bridge, the D-Bus relay, the Do Not Disturb hotkey. Nothing it recorded ever
//! reached the notifications service, so the notification screen could not show a screenshot
//! toast, an app could not raise one, and a mind could not read one. It also gave each toast an
//! id made from the wall clock in milliseconds, so two toasts raised in the same millisecond
//! shared an id and dismissing one dismissed both.
//!
//! ## What it is now
//!
//! The rendering half only. [`wire::notifications`](super::notifications) polls the one store
//! and calls [`push`] for each new notification; this file owns the queue, the three-at-a-time
//! rule, the "+N more" count and the sweep that takes an expired toast off the screen. Ids are
//! the store's.
//!
//! One exception is [`local`], for a toast that must **not** be stored — see its comment.
//!
//! ## No timer per toast
//!
//! Each toast used to start its own `slint::Timer` and then `std::mem::forget` it, so a machine
//! left running leaked one timer per notification, forever. There is one repeating sweeper
//! instead: it runs while any toast is up, expires whatever is due, and stops itself when the
//! queue empties. A timer cannot be dropped from inside its own callback, which is the reason
//! the per-toast version reached for `forget` in the first place.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, ModelRc, Timer, TimerMode, VecModel};

use crate::{App, ToastData};

/// How many toasts are on screen at once. Past this they are counted, not drawn.
const VISIBLE: usize = 3;

/// How long a low or normal toast stays. Critical ones do not expire — see [`lifetime`].
const DEFAULT_LIFETIME: Duration = Duration::from_secs(6);

/// How often the sweeper looks for an expired toast. Coarse on purpose: this runs for as long as
/// anything is on screen, and nobody can tell 6.0 s from 6.4 s.
const SWEEP: Duration = Duration::from_millis(400);

struct Live {
    data: ToastData,
    /// `None` for a toast that stays until it is dismissed.
    expires_at: Option<Instant>,
}

thread_local! {
    /// Newest first. Everything in here is on screen or counted in "+N more".
    static LIVE: RefCell<Vec<Live>> = const { RefCell::new(Vec::new()) };
    /// The one sweeper, alive only while something is up.
    static SWEEPER: RefCell<Option<Timer>> = const { RefCell::new(None) };
}

/// How long a toast of this urgency stays up.
///
/// Critical stays until it is dismissed. A message that is worth interrupting Do Not Disturb for
/// is worth being there when somebody comes back to the desk.
fn lifetime(urgency: i32) -> Option<Duration> {
    if urgency >= 2 {
        None
    } else {
        Some(DEFAULT_LIFETIME)
    }
}

/// Put a toast on screen. Replaces any toast with the same id, so a sender updating its own
/// notification moves the one that is up rather than stacking a second under it.
pub fn push(ui: &App, data: ToastData, urgency: i32) {
    let id = data.id.to_string();
    LIVE.with(|live| {
        let mut live = live.borrow_mut();
        live.retain(|t| t.data.id != id);
        live.insert(
            0,
            Live {
                data,
                expires_at: lifetime(urgency).map(|d| Instant::now() + d),
            },
        );
        // A person who walks away should not come back to a thousand held toasts. Anything past
        // this is already in the notification centre, which is where a backlog belongs.
        live.truncate(50);
    });
    republish(ui);
    ensure_sweeper(ui.as_weak());
}

/// Take one toast off the screen. The notification itself is untouched — dismissing the toast
/// and dismissing the notification are different acts, and the caller decides which it meant.
pub fn remove(ui: &App, id: &str) {
    LIVE.with(|live| live.borrow_mut().retain(|t| t.data.id != id));
    republish(ui);
}

/// Take them all off.
pub fn clear(ui: &App) {
    LIVE.with(|live| live.borrow_mut().clear());
    republish(ui);
}

/// A toast that is deliberately NOT stored.
///
/// One caller, and it earns the exception: the Do Not Disturb hotkey, which answers a key the
/// person just pressed with "Do Not Disturb: ON". Putting that in the store would file a
/// notification every time somebody toggles a switch, and the notification centre would fill
/// with a log of its own settings. It also has to appear while Do Not Disturb is being turned
/// **on**, which is exactly what the stored path suppresses.
///
/// Anything that is news rather than an acknowledgement goes to the service, so that it is kept,
/// countable, and readable by a mind: `yantrik_app_runtime::notify::send`.
pub fn local(ui_weak: &slint::Weak<App>, app: &str, summary: &str, body: &str, urgency: i32) {
    let Some(ui) = ui_weak.upgrade() else { return };
    let id = format!("local:{app}:{summary}");
    push(
        &ui,
        ToastData {
            id: id.into(),
            app_name: app.into(),
            summary: summary.into(),
            body: body.chars().take(120).collect::<String>().into(),
            urgency,
            icon_char: app
                .chars()
                .next()
                .unwrap_or('N')
                .to_uppercase()
                .to_string()
                .into(),
            actions: ModelRc::default(),
        },
        urgency,
    );
}

/// Hand the first [`VISIBLE`] to Slint and count the rest.
fn republish(ui: &App) {
    let (shown, overflow) = LIVE.with(|live| {
        let live = live.borrow();
        let shown: Vec<ToastData> = live.iter().take(VISIBLE).map(|t| t.data.clone()).collect();
        (shown, live.len().saturating_sub(VISIBLE))
    });
    ui.set_toast_overflow(overflow as i32);
    ui.set_toast_queue(ModelRc::new(VecModel::from(shown)));
}

/// Start the sweeper if it is not already running.
fn ensure_sweeper(ui_weak: slint::Weak<App>) {
    SWEEPER.with(|slot| {
        if slot.borrow().is_some() {
            return;
        }
        let timer = Timer::default();
        let weak = ui_weak.clone();
        timer.start(TimerMode::Repeated, SWEEP, move || {
            let Some(ui) = weak.upgrade() else { return };
            let now = Instant::now();
            let (changed, empty) = LIVE.with(|live| {
                let mut live = live.borrow_mut();
                let before = live.len();
                live.retain(|t| !matches!(t.expires_at, Some(at) if at <= now));
                (live.len() != before, live.is_empty())
            });
            if changed {
                republish(&ui);
            }
            if empty {
                // Stop, rather than tick forever over an empty queue for the life of the
                // session. Dropped on a *later* turn of the event loop, never from inside the
                // timer's own callback — and re-checked there, because a toast may have arrived
                // in between.
                let _ = slint::invoke_from_event_loop(|| {
                    let still_empty = LIVE.with(|live| live.borrow().is_empty());
                    if still_empty {
                        SWEEPER.with(|slot| slot.borrow_mut().take());
                    }
                });
            }
        });
        *slot.borrow_mut() = Some(timer);
    });
}
