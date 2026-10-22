//! The window closed while a save is in flight: it closes at once, and the editor runs on with no
//! window until the save lands (`fx.linger`), then ends; a failure then is kept as a draft.

use serde_json::json;
use yantrik_app::app::{ExitReason, WindowId};
use yantrik_app::documents::disk::Step;

use super::{act, failing, file, killed_and_started_again, notice, settle, state, tabs, text, typed, with_file};
use crate::command::{Command, WindowAction};

/// A saved file open, edited, and its Save started but not landed.
fn saving() -> (yantrik_testing::TestApp<crate::TextEditor>, std::path::PathBuf) {
    let (app, path) = with_file("linger", "v1\n");
    typed(&app, "v2\n");
    app.run(Command::Window(WindowAction::Save));
    assert_eq!(app.jobs_waiting(), 1, "the save is in flight");
    (app, path)
}

#[test]
fn the_window_closed_during_a_save_closes_at_once_and_the_editor_ends_when_it_lands() {
    let (app, path) = saving();
    app.close_requested(WindowId::PRIMARY);
    assert!(app.windows().is_empty(), "the window is really closed, not hidden");
    assert!(app.lingering() && app.exit_reason().is_none());
    settle(&app);
    assert_eq!(file(&path), "v2\n");
    assert_eq!(app.exit_reason(), Some(ExitReason::App));
}

#[test]
fn a_save_that_fails_while_the_window_is_gone_keeps_its_text_for_the_next_start() {
    let (app, path) = with_file("linger-fails", "v1\n");
    let disk = failing(&app);
    disk.fail(Step::Write, "notes.txt", Some(1));
    typed(&app, "v2\n");
    app.run(Command::Window(WindowAction::Save));
    app.close_requested(WindowId::PRIMARY);
    settle(&app);
    assert_eq!(app.exit_reason(), Some(ExitReason::App));
    assert_eq!(file(&path), "v1\n");
    app.shutdown();
    let again = killed_and_started_again(app);
    assert_eq!(text(&again), "v2\n", "the text came back");
}

#[test]
fn a_second_launch_during_the_wait_brings_the_window_back_and_the_editor_runs_on() {
    let (app, path) = saving();
    app.close_requested(WindowId::PRIMARY);
    app.launch_again().expect("the hand-over is taken");
    assert_eq!(app.windows(), vec![WindowId::PRIMARY]);
    assert!(!app.lingering());
    settle(&app);
    assert_eq!(file(&path), "v2\n");
    assert!(app.exit_reason().is_none(), "the person is back");
    act(&app, "append", json!({ "text": "more\n" })).expect("and it takes calls again");
    app.advance(std::time::Duration::from_secs(11));
    assert!(app.exit_reason().is_none(), "the backstop is the framework's, and was cancelled");
}

#[test]
fn the_window_closed_during_a_save_with_another_tab_unsaved_waits_in_words() {
    // Another tab with unsaved changes would need asking, and the editor reads or writes one file
    // at a time: the window waits, and says so.
    let (app, _) = with_file("linger-other", "x\n");
    act(&app, "new", json!({ "text": "unsaved" })).unwrap();
    act(&app, "select_tab", json!({ "index": 0 })).unwrap();
    typed(&app, "y\n");
    app.run(Command::Window(WindowAction::Save));
    app.close_requested(WindowId::PRIMARY);
    assert_eq!(notice(&app), crate::update::closing::WAIT_BEFORE_CLOSING);
    assert_eq!(app.windows(), vec![WindowId::PRIMARY]);
    settle(&app);
    assert_eq!((state(&app)["dialog"].clone(), tabs(&app)), (json!(0), 2));
}
