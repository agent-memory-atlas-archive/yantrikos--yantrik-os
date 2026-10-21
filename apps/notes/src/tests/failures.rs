//! Gate row "Save failure" (design/notes-gate-2026-10-21.md §3), on Notes with a real vault: every
//! way a save can fail, through the framework's fault-injecting disk. Each failure keeps the edits
//! as edits, says in the notice how much reached the disk, reports it in `describe.save_failure`,
//! is not retried by the autosave, and is retried by the person's Save.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use yantrik_app::documents::disk::{FaultyDisk, Step};
use yantrik_testing::TestApp;

use super::{bar, failing, file, note, notice, settle, started, state, unsaved, vault};
use crate::command::{Command, WindowAction};
use crate::update::AUTOSAVE_AFTER;
use crate::Notes;

const SAVED: &str = "# Kept\n\nfirst\n";
const EDITED: &str = "# Kept\n\nfirst\nsecond";

/// A saved note, open, its text and its notebook and tags edited, and the disk made to fail once
/// at `step` for paths ending `suffix`; the autosave has tried the save.
fn failed_at(step: Step, suffix: &str) -> (TestApp<Notes>, PathBuf, Arc<FaultyDisk>) {
    let app = started();
    let path = PathBuf::from(note(&app, "Kept", "first")["path"].as_str().unwrap());
    assert_eq!(file(&path), SAVED);
    let disk = failing(&app);
    disk.fail(step, suffix, Some(1));
    app.run(Command::Edit(EDITED.into()));
    app.run(Command::Metadata { notebook: "Work".into(), tags: "two".into() });
    app.advance(AUTOSAVE_AFTER);
    settle(&app);
    (app, path, disk)
}

fn tries(disk: &FaultyDisk) -> usize {
    disk.hits.lock().unwrap().len()
}

fn status(app: &TestApp<Notes>) -> String {
    app.model(|m| m.pane.status.clone())
}

/// What every failure shares: the edits kept as edits, no retry until something changes, and the
/// person's Save landing everything (the fault was for one write only).
fn kept_and_retried_by_save(app: &TestApp<Notes>, path: &std::path::Path, disk: &FaultyDisk) {
    assert!(unsaved(app), "the edits are kept as edits");
    assert!(app.model(|m| m.current().unwrap().doc.dirty()), "and the document is dirty");
    assert_eq!(status(app), "Needs attention · draft kept");
    let tried = tries(disk);
    app.advance(AUTOSAVE_AFTER * 4);
    app.run(Command::Autosave);
    settle(app);
    assert_eq!((tries(disk), app.jobs_waiting()), (tried, 0), "the autosave does not retry it by itself");
    app.run(Command::Window(WindowAction::Save));
    settle(app);
    assert_eq!(file(path), EDITED, "the person's Save retries, and it lands");
    assert_eq!(std::fs::read_to_string(path.with_extension("meta")).unwrap(), "notebook:Work\ntags:two\n");
    assert!(!unsaved(app));
    assert_eq!((notice(app), state(app)["save_failure"].clone()), (String::new(), serde_json::Value::Null));
}

/// A failure before the rename: the note's file is as it was.
fn not_written_at(step: Step) {
    let (app, path, disk) = failed_at(step, ".md");
    assert_eq!(file(&path), SAVED, "{step:?}: the file is unchanged");
    assert_eq!(state(&app)["save_failure"], "not_written");
    assert_eq!(
        notice(&app),
        format!(
            "Not saved: injected failure at {step:?}. The file is unchanged and your text is still here; \
             Save tries again."
        )
    );
    kept_and_retried_by_save(&app, &path, &disk);
}

#[test]
fn a_temp_file_that_cannot_be_created_writes_nothing_and_says_so() {
    not_written_at(Step::CreateTemp);
}

#[test]
fn a_write_cut_short_writes_nothing_and_says_so() {
    not_written_at(Step::Write);
}

#[test]
fn a_rename_that_fails_writes_nothing_and_says_so() {
    not_written_at(Step::Rename);
}

#[test]
fn a_directory_sync_that_fails_after_the_rename_is_unknown_and_stays_unsaved() {
    let (app, path, disk) = failed_at(Step::SyncDir, ".md");
    assert_eq!(file(&path), EDITED, "the bytes are there, their durability is not known");
    assert_eq!(state(&app)["save_failure"], "unknown");
    assert_eq!(
        notice(&app),
        "Saving did not finish: injected failure at SyncDir. The note may not be safe on disk yet; your text \
         is still here, and Save tries again."
    );
    // Our own unsure bytes, seen by the watch, are not another program's change.
    app.look_at_files();
    assert_eq!((state(&app)["outside"].clone(), bar(&app)), (serde_json::json!("clean"), 0));
    kept_and_retried_by_save(&app, &path, &disk);
}

