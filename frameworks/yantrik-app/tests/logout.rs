//! The logout an app is owed at the end, headless (#923 review): on this OS's shutdown the
//! SIGTERM goes to the whole session at once, the compositor exits within milliseconds, and the
//! event loop ends with the display gone before the UI thread has drained the signal. The signal
//! still queued is heard at shutdown, the display going is a logout of its own, and either way
//! the logout's draft reaches the disk. A person's quit during a logout asks nothing.

mod common;

use yantrik_app::app::ExitReason;
use yantrik_app::cx::Inbound;
use yantrik_app::drafts::Drafts;
use yantrik_app::quit::Signal;
use yantrik_testing::TestApp;

use common::quitter::{Command, Quitter, DRAFT, DRAFT_TEXT};

/// Posts a signal from a thread of its own, as the signal thread does.
fn signal(app: &TestApp<Quitter>, signal: Signal) {
    let post = app.post();
    std::thread::spawn(move || post.post(Inbound::Signal(signal)).is_ok())
        .join()
        .map(|posted| assert!(posted, "the app is running"))
        .expect("the posting thread");
}

/// The draft the logout kept, as the next start would read it.
fn draft_on_disk(app: &TestApp<Quitter>) -> Option<Vec<u8>> {
    Drafts::read(app.paths().state_dir()).get(DRAFT).map(<[u8]>::to_vec)
}

#[test]
fn a_signal_still_queued_at_shutdown_gets_its_logout_and_its_draft_written() {
    let app = TestApp::<Quitter>::start();
    app.run(Command::Edit);
    signal(&app, Signal::Terminate);
    // The loop ended (the display went) before the UI thread drained the signal: no tick.
    app.shutdown();
    assert_eq!(app.model(|m| (m.logouts, m.asked)), (1, 0), "the logout ran, asking nothing");
    assert_eq!(draft_on_disk(&app).as_deref(), Some(DRAFT_TEXT), "its draft was written");
}

#[test]
fn the_display_going_with_no_signal_gets_the_logout_and_its_draft_written() {
    let app = TestApp::<Quitter>::start();
    app.run(Command::Edit);
    app.display_gone();
    assert_eq!(app.model(|m| (m.logouts, m.asked)), (1, 0));
    app.display_gone();
    assert_eq!(app.model(|m| m.logouts), 1, "the display goes once; the logout is raised once");
    app.shutdown();
    assert_eq!(draft_on_disk(&app).as_deref(), Some(DRAFT_TEXT));
}

#[test]
fn a_signal_and_the_display_going_together_are_one_logout() {
    let app = TestApp::<Quitter>::start();
    app.run(Command::Edit);
    signal(&app, Signal::Terminate);
    app.display_gone();
    app.shutdown();
    assert_eq!(app.model(|m| m.logouts), 1);
}

#[test]
fn the_display_going_after_the_app_ended_itself_raises_nothing() {
    let app = TestApp::<Quitter>::start();
    // Nothing unsaved: a quit ends it.
    app.run(Command::QuitPressed);
    assert_eq!(app.exit_reason(), Some(ExitReason::App));
    signal(&app, Signal::Terminate);
    app.display_gone();
    app.shutdown();
    assert_eq!(app.model(|m| m.logouts), 0, "the app had already ended for its own reason");
    assert_eq!(draft_on_disk(&app), None);
}

#[test]
fn a_persons_quit_during_a_logout_puts_up_no_prompt() {
    let app = TestApp::<Quitter>::start();
    app.run(Command::Edit);
    signal(&app, Signal::HangUp);
    app.tick();
    assert!(app.model(|m| m.saving), "the logout's save is under way");
    app.run(Command::QuitPressed);
    assert_eq!(app.model(|m| (m.asking, m.asked)), (false, 0), "nothing asked while the session ends");
    assert_eq!(app.exit_reason(), None, "the logout ends it, not the quit");
    app.run(Command::Saved);
    assert_eq!(app.exit_reason(), Some(ExitReason::App));
}
