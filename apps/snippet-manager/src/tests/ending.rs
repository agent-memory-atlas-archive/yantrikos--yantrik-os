//! The end of a run: the window closed with work in flight, a logout, the display going, and the
//! typing the editor held, kept in the draft lane and offered back at the next start.

use std::time::Duration;

use serde_json::json;
use yantrik_app::app::{ExitReason, WindowId};
use yantrik_app::cx::Inbound;
use yantrik_app::mind::fake::FakeMind;
use yantrik_app::quit::Signal;

use super::{open, start_at, Fixture};
use crate::store::file;
use crate::update::{CLOSE_BACKSTOP, CLOSING};
use crate::Command;

/// The person's New pressed, its write queued and not yet run.
fn a_write_queued(f: &Fixture) {
    f.app.ui(|ui| ui.invoke_snip_new());
    assert_eq!(f.app.jobs_waiting(), 1, "the write is on its way");
}

/// A signal, posted from a thread of its own as the signal thread posts it, then heard.
fn signal(f: &Fixture, signal: Signal) {
    let post = f.app.post();
    std::thread::spawn(move || post.post(Inbound::Signal(signal)).is_ok()).join().expect("the posting thread");
    f.app.tick();
}

#[test]
fn closing_the_window_with_a_write_queued_keeps_it_and_then_ends() {
    let f = open();
    f.seeded();
    a_write_queued(&f);
    f.app.close_requested(WindowId::PRIMARY);
    assert!(f.app.windows().is_empty(), "the window is closed at once");
    assert!(f.app.lingering(), "and the app runs on without it");
    assert_eq!(f.app.exit_reason(), None, "it waits for the write already sent");
    let e = f.app.act("new", json!({ "title": "After closing", "language": "Rust", "code": "x" })).unwrap_err();
    assert_eq!(e, CLOSING);
    f.app.run_jobs();
    assert_eq!(f.app.exit_reason(), Some(ExitReason::App), "the write landed, and the app ended itself");
    let titles: Vec<String> = f.on_disk().snippets().iter().map(|s| s.title.clone()).collect();
    assert!(titles.contains(&"Untitled Snippet".to_string()), "{titles:?}");
    assert!(!titles.contains(&"After closing".to_string()));
}

/// A disk that never answers does not keep a closed app alive: the linger's backstop ends it.
#[test]
fn a_close_waiting_on_a_write_that_never_lands_ends_at_the_backstop() {
    let f = open();
    a_write_queued(&f);
    f.app.close_requested(WindowId::PRIMARY);
    f.app.advance(CLOSE_BACKSTOP - Duration::from_secs(1));
    assert_eq!(f.app.exit_reason(), None);
    f.app.advance(Duration::from_secs(1));
    assert_eq!(f.app.exit_reason(), Some(ExitReason::LastWindowClosed));
}

/// A second launch during the wait opens the window again and cancels the close: the person can
/// write again, and the app runs on after the write lands and past the backstop.
#[test]
fn a_window_presented_during_the_close_wait_cancels_the_close() {
    let f = open();
    a_write_queued(&f);
    f.app.close_requested(WindowId::PRIMARY);
    f.app.present().unwrap();
    assert_eq!(f.app.windows(), [WindowId::PRIMARY]);
    f.keep("After coming back", "Rust", "x", "");
    f.app.advance(CLOSE_BACKSTOP + Duration::from_secs(1));
    assert_eq!(f.app.exit_reason(), None, "the close was cancelled");
    assert_eq!(f.on_disk().snippets().len(), 2);
}

/// The window manager giving a closed window the keyboard does not bring it back.
#[test]
fn a_stale_activation_does_not_cancel_a_close() {
    let f = open();
    a_write_queued(&f);
    f.app.close_requested(WindowId::PRIMARY);
    f.app.activate(WindowId::PRIMARY);
    assert!(f.app.windows().is_empty());
    f.app.run_jobs();
    assert_eq!(f.app.exit_reason(), Some(ExitReason::App));
}

#[test]
fn closing_with_nothing_pending_ends_at_once() {
    let f = open();
    f.seeded();
    f.app.close_requested(WindowId::PRIMARY);
    assert_eq!(f.app.exit_reason(), Some(ExitReason::LastWindowClosed));
    assert!(!f.app.lingering());
}

/// Typing no button kept: the old app lost it when the window closed. It goes to the draft lane,
/// and the next start opens it in the editor, unsaved, and says so.
#[test]
fn typing_left_in_the_editor_at_close_is_offered_back_at_the_next_start() {
    let f = open();
    let (journal, borrow) = f.seeded();
    f.app.run(Command::Select(journal));
    f.type_into("Typed, never saved", "journalctl -fu yantrik-ui -p err");
    f.app.close_requested(WindowId::PRIMARY);
    assert_eq!(f.app.exit_reason(), Some(ExitReason::LastWindowClosed), "nothing to wait for");
    assert_eq!(f.on_disk().get(journal).unwrap().title, "Tail the journal", "not saved behind the person's back");

    let f = f.restart();
    assert_eq!(f.app.model(|m| m.selected), journal);
    assert_eq!(f.app.ui(|ui| ui.get_detail_title().to_string()), "Typed, never saved");
    assert!(f.app.model(|m| m.editor_dirty()));
    assert!(f.notice().contains("Tail the journal") && f.notice().contains("Save keeps them"), "{}", f.notice());
    // Moving on keeps it, as any move of the editor does, and the draft goes once it is kept.
    f.app.run(Command::Select(borrow));
    f.quiet();
    f.app.run_writes();
    assert_eq!(f.on_disk().get(journal).unwrap().code, "journalctl -fu yantrik-ui -p err");
    let f = f.restart();
    assert_eq!(f.app.model(|m| m.selected), -1, "nothing is offered twice");
    assert_eq!(f.notice(), "");
}