#[test]
fn a_journal_that_cannot_be_written_stops_the_save_before_anything_else() {
    let (app, path, disk) = failed_at(Step::Write, "pending.json");
    assert_eq!(*disk.hits.lock().unwrap(), vec![vault(&app).join(".recovery-v2/pending.json")]);
    assert_eq!(file(&path), SAVED, "the save did not go on to the note");
    assert!(!path.with_extension("meta").exists(), "nor to its sidecar");
    assert_eq!(state(&app)["save_failure"], "not_written");
    assert!(notice(&app).starts_with("Not saved: injected failure at Write."), "{}", notice(&app));
    kept_and_retried_by_save(&app, &path, &disk);
}

#[test]
fn a_sidecar_that_fails_after_the_text_is_content_only_and_the_next_start_recovers_it() {
    let (app, path, _) = failed_at(Step::Rename, ".meta");
    assert_eq!(file(&path), EDITED, "the text landed");
    assert!(!path.with_extension("meta").exists(), "the sidecar did not");
    assert_eq!(state(&app)["save_failure"], "content_only");
    assert!(unsaved(&app));
    assert_eq!(
        notice(&app),
        "Only the text was saved: injected failure at Rename. The notebook, tags and pin were not; Save tries \
         again, and if Notes closes first they come back in a recovered note."
    );
    // Notes closes with no second save, and the journal brings the rest back at the next start.
    let paths = app.paths();
    drop(app);
    let again = TestApp::<Notes>::start_at(paths, Vec::new());
    settle(&again);
    assert_eq!(state(&again)["notice"], "Recovered the interrupted save. Your original notes were preserved.");
    let recovered = again.model(|m| m.library.iter().find(|n| n.id.starts_with("recovered-")).cloned());
    let recovered = recovered.expect("the interrupted save comes back as a note of its own");
    assert_eq!(recovered.content.text, EDITED);
    assert_eq!((recovered.field("notebook"), recovered.field("tags")), ("Work".into(), "two".into()));
    assert_eq!(file(&path), EDITED, "and the note itself is as the text landed");
}

/// A vault remounted read-only mid-session: every write fails at its first step. However long the
/// person types on, one save is in flight at most, each change is tried once and not again on
/// every tick, and the notice is the same one line. Once the vault is writable, Save lands.
#[test]
fn a_read_only_vault_keeps_one_save_in_flight_and_one_notice() {
    let app = started();
    let path = PathBuf::from(note(&app, "Kept", "first")["path"].as_str().unwrap());
    let disk = failing(&app);
    disk.fail(Step::CreateTemp, "", None);
    let mut notices = BTreeSet::new();
    for word in ["one", "two", "three", "four", "five"] {
        let tried = tries(&disk);
        app.run(Command::Edit(format!("# Kept\n\n{word}")));
        app.advance(AUTOSAVE_AFTER);
        // Typing on while that save is in flight owes one follow-up, never a second job.
        app.run(Command::Edit(format!("# Kept\n\n{word}!")));
        app.advance(AUTOSAVE_AFTER);
        assert_eq!(app.jobs_waiting(), 1, "one save in flight, never a pile");
        settle(&app);
        assert_eq!(tries(&disk), tried + 1, "a failed save does not go on to its follow-up");
        notices.insert(notice(&app));
        // More ticks: the edit typed during the failed save is tried once, then nothing is.
        for _ in 0..4 {
            app.run(Command::Autosave);
            assert!(app.jobs_waiting() <= 1, "one save in flight");
            app.advance(AUTOSAVE_AFTER);
            settle(&app);
            notices.insert(notice(&app));
        }
        assert_eq!(tries(&disk), tried + 2, "each change is tried once, never once a tick");
    }
    assert_eq!(notices.len(), 1, "one notice, the same each time: {notices:?}");
    assert_eq!(file(&path), SAVED);
    assert!(unsaved(&app));
    disk.heal();
    app.run(Command::Window(WindowAction::Save));
    settle(&app);
    assert_eq!(file(&path), "# Kept\n\nfive!", "the latest text, once the vault is writable");
    assert_eq!((notice(&app), unsaved(&app)), (String::new(), false));
}
