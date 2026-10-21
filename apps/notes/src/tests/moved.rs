//! A note's text that moves to another note takes its draft with it (the 520 run of #918, defect
//! 1). *Keep mine as a copy*, *Save as new*, *Take theirs* (which keeps mine as a note), *Save a
//! copy* and text typed while a note goes to the Trash each leave the text in a note of its own;
//! the old note's draft must not outlive that, or the next start offers the same text again and
//! *Restore* makes it twice. Refresh, an import and a restore from the Trash move no text, and
//! leave nothing behind either. Each path ends in a crash here, and the next start offers nothing.

use std::path::PathBuf;

use serde_json::json;
use yantrik_app::documents::disk::Step;
use yantrik_testing::TestApp;

use super::{
    act, draft_file, failing, file, killed_and_started_again, notice, settle, state, unsaved, vault, with_note,
};
use crate::command::{Command, WindowAction};
use crate::update::said;
use crate::Notes;

const MINE: &str = "# Shared\n\nmine";
const THEIRS: &str = "# Shared\n\ntheirs\n";

fn press(app: &TestApp<Notes>, id: &'static str) {
    app.ui(move |ui| ui.invoke_action(id.into()));
    settle(app);
}

/// A saved note, open, edited with its draft written, and changed by another program meanwhile.
fn in_conflict() -> (TestApp<Notes>, PathBuf) {
    let (app, path) = with_note("Shared");
    app.run(Command::Edit(MINE.into()));
    app.run_writes();
    assert!(draft_file(&app, &path).exists(), "the draft is kept while the text is unsaved");
    std::fs::write(&path, THEIRS).unwrap();
    app.look_at_files();
    assert_eq!(super::bar(&app), 1);
    (app, path)
}

/// A saved note, open, edited with its draft written, and its file then deleted outside Notes.
fn file_gone() -> (TestApp<Notes>, PathBuf) {
    let (app, path) = with_note("Gone");
    app.run(Command::Edit("# Gone\n\nkeep me".into()));
    app.run_writes();
    std::fs::remove_file(&path).unwrap();
    app.look_at_files();
    assert_eq!(super::bar(&app), 2);
    (app, path)
}

/// What the next start offers back, by title.
fn offered(app: &TestApp<Notes>) -> Vec<String> {
    state(app)["recovery"].as_array().unwrap().iter().map(|o| o["title"].as_str().unwrap().to_string()).collect()
}

/// The lane does what it was handed (it would, within moments), then Notes is killed and started
/// again: it must offer nothing, since every text is in a note of its own.
fn crashed_offers_nothing(app: TestApp<Notes>, path: &str) {
    app.run_writes();
    let again = killed_and_started_again(app);
    assert_eq!(offered(&again), Vec::<String>::new(), "{path}: nothing is offered twice");
    assert_eq!(notice(&again), "", "{path}");
}

/// The note in the library whose file name starts with `prefix`, and its text on disk.
fn kept(app: &TestApp<Notes>, prefix: &'static str) -> String {
    let id = app.model(move |m| m.library.iter().find(|n| n.id.starts_with(prefix)).map(|n| n.id.clone()));
    file(&vault(app).join(id.unwrap_or_else(|| panic!("a {prefix} note"))))
}

#[test]
fn keep_mine_as_a_copy_leaves_no_draft_of_the_original() {
    let (app, path) = in_conflict();
    press(&app, "keep-mine");
    assert_eq!(kept(&app, "copy-"), MINE);
    app.run_writes();
    assert!(!draft_file(&app, &path).exists(), "the original's draft went with its text");
    crashed_offers_nothing(app, "keep mine");
}

#[test]
fn save_as_new_leaves_no_draft_of_the_gone_note() {
    let (app, path) = file_gone();
    press(&app, "keep-mine");
    assert_eq!(kept(&app, "copy-"), "# Gone\n\nkeep me");
    app.run_writes();
    assert!(!draft_file(&app, &path).exists());
    crashed_offers_nothing(app, "save as new");
}

#[test]
fn take_theirs_keeping_mine_as_a_note_leaves_no_draft() {
    let (app, path) = in_conflict();
    press(&app, "take-theirs");
    assert_eq!(kept(&app, "recovered-"), MINE);
    assert_eq!(file(&path), THEIRS);
    crashed_offers_nothing(app, "take theirs");
}

/// With the watch off, the store's check finds the file gone under a save: Take theirs has nothing
/// to take and moves the window to my text as a recovered note, as Keep mine would.
#[test]
fn take_theirs_on_a_file_found_gone_leaves_no_draft_of_the_gone_note() {
    let (app, path) = with_note("Gone");
    app.run(Command::Edit("# Gone\n\nkeep me".into()));
    app.run_writes();
    std::fs::remove_file(&path).unwrap();
    super::autosave(&app);
    press(&app, "take-theirs");
    assert_eq!(notice(&app), said::NOTHING_TO_TAKE);
    assert_eq!(kept(&app, "recovered-"), "# Gone\n\nkeep me");
    crashed_offers_nothing(app, "take theirs, nothing to take");
}

