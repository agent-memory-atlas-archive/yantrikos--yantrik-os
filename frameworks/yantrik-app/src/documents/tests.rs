//! The document rules, each as a test (design/notes-gate-2026-10-21.md §2 and §3).

use std::path::PathBuf;

use super::close::{Next, Stage};
use super::disk::{Disk, FaultyDisk, RealDisk, Step};
use super::draft::recover;
use super::*;

fn fp(text: &str) -> Fingerprint {
    Fingerprint::of_bytes(text.as_bytes())
}

fn file(name: &str) -> Source {
    Source::File(PathBuf::from(format!("/notes/{name}.md")))
}

/// A table with one document read from `name` with `text`.
fn opened(name: &str, text: &str) -> (Documents<String>, DocumentId) {
    let mut docs = Documents::new();
    let id = docs.open(file(name), text.to_string(), Some(fp(text)));
    (docs, id)
}

fn ticket(begin: BeginSave<String>) -> SaveTicket<String> {
    match begin {
        BeginSave::Ticket(t) => t,
        other => panic!("expected a ticket, got {other:?}"),
    }
}

// ---- the table: one document per file, views share it ----

#[test]
fn one_file_opened_twice_is_one_document() {
    let (mut docs, id) = opened("a", "hello");
    let again = docs.open(file("a"), "stale copy".into(), Some(fp("stale copy")));
    assert_eq!(again, id);
    assert_eq!(docs.get(id).unwrap().content, "hello", "the open document is not replaced");
    let one = docs.show(id, WindowId(1));
    let two = docs.show(id, WindowId(2));
    assert_eq!(docs.views_of(id).count(), 2);
    docs.get_mut(id).unwrap().edit(|c| c.push('!'));
    // Both views read the one document: content and dirty state cannot diverge.
    for v in [one, two] {
        let shown = docs.view(v).unwrap().document;
        assert!(docs.get(shown).unwrap().dirty());
    }
}

#[test]
fn forgetting_the_last_view_hands_back_the_document() {
    let (mut docs, id) = opened("a", "x");
    let one = docs.show(id, WindowId(1));
    let two = docs.show(id, WindowId(2));
    assert_eq!(docs.forget_view(one), None, "another view still shows it");
    assert_eq!(docs.forget_view(two), Some(id));
    assert!(docs.close_document(id).is_some());
    assert!(docs.get(id).is_none());
}

#[test]
fn a_key_names_the_file_without_carrying_its_path() {
    let key = file("private-diary").key();
    assert!(key.starts_with("doc-"));
    assert!(!key.contains("diary") && !key.contains('/'));
    assert_eq!(key, file("private-diary").key(), "stable across runs");
    assert_ne!(key, file("other").key());
    assert_eq!(Source::New("7".into()).key(), "new-7");
}

// ---- saves: one in flight, one follow-up, no retry spam ----

#[test]
fn edits_during_a_save_owe_exactly_one_follow_up() {
    let (mut docs, id) = opened("a", "v0");
    let d = docs.get_mut(id).unwrap();
    d.edit(|c| *c = "v1".into());
    let first = ticket(d.begin_save());
    assert_eq!(first.content, "v1");
    d.edit(|c| *c = "v2".into());
    assert!(matches!(d.begin_save(), BeginSave::InFlight));
    d.edit(|c| *c = "v3".into());
    assert!(matches!(d.begin_save(), BeginSave::InFlight), "still one in flight, not a queue");
    let follow = match d.save_landed(first.seq, Ok(fp("v1"))) {
        SaveOutcome::FollowUp(t) => t,
        other => panic!("expected a follow-up, got {other:?}"),
    };
    assert_eq!(follow.content, "v3", "the follow-up saves the newest edits, once");
    assert_eq!(follow.baseline, Some(fp("v1")), "and expects our own write on disk");
    assert!(matches!(d.save_landed(follow.seq, Ok(fp("v3"))), SaveOutcome::Saved));
    assert!(!d.dirty());
    assert!(matches!(d.begin_save(), BeginSave::Clean));
}

