//! An approval card is never left behind another window.
//!
//! A card is drawn inside the shell's own window: the Mind panel, the Lens. When a card rises,
//! `control_approvals` raises the shell. But anything that takes focus afterwards covers it again:
//! a mind opening its next app into Mind View, a script, the person. On the live instance a
//! mind's `agent_run` card sat behind Mind View, maximised over the whole shell. The person at the
//! machine looked for it three times and could not see it, every card expired with the mind's
//! turn, and the conclusion was that approvals were broken (2 Oct 2026). A card is how a person
//! decides what a mind does. Hidden, it is a decision nobody can make.
//!
//! So while a card waits, a change of focus to any other window brings the shell back in front.
//! The changes come from `toplevel_watch`, which follows the compositor's own stream, so nothing
//! here polls. It is bounded on purpose, because a person may switch away on purpose to check
//! something before answering, and must not be fought for the keyboard: at most [`MAX_RAISES`]
//! times for one stretch of waiting, never twice within [`MIN_GAP`], and not at all once nothing
//! is waiting.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::windows::SHELL_WINDOW_TITLE;

/// Two raises are at least this far apart: one focus change that sets off another must not
/// become a loop between two windows.
const MIN_GAP: Duration = Duration::from_millis(1500);

/// How many times one stretch of waiting brings the shell back. A person who has switched away
/// this often is doing it on purpose; after that the taskbar's Chat button, amber with a count,
/// says a card is waiting without taking the keyboard.
const MAX_RAISES: u32 = 5;

static WAITING: AtomicBool = AtomicBool::new(false);
static RAISES: AtomicU32 = AtomicU32::new(0);
static LAST: Mutex<Option<Instant>> = Mutex::new(None);

/// The approvals store's word on whether any card is waiting. A new stretch of waiting starts its
/// count again; an answered or expired card ends it.
pub fn set_waiting(waiting: bool) {
    let was = WAITING.swap(waiting, Ordering::Relaxed);
    if waiting && !was {
        RAISES.store(0, Ordering::Relaxed);
    }
}

/// The compositor says another toplevel now has focus. Called from `toplevel_watch`'s thread.
pub fn front_changed(title: &str) {
    let since = LAST.lock().ok().and_then(|l| l.map(|at| at.elapsed()));
    if !should_raise(WAITING.load(Ordering::Relaxed), title, since, RAISES.load(Ordering::Relaxed)) {
        return;
    }
    RAISES.fetch_add(1, Ordering::Relaxed);
    if let Ok(mut last) = LAST.lock() {
        *last = Some(Instant::now());
    }
    // Off this thread: the raise waits on the compositor, and the stream must keep flowing.
    std::thread::spawn(|| {
        if let Err(why) = crate::windows::raise_shell() {
            tracing::warn!(%why, "a card is waiting behind another window and the shell could not come back in front");
        } else {
            tracing::info!("brought the shell back in front of a window that covered a waiting card");
        }
    });
}

/// Whether this change of focus should bring the shell back. Pure, for the tests.
fn should_raise(waiting: bool, title: &str, since_last: Option<Duration>, raises: u32) -> bool {
    waiting
        && !title.is_empty()
        && title != SHELL_WINDOW_TITLE
        && raises < MAX_RAISES
        && since_last.is_none_or(|gap| gap >= MIN_GAP)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The case from the live instance: a card waiting, and Mind View takes focus over it.
    #[test]
    fn a_window_taking_focus_over_a_waiting_card_brings_the_shell_back() {
        assert!(should_raise(true, "Mind View", None, 0));
        assert!(should_raise(true, "Notes", Some(Duration::from_secs(3)), 2));
    }

    #[test]
    fn nothing_waiting_takes_nothing_from_anyone() {
        assert!(!should_raise(false, "Mind View", None, 0));
    }

    /// The shell taking focus is the card coming forward, not something covering it.
    #[test]
    fn the_shell_itself_is_not_covering_the_card() {
        assert!(!should_raise(true, SHELL_WINDOW_TITLE, None, 0));
        assert!(!should_raise(true, "", None, 0));
    }

    /// Two windows handing focus back and forth must not become a loop.
    #[test]
    fn two_raises_are_never_closer_than_the_gap() {
        assert!(!should_raise(true, "Mind View", Some(Duration::from_millis(200)), 1));
    }

    /// A person who keeps switching away is doing it on purpose: after a few raises the shell
    /// stops taking the keyboard, and the amber Chat button carries the news instead.
    #[test]
    fn a_person_who_keeps_switching_away_is_left_alone() {
        assert!(!should_raise(true, "Mind View", None, MAX_RAISES));
    }

    #[test]
    fn a_new_stretch_of_waiting_counts_again() {
        set_waiting(false);
        RAISES.store(MAX_RAISES, Ordering::Relaxed);
        set_waiting(true);
        assert_eq!(RAISES.load(Ordering::Relaxed), 0);
        set_waiting(true);
        set_waiting(false);
    }
}
