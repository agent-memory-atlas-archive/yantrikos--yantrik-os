//! Gate row "Close cancelled" (design/notes-gate-2026-10-21.md §3): close as a document flow. A
//! clean close asks nothing; a dirty one asks *Save / Don't save / Cancel* once, and each answer
//! does exactly what it says. One note in two views is 3b's.

use yantrik_app::app::{Event, ExitReason, WindowId};
use yantrik_app::documents::disk::Step;
use yantrik_testing::TestApp;

use super::{draft_file, failing, file, notice, settle, unsaved, with_note};
use crate::command::{Command, WindowAction};
use crate::update::closing::CLOSE_DIALOG;
use crate::update::said;
use crate::Notes;

const TYPED: &str = "# Closing\n\nlast words";

/// A saved note with text typed and not saved yet, its draft written, and its file.
fn typed() -> (TestApp<Notes>, std::path::PathBuf) {
    let (app, path) = with_note("Closing");
    app.run(Command::Edit(TYPED.into()));
    app.run_writes();
    assert!(draft_file(&app, &path).exists(), "the draft is kept");
    (app, path)
}

fn prompt(app: &TestApp<Notes>) -> (i32, String) {
    app.ui(|ui| (ui.get_dialog(), ui.get_close_heading().to_string()))
}

fn press(app: &TestApp<Notes>, id: &'static str) {
    app.ui(move |ui| ui.invoke_action(id.into()));
}

#[test]
fn a_clean_close_asks_nothing() {
    let (app, _) = with_note("Clean");
    app.close_requested(WindowId::PRIMARY);
    assert_eq!(app.exit_reason(), Some(ExitReason::LastWindowClosed));
    assert_eq!(prompt(&app).0, 0, "no prompt");
}

#[test]
fn a_dirty_close_asks_and_cancel_keeps_everything_and_saves_nothing() {
    let (app, path) = typed();
    let saved = file(&path);
    app.close_requested(WindowId::PRIMARY);
    assert_eq!(prompt(&app), (CLOSE_DIALOG, "Save \"Closing\" before closing?".into()));
    assert_eq!(app.exit_reason(), None);
    assert_eq!(app.jobs_waiting(), 0, "nothing is saved while the person decides");
    press(&app, "close-cancel");
    settle(&app);
    assert_eq!(prompt(&app).0, 0, "the prompt goes");
    assert_eq!((app.exit_reason(), app.jobs_waiting()), (None, 0), "the window stays and nothing was saved");
    assert_eq!(file(&path), saved);
    assert!(unsaved(&app), "the text is kept as edits");
    assert_eq!(app.model(|m| m.current().unwrap().text().to_string()), TYPED);
    assert!(draft_file(&app, &path).exists(), "and so is its draft");
    // Escape is Cancel too.
    app.close_requested(WindowId::PRIMARY);
    app.run(Command::ViewSynced { focus_mode: false, dialog: 0 });
    assert!(app.model(|m| m.closing.is_none()));
    assert_eq!(app.exit_reason(), None);
}

#[test]
fn save_closes_only_once_the_save_lands() {
    let (app, path) = typed();
    app.close_requested(WindowId::PRIMARY);
    press(&app, "close-save");
    assert_eq!(app.jobs_waiting(), 1, "the save is in flight");
    assert_eq!(app.exit_reason(), None, "and the window waits for it");
    settle(&app);
    assert_eq!(file(&path), TYPED);
    assert_eq!(app.exit_reason(), Some(ExitReason::LastWindowClosed));
    app.run_writes();
    assert!(!draft_file(&app, &path).exists(), "the draft goes with the save");
}

#[test]
fn text_typed_while_the_closing_save_runs_is_saved_too_before_it_closes() {
    let (app, path) = typed();
    app.close_requested(WindowId::PRIMARY);
    press(&app, "close-save");
    app.run(Command::Edit(format!("{TYPED}, and more")));
    assert!(app.run_one_job(), "the first save lands, and its follow-up starts");
    assert_eq!(app.exit_reason(), None, "not closed over the newer text");
    settle(&app);
    assert_eq!(file(&path), format!("{TYPED}, and more"));
    assert_eq!(app.exit_reason(), Some(ExitReason::LastWindowClosed));
}

#[test]
fn a_failed_save_keeps_the_window_and_says_why() {
    let (app, path) = typed();
    let saved = file(&path);
    failing(&app).fail(Step::Rename, ".md", None);
    app.close_requested(WindowId::PRIMARY);
    press(&app, "close-save");
    settle(&app);
    assert_eq!(app.exit_reason(), None, "the window stays");
    assert!(notice(&app).contains("injected failure at Rename"), "and says why: {}", notice(&app));
    assert!(unsaved(&app) && file(&path) == saved);
    assert!(app.model(|m| m.closing.is_none()), "the close is over; asking again asks again");
    app.close_requested(WindowId::PRIMARY);
    assert_eq!(prompt(&app).0, CLOSE_DIALOG);
}