#[test]
fn an_outcome_for_a_save_no_longer_expected_changes_nothing() {
    let (mut docs, id) = opened("a", "v0");
    let d = docs.get_mut(id).unwrap();
    d.edit(|c| *c = "v1".into());
    let t = ticket(d.begin_save());
    assert!(matches!(d.save_landed(t.seq + 5, Ok(fp("zzz"))), SaveOutcome::Stale));
    assert!(matches!(d.save, SaveState::Saving { .. }));
    assert_eq!(d.baseline, Some(fp("v0")));
}

#[test]
fn a_failed_save_is_not_retried_by_autosave_until_something_changes() {
    let (mut docs, id) = opened("a", "v0");
    let d = docs.get_mut(id).unwrap();
    d.edit(|c| *c = "v1".into());
    let t = ticket(d.begin_save());
    let failed = SaveError::new(Durability::NotWritten, "The disk is read-only.");
    assert!(matches!(d.save_landed(t.seq, Err(failed.clone())), SaveOutcome::Failed));
    assert!(d.dirty(), "nothing was saved");
    assert!(matches!(d.begin_save(), BeginSave::Failed), "an autosave tick does not spam");
    assert!(matches!(d.begin_save(), BeginSave::Failed));
    // The person asks: retried.
    let retry = ticket(d.retry_save());
    assert_eq!(retry.seq, t.seq);
    assert!(matches!(d.save_landed(retry.seq, Err(failed)), SaveOutcome::Failed));
    // A new edit is new content: autosave tries it.
    d.edit(|c| *c = "v2".into());
    assert_eq!(ticket(d.begin_save()).content, "v2");
}

