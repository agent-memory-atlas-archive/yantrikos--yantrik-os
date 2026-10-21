//! How Calendar ends: a signal or the display going is a logout, with nobody there to answer
//! (#923), and the person may close the window. A write already sent to the store is let land
//! first, so a Save pressed a moment before either is not lost, and then the app ends.

use std::time::Duration;

use serde_json::json;
use slint::ComponentHandle as _;
use yantrik_app::app::{ExitReason, WindowId};
use yantrik_app::cx::Inbound;
use yantrik_app::quit::Signal;

use super::{a_week, open, Fixture};

/// A signal, posted from a thread of its own as the signal thread posts it, then heard.
fn signal(f: &Fixture, signal: Signal) {
    let post = f.app.post();
    std::thread::spawn(move || post.post(Inbound::Signal(signal)).is_ok())
        .join()
        .expect("the posting thread");
    f.app.tick();
}

#[test]
fn a_logout_by_signal_lets_a_save_in_flight_land_and_then_ends() {
    let f = open(a_week());
    f.app.ui(|ui| ui.invoke_add_event());
    f.app.ui(|ui| ui.invoke_save_event("Pressed just before".into(), "2026-09-24".into(), "09:00".into(), "".into()));
    assert_eq!(f.app.jobs_waiting(), 1, "the create is on its way");
    signal(&f, Signal::Terminate);
    assert_eq!(f.app.exit_reason(), None, "it waits for the write already sent");
    let e = f.act("add_event", json!({ "title": "Too late", "date": "2026-09-24", "time": "10:00" })).unwrap_err();
    assert!(e.contains("Calendar is closing"), "{e}");
    f.app.run_jobs();
    assert!(f.app.exit_reason().is_some(), "the save landed, and the app ended");
    let titles: Vec<String> = f.store.stored().into_iter().map(|e| e.title).collect();
    assert!(titles.contains(&"Pressed just before".to_string()), "{titles:?}");
    assert!(!titles.contains(&"Too late".to_string()));
}

#[test]
fn the_display_going_with_nothing_pending_ends_the_app() {
    let f = open(a_week());
    assert_eq!(f.app.jobs_waiting(), 0);
    f.app.display_gone();
    assert!(f.app.exit_reason().is_some(), "nothing to wait for");
    let _ = f.app.shutdown();
}

/// A signal while nothing is being written ends the app at once, as the framework's default did.
#[test]
fn a_logout_with_nothing_pending_ends_at_once() {
    let f = open(a_week());
    signal(&f, Signal::HangUp);
    assert!(f.app.exit_reason().is_some());
}

/// Closing the last window with a save on its way: the window goes, the process stays until the
/// store has answered, the event is stored, and then the app ends. Before the framework this was
/// implicit: the save ran on the UI thread, so the close could not be handled first.
#[test]
fn closing_the_window_with_a_save_queued_stores_it_and_then_ends() {
    let f = open(a_week());
    f.app.ui(|ui| ui.invoke_add_event());
    f.app.ui(|ui| ui.invoke_save_event("Before closing".into(), "2026-09-24".into(), "09:00".into(), "".into()));
    f.app.close_requested(WindowId::PRIMARY);
    assert_eq!(f.app.exit_reason(), None, "it waits for the write already sent");
    assert!(!f.app.ui(|ui| ui.window().is_visible()), "the window is put away at once");
    let e = f.act("add_event", json!({ "title": "After closing", "date": "2026-09-24", "time": "10:00" })).unwrap_err();
    assert!(e.contains("Calendar is closing, so nothing more is written."), "{e}");
    f.app.run_jobs();
    assert!(f.app.exit_reason().is_some(), "the save landed, and the app ended");
    let titles: Vec<String> = f.store.stored().into_iter().map(|e| e.title).collect();
    assert!(titles.contains(&"Before closing".to_string()), "{titles:?}");
    assert!(!titles.contains(&"After closing".to_string()));
}

/// A store that never answers does not keep a closed app alive: after the backstop it ends anyway.
#[test]
fn a_close_waiting_on_a_store_that_never_answers_ends_at_the_backstop() {
    let f = open(a_week());
    f.app.ui(|ui| ui.invoke_delete_event(0));
    f.app.close_requested(WindowId::PRIMARY);
    f.app.advance(Duration::from_secs(9));
    assert_eq!(f.app.exit_reason(), None);
    f.app.advance(Duration::from_secs(1));
    assert!(f.app.exit_reason().is_some(), "ended without the store's answer");
}

/// With nothing in flight, closing the window ends the app at once, as the framework's default.
#[test]
fn closing_the_window_with_nothing_pending_ends_at_once() {
    let f = open(a_week());
    f.app.close_requested(WindowId::PRIMARY);
    assert_eq!(f.app.exit_reason(), Some(ExitReason::LastWindowClosed));
}