#[test]
fn a_save_an_outside_change_holds_back_keeps_the_window_and_says_why() {
    let (app, path) = typed();
    std::fs::write(&path, "theirs").unwrap();
    app.look_at_files();
    app.close_requested(WindowId::PRIMARY);
    press(&app, "close-save");
    settle(&app);
    assert_eq!(app.exit_reason(), None);
    assert_eq!(notice(&app), super::not_saved_over());
    assert_eq!(file(&path), "theirs", "nothing was saved over theirs");
}

#[test]
fn dont_save_drops_the_edits_and_the_draft() {
    let (app, path) = typed();
    let saved = file(&path);
    app.close_requested(WindowId::PRIMARY);
    press(&app, "close-discard");
    assert_eq!(app.exit_reason(), Some(ExitReason::LastWindowClosed));
    assert_eq!(app.jobs_waiting(), 0, "nothing saved");
    assert_eq!(file(&path), saved);
    app.run_writes();
    assert!(!draft_file(&app, &path).exists(), "the draft is gone too");
}

#[test]
fn a_second_close_during_the_save_asks_nothing_and_saves_nothing_more() {
    let (app, path) = typed();
    app.close_requested(WindowId::PRIMARY);
    press(&app, "close-save");
    assert_eq!(app.jobs_waiting(), 1);
    app.close_requested(WindowId::PRIMARY);
    app.close_requested(WindowId::PRIMARY);
    assert_eq!(prompt(&app).0, 0, "no second prompt");
    assert_eq!(app.jobs_waiting(), 1, "no second save");
    settle(&app);
    assert_eq!(file(&path), TYPED);
    assert_eq!(app.exit_reason(), Some(ExitReason::LastWindowClosed));
}

/// Quitting asks once over every note with edits not on disk. Notes shows one note at a time in
/// 3a, so one is asked about here; two open at once (two windows) is 3b's.
#[test]
fn quitting_asks_once_and_ends_once_saved() {
    let (app, path) = typed();
    app.event(Event::ExitRequested(ExitReason::App));
    app.event(Event::ExitRequested(ExitReason::App));
    assert_eq!(prompt(&app), (CLOSE_DIALOG, "Save \"Closing\" before Notes closes?".into()), "asked once");
    press(&app, "close-save");
    settle(&app);
    assert_eq!(file(&path), TYPED);
    assert_eq!(app.exit_reason(), Some(ExitReason::App));
}

#[test]
fn logging_out_asks_nothing_and_writes_the_draft_now() {
    let (app, path) = with_note("Leaving");
    app.run(Command::Edit("# Leaving\n\nunsaved".into()));
    app.event(Event::ExitRequested(ExitReason::Logout));
    assert_eq!(app.exit_reason(), Some(ExitReason::App));
    app.run_writes();
    assert!(draft_file(&app, &path).exists(), "the next start offers it back");
}

/// #914's open item: a library open read-only cannot save, so the prompt offers Don't save and
/// says Export keeps a copy, instead of waiting on a save the vault refuses.
#[test]
fn closing_a_read_only_library_with_unsaved_text_offers_dont_save() {
    let (app, _disk, path, saved, typed) = super::read_only::holding_unsaved_text();
    app.close_requested(WindowId::PRIMARY);
    let (heading, sentence) = app.ui(|ui| (ui.get_close_heading().to_string(), ui.get_close_said().to_string()));
    assert_eq!(app.ui(|ui| ui.get_dialog()), CLOSE_DIALOG);
    assert!(heading.starts_with("Save \""), "{heading}");
    assert_eq!(sentence, said::close_prompt(&["x".into()], false, true).1, "it says Export keeps a copy");
    assert!(app.ui(|ui| ui.get_read_only()), "and the prompt's Save is not shown");
    // Export first, as the sentence says: Cancel, then the window's Export.
    press(&app, "close-cancel");
    let out = path.parent().unwrap().parent().unwrap().join(format!("closing-export-{}.md", std::process::id()));
    assert!(!out.starts_with(path.parent().unwrap()), "outside the library");
    let _ = std::fs::remove_file(&out);
    press(&app, "export");
    app.run(Command::Window(WindowAction::Confirm { path: out.display().to_string() }));
    settle(&app);
    assert_eq!(file(&out), typed, "the text got out");
    app.close_requested(WindowId::PRIMARY);
    press(&app, "close-discard");
    assert_eq!(app.exit_reason(), Some(ExitReason::LastWindowClosed));
    assert_eq!(file(&path), saved, "nothing was written into the library");
}
