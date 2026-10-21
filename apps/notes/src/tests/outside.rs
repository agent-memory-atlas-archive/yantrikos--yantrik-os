//! Gate row "Conflicting outside edit" (design/notes-gate-2026-10-21.md §3): a change during an
//! in-flight save in each of the three orders it can meet the save in, before the store's check
//! (the save loses), between the check and the rename (the save wins, and says so), after the
//! rename (a change to what was saved), and a change to the sidecar alone; and a note with nothing
//! unsaved, which follows its file. Detection before the next save, the store's check with the
//! watch off, and our own save are in saving.rs.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use yantrik_app::app::Event;
use yantrik_app::cx::Inbound;
use yantrik_app::documents::disk::{Disk, DiskError, RealDisk};
use yantrik_app::documents::{Fingerprint, Observed, OutsideState, Replaced};

use super::{act, autosave, bar, file, not_saved_over, notice, outside, settle, state, unsaved, vault, with_note};
use crate::command::{Command, WindowAction};
use crate::update::{said, AUTOSAVE_AFTER};

#[test]
fn a_change_before_the_stores_check_wins_and_the_save_becomes_a_conflict() {
    let (app, path) = with_note("Race");
    app.run(Command::Edit("# Race\n\nmine".into()));
    app.advance(AUTOSAVE_AFTER);
    assert_eq!(app.jobs_waiting(), 1, "the save is in flight");
    std::fs::write(&path, "theirs").unwrap();
    app.look_at_files();
    assert_eq!(outside(&app), OutsideState::Clean, "seen during the save, judged when it lands");
    settle(&app);
    assert_eq!(outside(&app), OutsideState::Conflict { now: Some(Fingerprint::of_bytes(b"theirs")) });
    assert_eq!(file(&path), "theirs", "never written over");
    assert!(unsaved(&app), "my text is kept as edits");
    assert_eq!((notice(&app), bar(&app)), (not_saved_over(), 1));
}