#[test]
fn a_failure_after_the_rename_is_not_called_saved() {
    let (mut docs, id) = opened("a", "v0");
    let d = docs.get_mut(id).unwrap();
    d.edit(|c| *c = "v1".into());
    let t = ticket(d.begin_save());
    let unknown = SaveError::new(Step::SyncDir.durability(), "Saved, but the disk did not confirm it.");
    d.save_landed(t.seq, Err(unknown));
    assert!(d.dirty(), "Unknown durability is treated as not saved");
    match &d.save {
        SaveState::Failed { error, .. } => assert_eq!(error.durability, Durability::Unknown),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_save_that_finds_the_file_changed_becomes_a_conflict_and_blocks_saves() {
    let (mut docs, id) = opened("a", "v0");
    let d = docs.get_mut(id).unwrap();
    d.edit(|c| *c = "mine".into());
    let t = ticket(d.begin_save());
    d.save_landed(t.seq, Err(SaveError::conflict("The note changed on disk.")));
    assert_eq!(d.outside, OutsideState::Conflict { now: None });
    assert!(matches!(d.begin_save(), BeginSave::Blocked), "not Failed: the person has a choice");
    assert!(matches!(d.retry_save(), BeginSave::Blocked), "the person chooses first");
}

// ---- outside changes: classified, our own writes recognised ----

#[test]
fn our_own_write_seen_by_the_watch_is_not_an_outside_edit() {
    let (mut docs, id) = opened("a", "v0");
    let d = docs.get_mut(id).unwrap();
    d.edit(|c| *c = "v1".into());
    let t = ticket(d.begin_save());
    d.save_landed(t.seq, Ok(fp("v1")));
    assert_eq!(d.observe(Observed::File(fp("v1"))), &OutsideState::Clean);
    // A watch tick late enough to see the write before ours landed again: still ours.
    d.edit(|c| *c = "v2".into());
    let t = ticket(d.begin_save());
    d.save_landed(t.seq, Ok(fp("v2")));
    assert_eq!(d.observe(Observed::File(fp("v1"))), &OutsideState::Clean);
}

#[test]
fn an_outside_edit_to_a_clean_document_is_offered() {
    let (mut docs, id) = opened("a", "v0");
    let d = docs.get_mut(id).unwrap();
    assert_eq!(d.observe(Observed::File(fp("theirs"))), &OutsideState::Changed { now: fp("theirs") });
    d.take_theirs("theirs".into(), fp("theirs"));
    assert_eq!(d.outside, OutsideState::Clean);
    assert!(!d.dirty());
    assert_eq!(d.baseline, Some(fp("theirs")));
}

#[test]
fn an_outside_edit_to_a_dirty_document_is_a_conflict_never_last_writer_wins() {
    let (mut docs, id) = opened("a", "v0");
    let d = docs.get_mut(id).unwrap();
    d.edit(|c| *c = "mine".into());
    assert_eq!(d.observe(Observed::File(fp("theirs"))), &OutsideState::Conflict { now: Some(fp("theirs")) });
    assert!(matches!(d.begin_save(), BeginSave::Blocked));
    // Seeing the old file again does not clear a conflict on its own.
    assert!(matches!(d.observe(Observed::File(fp("v0"))), OutsideState::Conflict { .. }));
    let mine = d.keep_mine_as_copy("theirs".into(), fp("theirs"));
    assert_eq!(mine, "mine", "the person's text is handed back to be saved as a copy");
    assert_eq!(d.content, "theirs");
    assert_eq!(d.outside, OutsideState::Clean);
}

#[test]
fn a_deleted_file_is_said_and_a_save_becomes_save_as_new() {
    let (mut docs, id) = opened("a", "v0");
    let d = docs.get_mut(id).unwrap();
    d.edit(|c| *c = "keep me".into());
    assert_eq!(d.observe(Observed::Gone), &OutsideState::Deleted);
    assert!(matches!(d.begin_save(), BeginSave::Blocked));
    d.save_as_new("recovered-1");
    let t = ticket(d.begin_save());
    assert_eq!(t.baseline, None, "a new file is created, not one replaced");
    assert_eq!(t.content, "keep me");
}

#[test]
fn a_document_never_saved_is_not_deleted_by_an_absent_file() {
    let mut docs: Documents<String> = Documents::new();
    let id = docs.open(Source::New("1".into()), String::new(), None);
    let d = docs.get_mut(id).unwrap();
    assert_eq!(d.observe(Observed::Gone), &OutsideState::Clean);
}

// ---- a change seen during a save: three outcomes ----

#[test]
fn during_a_save_the_watch_seeing_our_write_is_ours() {
    let (mut docs, id) = opened("a", "v0");
    let d = docs.get_mut(id).unwrap();
    d.edit(|c| *c = "v1".into());
    let t = ticket(d.begin_save());
    d.observe(Observed::File(fp("v1")));
    assert_eq!(d.outside, OutsideState::Clean, "judged when the save lands, not now");
    assert!(matches!(d.save_landed(t.seq, Ok(fp("v1"))), SaveOutcome::Saved));
    assert_eq!(d.outside, OutsideState::Clean);
}

#[test]
fn during_a_save_an_outside_write_that_ours_replaced_is_said() {
    let (mut docs, id) = opened("a", "v0");
    let d = docs.get_mut(id).unwrap();
    d.edit(|c| *c = "v1".into());
    let t = ticket(d.begin_save());
    d.observe(Observed::File(fp("theirs")));
    assert!(matches!(d.save_landed(t.seq, Ok(fp("v1"))), SaveOutcome::OverwroteOutsideEdit));
}

#[test]
fn during_a_save_that_fails_an_outside_write_is_judged_against_the_old_file() {
    let (mut docs, id) = opened("a", "v0");
    let d = docs.get_mut(id).unwrap();
    d.edit(|c| *c = "v1".into());
    let t = ticket(d.begin_save());
    d.observe(Observed::File(fp("theirs")));
    d.save_landed(t.seq, Err(SaveError::new(Durability::NotWritten, "Disk full.")));
    assert_eq!(d.outside, OutsideState::Conflict { now: Some(fp("theirs")) });
}

// ---- drafts ----

#[test]
fn a_draft_is_owed_only_for_edits_newer_than_the_last_one_kept() {
    let (mut docs, id) = opened("a", "v0");
    let d = docs.get_mut(id).unwrap();
    assert!(d.draft_owed(100).is_none(), "a clean document keeps no draft");
    d.edit(|c| *c = "v1".into());
    let (name, bytes) = d.draft_owed(100).expect("owed");
    assert_eq!(name, d.source.key());
    let envelope: Envelope<String> = serde_json::from_slice(&bytes).unwrap();
    assert_eq!((envelope.seq, envelope.content.as_str(), envelope.at), (d.seq, "v1", 100));
    assert_eq!(envelope.path.as_deref(), Some("/notes/a.md"));
    d.draft_asked();
    assert!(d.draft_owed(101).is_none());
    d.edit(|c| *c = "v2".into());
    assert!(d.draft_owed(102).is_some());
}

#[test]
fn a_draft_is_superseded_once_a_save_of_its_edits_commits() {
    let (mut docs, id) = opened("a", "v0");
    let d = docs.get_mut(id).unwrap();
    d.edit(|c| *c = "v1".into());
    d.draft_asked();
    let t = ticket(d.begin_save());
    d.edit(|c| *c = "v2".into());
    d.draft_asked();
    assert!(matches!(d.begin_save(), BeginSave::InFlight), "the autosave tick asks again");
    let follow = match d.save_landed(t.seq, Ok(fp("v1"))) {
        SaveOutcome::FollowUp(f) => f,
        other => panic!("{other:?}"),
    };
    assert!(!d.draft_superseded(), "the draft holds v2, which is not saved yet");
    d.save_landed(follow.seq, Ok(fp("v2")));
    assert!(d.draft_superseded());
}

#[test]
fn recovery_offers_only_what_differs_from_the_file_and_the_newest_wins() {
    let on_disk = "saved".to_string();
    let file = Some((&on_disk, 10));
    assert_eq!(recover(file, None, None), Recovered::File);
    assert_eq!(recover(file, Some(("saved".into(), 20)), Some(("saved".into(), 30))), Recovered::File);
    assert_eq!(recover(file, Some(("journal".into(), 20)), None), Recovered::Journal("journal".into()));
    assert_eq!(recover(file, None, Some(("draft".into(), 20))), Recovered::Draft("draft".into()));
    assert_eq!(
        recover(file, Some(("journal".into(), 40)), Some(("draft".into(), 30))),
        Recovered::Journal("journal".into())
    );
    assert_eq!(
        recover(file, Some(("journal".into(), 30)), Some(("draft".into(), 30))),
        Recovered::Draft("draft".into()),
        "a tie goes to the draft"
    );
    assert_eq!(recover::<String>(None, None, Some(("new".into(), 1))), Recovered::Draft("new".into()));
}

// ---- close as a document flow ----

#[test]
fn closing_a_view_asks_only_for_the_last_view_on_a_dirty_document() {
    let (mut docs, id) = opened("a", "v0");
    let one = docs.show(id, WindowId(1));
    let two = docs.show(id, WindowId(2));
    assert_eq!(docs.closing_view(one), Closing::Now, "clean");
    docs.get_mut(id).unwrap().edit(|c| c.push('!'));
    assert_eq!(docs.closing_view(one), Closing::Now, "another view still holds it");
    docs.forget_view(one);
    assert_eq!(docs.closing_view(two), Closing::Ask(vec![id]));
}

#[test]
fn closing_a_window_asks_about_what_no_other_window_shows_and_quit_asks_once() {
    let mut docs: Documents<String> = Documents::new();
    let a = docs.open(file("a"), "a".into(), Some(fp("a")));
    let b = docs.open(file("b"), "b".into(), Some(fp("b")));
    let c = docs.open(file("c"), "c".into(), Some(fp("c")));
    docs.show(a, WindowId(1));
    docs.show(b, WindowId(1));
    docs.show(b, WindowId(2));
    docs.show(c, WindowId(2));
    for id in [a, b, c] {
        docs.get_mut(id).unwrap().edit(|t| t.push('!'));
    }
    assert_eq!(docs.closing_window(WindowId(1)), Closing::Ask(vec![a]), "b is still in window 2");
    assert_eq!(docs.quitting(), Closing::Ask(vec![a, b, c]), "every dirty document, once");
}

#[test]
fn save_then_close_waits_for_edits_made_meanwhile() {
    let (mut docs, id) = opened("a", "v0");
    docs.show(id, WindowId(1));
    docs.get_mut(id).unwrap().edit(|c| *c = "v1".into());
    let mut flow = CloseFlow::new(Some(WindowId(1)), vec![id]);
    assert_eq!(flow.answer(Choice::Save), Next::Save(vec![id]));
    assert_eq!(flow.answer(Choice::Save), Next::Wait, "a second close while saving changes nothing");
    let t = ticket(docs.get_mut(id).unwrap().begin_save());
    docs.get_mut(id).unwrap().edit(|c| *c = "v2".into());
    assert!(matches!(docs.get_mut(id).unwrap().save_landed(t.seq, Ok(fp("v1"))), SaveOutcome::Saved));
    assert_eq!(flow.progress(&docs, None), Next::Save(vec![id]), "v2 is not saved yet");
    let again = ticket(docs.get_mut(id).unwrap().begin_save());
    assert_eq!(again.content, "v2");
    docs.get_mut(id).unwrap().save_landed(again.seq, Ok(fp("v2")));
    assert_eq!(flow.progress(&docs, None), Next::Close { discard: vec![] });
}

#[test]
fn a_failed_save_while_closing_keeps_the_window() {
    let (docs, id) = opened("a", "v0");
    let mut flow = CloseFlow::new(Some(WindowId(1)), vec![id]);
    flow.answer(Choice::Save);
    assert_eq!(flow.progress(&docs, Some("Disk full.".into())), Next::Stay);
    assert_eq!(flow.stage, Stage::Blocked("Disk full.".into()));
}

#[test]
fn dont_save_discards_and_cancel_stays() {
    let (_, id) = opened("a", "v0");
    let mut flow = CloseFlow::new(None, vec![id]);
    assert_eq!(flow.answer(Choice::Cancel), Next::Stay);
    assert_eq!(flow.answer(Choice::DontSave), Next::Close { discard: vec![id] });
}

// ---- the disk seam ----

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("yantrik-documents-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_failure_before_the_rename_leaves_the_file_as_it_was() {
    let dir = scratch("before-rename");
    let path = dir.join("note.md");
    RealDisk.write_atomically(&path, b"old").unwrap();
    let disk = FaultyDisk::new();
    for step in [Step::CreateTemp, Step::Write, Step::SyncFile, Step::Rename] {
        disk.fail(step, "note.md", Some(1));
        let err = disk.write_atomically(&path, b"new").unwrap_err();
        assert_eq!(err.step, step);
        assert_eq!(step.durability(), Durability::NotWritten);
        assert_eq!(std::fs::read(&path).unwrap(), b"old");
    }
    let leftovers: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).collect();
    assert_eq!(leftovers.len(), 1, "no temp file is left behind");
    disk.write_atomically(&path, b"new").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"new", "a fault used up, the disk behaves");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failure_after_the_rename_has_the_bytes_but_not_their_durability() {
    let dir = scratch("after-rename");
    let path = dir.join("note.md");
    let disk = FaultyDisk::new();
    disk.fail(Step::SyncDir, ".md", None);
    let err = disk.write_atomically(&path, b"new").unwrap_err();
    assert_eq!(err.step.durability(), Durability::Unknown);
    assert_eq!(std::fs::read(&path).unwrap(), b"new");
    // A fault scoped to one suffix spares the rest: metadata and content fail apart.
    RealDisk.write_atomically(&dir.join("note.meta"), b"m").unwrap();
    assert!(disk.write_atomically(&dir.join("note.meta"), b"m2").is_ok());
    assert_eq!(disk.hits.lock().unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- the watched files ----

#[test]
fn the_watch_reports_a_change_from_the_baseline_once_and_a_deletion() {
    let dir = scratch("watched");
    let path = dir.join("note.md");
    std::fs::write(&path, "v0").unwrap();
    let watched = Watched::new();
    watched.add("doc-1", path.clone(), Some(Observed::File(fp("v0"))));
    assert!(watched.poll().is_empty(), "the file it was read from is not a change");
    std::fs::write(&path, "theirs").unwrap();
    assert_eq!(watched.poll(), vec![("doc-1".to_string(), Observed::File(fp("theirs")))]);
    assert!(watched.poll().is_empty(), "reported once");
    std::fs::remove_file(&path).unwrap();
    assert_eq!(watched.poll(), vec![("doc-1".to_string(), Observed::Gone)]);
    watched.remove("doc-1");
    std::fs::write(&path, "back").unwrap();
    assert!(watched.poll().is_empty(), "an unwatched file is not reported");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_file_that_cannot_be_looked_at_is_not_a_change() {
    let dir = scratch("unreadable");
    // A path under a plain file cannot be looked at (not a directory): skipped, not "gone".
    std::fs::write(dir.join("plain"), "a file, not a directory").unwrap();
    let watched = Watched::new();
    watched.add("doc-1", dir.join("plain").join("note.md"), Some(Observed::File(fp("v0"))));
    assert!(watched.poll().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}
