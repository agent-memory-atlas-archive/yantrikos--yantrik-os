//! Saves as jobs through the open note's document: one in flight, the edits made meanwhile as one
//! follow-up, no retry by the autosave after a failure, never over a file changed outside, the
//! file watched while the note is open, and a close that waits for its save.

use serde_json::json;
use yantrik_app::app::{ExitReason, WindowId};
use yantrik_app::documents::disk::Step;
use yantrik_app::documents::OutsideState;
use yantrik_testing::TestApp;

use super::{act, autosave, failing, note, settle, started};
use crate::command::{Command, WindowAction};
use crate::store::CHANGED_ON_DISK;
use crate::update::AUTOSAVE_AFTER;
use crate::Notes;

/// A saved note, open, and where its file is.
fn with_note(title: &str) -> (TestApp<Notes>, std::path::PathBuf) {
    let app = started();
    let answer = note(&app, title, "");
    let path = std::path::PathBuf::from(answer["path"].as_str().unwrap());
    (app, path)
}

fn file(path: &std::path::Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

fn unsaved(app: &TestApp<Notes>) -> bool {
    app.model(|m| m.unsaved())
}

fn notice(app: &TestApp<Notes>) -> String {
    app.model(|m| m.notice.clone())
}

fn outside(app: &TestApp<Notes>) -> OutsideState {
    app.model(|m| m.current().unwrap().doc.outside.clone())
}

#[test]
fn two_quick_edits_during_a_save_produce_exactly_one_follow_up() {
    let (app, path) = with_note("Draft");
    app.run(Command::Edit("# Draft\n\none".into()));
    app.advance(AUTOSAVE_AFTER);
    assert_eq!(app.jobs_waiting(), 1, "the autosave's save is in flight");
    app.run(Command::Edit("# Draft\n\none two".into()));
    app.advance(AUTOSAVE_AFTER);
    app.run(Command::Edit("# Draft\n\none two three".into()));
    app.advance(AUTOSAVE_AFTER);
    assert_eq!(app.jobs_waiting(), 1, "one save in flight, never a pile");
    assert!(app.run_one_job());
    assert_eq!(file(&path), "# Draft\n\none", "the save in flight wrote what it was given");
    assert_eq!(app.jobs_waiting(), 1, "exactly one follow-up for both edits");
    assert!(app.run_one_job());
    assert_eq!(app.jobs_waiting(), 0, "and nothing after it");
    assert_eq!(file(&path), "# Draft\n\none two three", "the follow-up wrote the latest edit");
    assert!(!unsaved(&app));
    assert_eq!(app.model(|m| m.status.clone()), "All changes saved");
}

#[test]
fn a_failed_save_stays_unsaved_and_the_autosave_does_not_retry_it_until_something_changes() {
    let (app, path) = with_note("Read only");
    let disk = failing(&app);
    disk.fail(Step::Write, ".md", None);
    app.run(Command::Edit("# Read only\n\nmine".into()));
    autosave(&app);
    assert!(unsaved(&app), "a failed save keeps the edits as edits");
    assert!(notice(&app).contains("injected failure at Write"), "{}", notice(&app));
    assert_eq!(app.model(|m| m.status.clone()), "Needs attention · draft kept");
    let tried = disk.hits.lock().unwrap().len();
    // Ticks, and the autosave itself, find the same content failed: nothing is tried again.
    app.advance(AUTOSAVE_AFTER * 6);
    app.run(Command::Autosave);
    settle(&app);
    assert_eq!(disk.hits.lock().unwrap().len(), tried, "no retry, so no second notice");
    // An edit tries again.
    app.run(Command::Edit("# Read only\n\nmine, again".into()));
    autosave(&app);
    assert_eq!(disk.hits.lock().unwrap().len(), tried + 1);
    // So does the person's Save, once the disk is back, and that one lands.
    disk.heal();
    app.run(Command::Window(WindowAction::Save));
    settle(&app);
    assert!(!unsaved(&app));
    assert_eq!(file(&path), "# Read only\n\nmine, again");
    assert_eq!(notice(&app), "");
}

#[test]
fn the_store_refuses_to_save_over_a_file_changed_outside() {
    let (app, path) = with_note("Shared");
    std::fs::write(&path, "theirs").unwrap();
    app.run(Command::Edit("# Shared\n\nmine".into()));
    autosave(&app);
    assert_eq!(file(&path), "theirs", "never last-writer-wins");
    assert!(unsaved(&app), "my text is kept as edits");
    assert_eq!(notice(&app), CHANGED_ON_DISK);
    // Once the store has refused, the document knows: every later save is refused at once, in
    // the same words, with no job.
    assert!(matches!(outside(&app), OutsideState::Conflict { .. }));
    app.run(Command::Window(WindowAction::Save));
    assert_eq!(app.jobs_waiting(), 0);
    assert_eq!(file(&path), "theirs");
    let e = act(&app, "save", json!({})).unwrap_err();
    assert_eq!(e, CHANGED_ON_DISK);
    // The way out is still a copy.
    app.run(Command::Window(WindowAction::Copy));
    settle(&app);
    assert!(!unsaved(&app));
    assert_eq!(file(&path), "theirs");
}

#[test]
fn a_change_the_watch_sees_first_blocks_the_save_with_the_same_refusal() {
    let (app, path) = with_note("Watched");
    app.run(Command::Edit("# Watched\n\nmine".into()));
    std::fs::write(&path, "theirs").unwrap();
    app.look_at_files();
    assert!(matches!(outside(&app), OutsideState::Conflict { .. }));
    assert_eq!(notice(&app), "", "nothing is said until a save meets it, as before");
    autosave(&app);
    assert_eq!(app.jobs_waiting(), 0, "a save over it is not even tried");
    assert_eq!(notice(&app), CHANGED_ON_DISK);
    assert_eq!(file(&path), "theirs");
}

#[test]
fn an_outside_change_to_a_saved_note_changes_nothing_until_a_reload() {
    let (app, path) = with_note("Clean");
    let before = app.revision();
    std::fs::write(&path, "# Clean\n\nfrom elsewhere\n").unwrap();
    app.look_at_files();
    assert!(matches!(outside(&app), OutsideState::Changed { .. }));
    assert_eq!(app.revision(), before, "the window and describe are as they were");
    app.run(Command::Window(WindowAction::Reload));
    settle(&app);
    assert_eq!(app.model(|m| m.current().unwrap().text().to_string()), "# Clean\n\nfrom elsewhere\n");
    assert!(matches!(outside(&app), OutsideState::Clean), "read afresh, a fresh document");
}

#[test]
fn our_own_save_is_not_an_outside_edit_and_the_watch_follows_the_open_note() {
    let (app, path) = with_note("Mine");
    assert_eq!(app.watched().iter().map(|(_, p)| p.clone()).collect::<Vec<_>>(), vec![path.clone()]);
    app.run(Command::Edit("# Mine\n\nsaved by us".into()));
    autosave(&app);
    app.look_at_files();
    app.look_at_files();
    assert!(matches!(outside(&app), OutsideState::Clean), "{:?}", outside(&app));
    app.run(Command::Edit("# Mine\n\nsaved by us, twice".into()));
    autosave(&app);
    assert_eq!(file(&path), "# Mine\n\nsaved by us, twice");
    // Another note open: its file is watched, the first one's no longer.
    let other = note(&app, "Other", "");
    assert_eq!(app.watched().len(), 1);
    assert_eq!(app.watched()[0].1.display().to_string(), other["path"].as_str().unwrap());
    app.run(Command::Window(WindowAction::Trash));
    settle(&app);
    assert!(app.watched().is_empty(), "nothing open, nothing watched");
}

#[test]
fn a_clean_window_closes_at_once_and_a_dirty_one_once_its_save_lands() {
    let (app, _) = with_note("Closing");
    app.close_requested(WindowId::PRIMARY);
    assert_eq!(app.exit_reason(), Some(ExitReason::LastWindowClosed));

    let (app, path) = with_note("Closing later");
    app.run(Command::Edit("# Closing later\n\nlast words".into()));
    app.close_requested(WindowId::PRIMARY);
    assert_eq!(app.exit_reason(), None, "the window waits for the save");
    settle(&app);
    assert_eq!(file(&path), "# Closing later\n\nlast words");
    assert_eq!(app.exit_reason(), Some(ExitReason::LastWindowClosed));
}

#[test]
fn a_close_whose_save_fails_keeps_the_window_and_says_why() {
    let (app, _) = with_note("Stuck");
    failing(&app).fail(Step::Rename, ".md", None);
    app.run(Command::Edit("# Stuck\n\nunsaved".into()));
    app.close_requested(WindowId::PRIMARY);
    settle(&app);
    assert_eq!(app.exit_reason(), None);
    assert!(notice(&app).contains("injected failure at Rename"));
    assert_eq!(app.model(|m| m.quitting), None, "the close is over; asking again tries again");
    // A close while the store is busy waits, in words.
    app.run(Command::Window(WindowAction::Save));
    app.close_requested(WindowId::PRIMARY);
    assert_eq!(notice(&app), "Please wait for the current operation before closing.");
}