/// A logout has nobody to answer: the typing goes to the draft lane without a question, a write
/// in flight lands, and then the app ends.
#[test]
fn a_logout_keeps_the_typing_and_waits_for_the_write_in_flight() {
    let f = open();
    let (journal, _) = f.seeded();
    f.app.run(Command::Select(journal));
    f.app.ui(move |ui| ui.invoke_snip_toggle_favorite(journal));
    f.type_into("Typed at logout", "journalctl");
    signal(&f, Signal::Terminate);
    assert_eq!(f.app.exit_reason(), None, "it waits for the write already sent");
    assert_eq!(f.app.act("delete", json!({ "id": journal.to_string() })).unwrap_err(), CLOSING);
    f.app.run_jobs();
    assert!(f.app.exit_reason().is_some(), "the write landed, and the app ended");
    assert!(f.on_disk().get(journal).unwrap().favorite);
    let f = f.restart();
    assert_eq!(f.app.ui(|ui| ui.get_detail_title().to_string()), "Typed at logout");
}

/// The display going is a logout too.
#[test]
fn the_display_going_keeps_the_typing() {
    let f = open();
    let (journal, _) = f.seeded();
    f.app.run(Command::Select(journal));
    f.type_into("Typed as the display went", "journalctl");
    f.app.display_gone();
    assert!(f.app.exit_reason().is_some());
    let f = f.restart();
    assert_eq!(f.app.ui(|ui| ui.get_detail_title().to_string()), "Typed as the display went");
}

/// Typing for a snippet the file no longer holds is not dropped: it is kept as a new snippet.
#[test]
fn typing_for_a_snippet_no_longer_kept_becomes_a_new_one() {
    let f = open();
    let (journal, borrow) = f.seeded();
    f.app.run(Command::Select(journal));
    f.type_into("Orphaned typing", "echo orphan");
    f.app.close_requested(WindowId::PRIMARY);
    let paths = f.paths.clone();
    let _ = f.app.shutdown();
    drop(f);
    // Between the runs the snippet goes from the file.
    let mut library = file::read_back(&file::state_path(paths.data_dir())).unwrap();
    library.delete(journal).unwrap();
    file::write(&file::state_path(paths.data_dir()), &file::encode(&library).unwrap()).unwrap();

    let f = start_at(paths, FakeMind::new(), Vec::new());
    let kept: Vec<(String, String)> = f.on_disk().snippets().iter().map(|s| (s.title.clone(), s.code.clone())).collect();
    assert!(kept.contains(&("Orphaned typing".to_string(), "echo orphan".to_string())), "{kept:?}");
    assert!(f.notice().contains("kept as a new snippet"), "{}", f.notice());
    assert_eq!(f.state()["open"], json!("Orphaned typing"));
    assert!(f.on_disk().get(borrow).is_some());
}

/// A second launch with a file while a close waits: the close is over, the window is back, and
/// the file it hands over is kept rather than refused.
#[test]
fn a_second_launch_with_a_file_during_the_close_wait_is_taken() {
    let dir = std::env::temp_dir().join(format!("snippets-relaunch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("later.py"), "print('handed over')\n").unwrap();
    let f = open();
    a_write_queued(&f);
    f.app.close_requested(WindowId::PRIMARY);
    assert!(f.app.lingering());
    let mut launch = yantrik_app::launch::Launch::empty(f.paths.profile());
    launch.files = vec![dir.join("later.py")];
    f.act("launch", json!({ "launch": launch })).unwrap();
    assert_eq!(f.app.windows(), [WindowId::PRIMARY], "the window is back");
    f.app.advance(CLOSE_BACKSTOP + Duration::from_secs(1));
    assert_eq!(f.app.exit_reason(), None, "and the app runs on");
    assert_eq!(f.state()["open"], json!("later.py"));
    assert_eq!(f.on_disk().snippets().len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The dock's click on a running app is a second launch with nothing to open: it cancels the
/// close the same way.
#[test]
fn a_second_launch_from_the_dock_during_the_close_wait_cancels_the_close() {
    let f = open();
    a_write_queued(&f);
    f.app.close_requested(WindowId::PRIMARY);
    f.app.launch_again().unwrap();
    assert_eq!(f.app.windows(), [WindowId::PRIMARY]);
    f.quiet();
    f.app.advance(CLOSE_BACKSTOP + Duration::from_secs(1));
    assert_eq!(f.app.exit_reason(), None);
    assert!(!f.app.lingering());
}