/// A disk on which another program writes the note after the store's check and before our
/// rename, and the watch sees it in that moment: the one order the check cannot catch.
struct WrittenBeforeOurRename {
    note: PathBuf,
    theirs: &'static str,
    /// What the watch thread posts on seeing the file; the write and the sighting happen once.
    watch_sees: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl Disk for WrittenBeforeOurRename {
    fn write_atomically(&self, path: &Path, bytes: &[u8]) -> Result<(), DiskError> {
        if path == self.note {
            if let Some(sighting) = self.watch_sees.lock().unwrap().take() {
                std::fs::write(path, self.theirs).unwrap();
                sighting();
            }
        }
        RealDisk.write_atomically(path, bytes)
    }

    fn create_new(&self, path: &Path, bytes: &[u8]) -> Result<(), DiskError> {
        RealDisk.create_new(path, bytes)
    }

    fn read(&self, path: &Path) -> std::io::Result<Option<Vec<u8>>> {
        RealDisk.read(path)
    }

    fn remove(&self, path: &Path) -> std::io::Result<()> {
        RealDisk.remove(path)
    }
}

#[test]
fn a_change_between_the_check_and_the_rename_is_replaced_and_said() {
    let (app, path) = with_note("Race");
    let (key, _) = app.watched().remove(0);
    let theirs = "# Race\n\ntheirs";
    let post = app.post();
    // Posted from inside the save job, as the watch thread would: it reaches the app before the
    // save's own outcome does.
    let sighting = move || {
        let seen = Observed::File(Fingerprint::of_bytes(theirs.as_bytes()));
        post.post(Inbound::Event(Event::FileChanged { key, seen })).expect("the app is running");
    };
    app.run(Command::UseDisk(Arc::new(WrittenBeforeOurRename {
        note: path.clone(),
        theirs,
        watch_sees: Mutex::new(Some(Box::new(sighting))),
    })));
    app.run(Command::Edit("# Race\n\nmine".into()));
    autosave(&app);
    assert_eq!(file(&path), "# Race\n\nmine", "ours landed over theirs");
    assert_eq!(notice(&app), said::replaced(Replaced::OutsideEdit), "and the person is told");
    assert_eq!((outside(&app), bar(&app), unsaved(&app)), (OutsideState::Clean, 0, false));
    // Still said while the person types on and the next save lands.
    app.run(Command::Edit("# Race\n\nmine, more".into()));
    autosave(&app);
    assert_eq!(file(&path), "# Race\n\nmine, more");
    assert_eq!(notice(&app), said::replaced(Replaced::OutsideEdit));
    // The person's next action takes it back.
    app.run(Command::Window(WindowAction::Save));
    assert_eq!(notice(&app), "");
}

#[test]
fn a_change_after_the_rename_is_a_change_to_what_was_saved() {
    let (app, path) = with_note("Race");
    app.run(Command::Edit("# Race\n\nv1".into()));
    app.advance(AUTOSAVE_AFTER);
    // Typed on during the save: one follow-up is owed.
    app.run(Command::Edit("# Race\n\nv2".into()));
    app.advance(AUTOSAVE_AFTER);
    assert!(app.run_one_job(), "the first save lands, and its follow-up starts");
    assert_eq!(file(&path), "# Race\n\nv1");
    assert_eq!(app.jobs_waiting(), 1);
    // The other program writes after our rename, and the watch sees it while the follow-up runs.
    std::fs::write(&path, "theirs").unwrap();
    app.look_at_files();
    settle(&app);
    assert_eq!(file(&path), "theirs", "the follow-up does not save over it");
    assert!(matches!(outside(&app), OutsideState::Conflict { .. }));
    assert_eq!((notice(&app), bar(&app)), (not_saved_over(), 1), "a conflict, not a replaced write");
    assert_eq!(app.model(|m| m.current().unwrap().text().to_string()), "# Race\n\nv2", "v2 is kept");
}

/// The watch reads the text file only; the store's check reads the sidecar too.
#[test]
fn a_change_to_the_sidecar_alone_is_caught_by_the_stores_check_and_resolved_like_any_other() {
    let (app, path) = with_note("Tagged");
    app.run(Command::Metadata { notebook: String::new(), tags: "mine".into() });
    autosave(&app);
    let meta = path.with_extension("meta");
    std::fs::write(&meta, "tags:theirs\n").unwrap();
    app.look_at_files();
    assert_eq!(outside(&app), OutsideState::Clean, "nothing the watch looks at changed");
    app.run(Command::Edit("# Tagged\n\nmine".into()));
    autosave(&app);
    assert_eq!(std::fs::read_to_string(&meta).unwrap(), "tags:theirs\n", "the sidecar is not written over");
    assert_eq!(file(&path), "# Tagged\n\n", "nor the text beside it");
    assert_eq!((notice(&app), bar(&app)), (not_saved_over(), 1));
    // Compare shows where the two differ: the sidecars as well as the texts.
    let compared = act(&app, "compare", serde_json::json!({})).expect("compare");
    assert_eq!(compared["theirs_metadata"], "tags:theirs\n");
    assert_eq!(compared["mine_metadata"], "notebook:\ntags:mine\n");
    let fields = app.ui(|ui| (ui.get_mine_fields().to_string(), ui.get_theirs_fields().to_string()));
    assert_eq!(fields, ("Tags: mine".to_string(), "Tags: theirs".to_string()));
    // Take theirs: the file's sidecar comes in; my text and tags are kept as a note of their own.
    app.run(Command::Window(WindowAction::TakeTheirs));
    settle(&app);
    let (text, tags) = app.model(|m| {
        let c = m.current().unwrap();
        (c.text().to_string(), c.content().field("tags"))
    });
    assert_eq!((text.as_str(), tags.as_str()), ("# Tagged\n\n", "theirs"));
    let kept = app.model(|m| m.library.iter().find(|n| n.id.starts_with("recovered-")).cloned());
    let kept = kept.expect("my version is kept");
    assert_eq!((kept.content.text.as_str(), kept.field("tags").as_str()), ("# Tagged\n\nmine", "mine"));
    assert!(!unsaved(&app));
}

fn text(app: &yantrik_testing::TestApp<crate::Notes>) -> String {
    app.model(|m| m.current().unwrap().text().to_string())
}

/// A note with nothing unsaved follows its file: read again and shown, said in one quiet line,
/// and its undo history starts again, as *Take theirs* does.
#[test]
fn a_saved_note_changed_outside_is_read_again_and_shown() {
    let (app, path) = with_note("Clean");
    app.run(Command::Edit("# Clean\n\nmine".into()));
    autosave(&app);
    std::fs::write(&path, "# Clean\n\nfrom elsewhere\n").unwrap();
    app.look_at_files();
    assert_eq!(app.jobs_waiting(), 1, "the note is read again");
    settle(&app);
    assert_eq!(app.ui(|ui| ui.get_content().to_string()), "# Clean\n\nfrom elsewhere\n", "and shown");
    assert_eq!(notice(&app), said::UPDATED_FROM_DISK);
    assert_eq!((outside(&app), bar(&app), unsaved(&app)), (OutsideState::Clean, 0, false));
    assert_eq!(state(&app)["outside"], "clean");
    app.run(Command::Window(WindowAction::Undo));
    assert_eq!(text(&app), "# Clean\n\nfrom elsewhere\n", "no undo puts the old text back");
    // The watch goes on from the file as read: another look is quiet, and the next save lands.
    app.look_at_files();
    assert_eq!(app.jobs_waiting(), 0);
    app.run(Command::Edit("# Clean\n\nfrom elsewhere\nand here".into()));
    autosave(&app);
    assert_eq!(file(&path), "# Clean\n\nfrom elsewhere\nand here");
}

/// Never replaced once something is typed: a key between the change and the read landing makes
/// it the conflict it is, and the person chooses.
#[test]
fn a_key_typed_before_the_reread_lands_makes_it_a_conflict_and_nothing_is_lost() {
    let (app, path) = with_note("Clean");
    std::fs::write(&path, "theirs").unwrap();
    app.look_at_files();
    assert_eq!(app.jobs_waiting(), 1, "the read is in flight");
    app.run(Command::Edit("# Clean\n\ntyped".into()));
    settle(&app);
    assert_eq!(text(&app), "# Clean\n\ntyped", "what was typed is not replaced");
    assert!(matches!(outside(&app), OutsideState::Conflict { .. }));
    assert_eq!(bar(&app), 1, "the choices are offered");
    assert_ne!(notice(&app), said::UPDATED_FROM_DISK);
    autosave(&app);
    assert_eq!(file(&path), "theirs", "and nothing is saved over theirs");
    app.ui(|ui| ui.invoke_action("keep-mine".into()));
    settle(&app);
    let copy = app.model(|m| m.current().unwrap().id().to_string());
    assert_eq!(file(&vault(&app).join(copy)), "# Clean\n\ntyped");
    assert_eq!(file(&path), "theirs");
}

/// A change seen while the store works on something else is read once the store is free.
#[test]
fn a_change_seen_while_the_store_works_is_read_again_once_it_is_done() {
    let (app, path) = with_note("Clean");
    let out = vault(&app).parent().unwrap().join(format!("exported-{}.md", std::process::id()));
    let export = app.act("export", serde_json::json!({ "path": out.display().to_string() })).unwrap();
    assert_eq!(app.jobs_waiting(), 1, "the export is in flight");
    std::fs::write(&path, "# Clean\n\nfrom elsewhere\n").unwrap();
    app.look_at_files();
    assert_eq!(app.jobs_waiting(), 1, "the read waits for the store");
    settle(&app);
    export.settle().expect("the export is answered");
    assert_eq!(file(&out), "# Clean\n\n");
    assert_eq!(text(&app), "# Clean\n\nfrom elsewhere\n");
    assert_eq!(notice(&app), said::UPDATED_FROM_DISK);
    assert_eq!(outside(&app), OutsideState::Clean);
}