#[test]
fn save_a_copy_leaves_no_draft_of_the_original() {
    let (app, path) = in_conflict();
    press(&app, "copy");
    assert_eq!(kept(&app, "copy-"), MINE);
    assert!(!unsaved(&app), "the copy is saved");
    assert_eq!(file(&path), THEIRS);
    crashed_offers_nothing(app, "save a copy");
}

/// Never none either: a copy that cannot be saved keeps its own draft, written at once in place of
/// the original's, so the text is offered once, as the copy.
#[test]
fn a_copy_that_cannot_be_saved_is_offered_once_as_itself() {
    let (app, path) = in_conflict();
    failing(&app).fail(Step::Write, ".md", None);
    press(&app, "copy");
    assert!(unsaved(&app), "the copy could not be saved");
    let copy = app.model(|m| m.current().unwrap().id().to_string());
    app.run_writes();
    assert!(draft_file(&app, &vault(&app).join(&copy)).exists(), "the copy's draft is written");
    assert!(!draft_file(&app, &path).exists(), "and the original's is gone");
    let again = killed_and_started_again(app);
    // The copy's draft is offered, or the store's journal of its interrupted save came back as
    // a recovered note; either way the text is there once, and never under the original.
    let offers = state(&again)["recovery"].as_array().unwrap().clone();
    assert!(offers.iter().all(|o| o["filename"] == json!(copy)), "never as the original: {offers:?}");
    let listed = again.model(|m| m.library.iter().filter(|n| n.content.text == MINE).count());
    assert_eq!(offers.len() + listed, 1, "the text is there once: {offers:?}");
}

#[test]
fn text_typed_while_its_note_went_to_the_trash_is_offered_nothing_twice() {
    let (app, path) = with_note("Binned");
    app.run(Command::Window(WindowAction::Trash));
    assert_eq!(app.jobs_waiting(), 1, "the Trash job is in flight");
    app.run(Command::Edit("# Binned\n\ntyped while it went".into()));
    app.run_writes();
    assert!(draft_file(&app, &path).exists(), "the typing is kept as the note's draft meanwhile");
    settle(&app);
    assert_eq!(notice(&app), said::TYPED_DURING_TRASH);
    assert_eq!(kept(&app, "recovered-"), "# Binned\n\ntyped while it went");
    app.run_writes();
    assert!(!draft_file(&app, &path).exists(), "the trashed note's draft went with the text");
    crashed_offers_nothing(app, "trash");
}

#[test]
fn refresh_an_import_and_a_restore_from_the_trash_leave_nothing_to_offer() {
    // Refresh with unsaved text: saved first, then read again.
    let (app, path) = with_note("Refreshed");
    app.run(Command::Edit("# Refreshed\n\nunsaved".into()));
    app.run_writes();
    press(&app, "reload");
    assert_eq!(file(&path), "# Refreshed\n\nunsaved");
    crashed_offers_nothing(app, "refresh");

    // An import with unsaved text: the open note is saved first, then the imported one opened.
    let (app, path) = with_note("Before import");
    app.run(Command::Edit("# Before import\n\nunsaved".into()));
    app.run_writes();
    let outside = super::store::fixture("moved-import").join("in.md");
    std::fs::write(&outside, "# Imported\n\nfrom outside\n").unwrap();
    act(&app, "import", json!({ "path": outside.display().to_string() })).expect("imported");
    assert_eq!(file(&path), "# Before import\n\nunsaved");
    crashed_offers_nothing(app, "import");

    // To the Trash and back.
    let (app, _) = with_note("Round trip");
    app.run(Command::Edit("# Round trip\n\nunsaved".into()));
    app.run_writes();
    let id = app.model(|m| m.current().unwrap().id().to_string());
    act(&app, "trash", json!({})).expect("trashed");
    app.run(Command::Window(WindowAction::Open { id, trash: true }));
    act(&app, "restore", json!({})).expect("restored");
    assert_eq!(state(&app)["title"], "Round trip");
    crashed_offers_nothing(app, "trash and restore");
}

/// Discarding an offer whose note's file is gone does not say the note is as it is on disk.
#[test]
fn discard_of_an_offer_whose_file_is_gone_says_nothing_is_kept() {
    let (app, path) = with_note("Vanished");
    app.run(Command::Edit("# Vanished\n\nonly here".into()));
    app.run_writes();
    let paths = app.paths();
    drop(app);
    std::fs::remove_file(&path).unwrap();
    let app = TestApp::<Notes>::start_at(paths, Vec::new());
    settle(&app);
    assert_eq!(offered(&app), vec!["Vanished".to_string()]);
    act(&app, "discard_draft", json!({})).expect("discarded");
    assert_eq!(notice(&app), said::DISCARDED_GONE);
    crashed_offers_nothing(app, "discard");
}
