//! The document's file changed outside yDoc, as the watch sees it: never last-writer-wins.
//! (Text Editor's handling, adopted: the old build only followed a move, and found any other
//! change when a save refused.)

use serde_json::json;

use super::{act, file, modified, none, notice, outside_writes, path_of, press, settle, state, text, typed, with_file};
use crate::command::WindowAction;

#[test]
fn an_outside_edit_to_a_document_with_nothing_unsaved_is_followed() {
    let (app, path) = with_file("follow", "first\n");
    outside_writes(&app, &path, "theirs\n");
    assert_eq!(text(&app), "theirs\n", "the document shows the file as it is");
    assert!(!modified(&app));
    assert!(notice(&app).contains("changed outside yDoc; the document now shows the file"), "{}", notice(&app));
    assert_eq!(app.ui(|ui| ui.get_doc_content().to_string()), "theirs\n", "and so does the page");
}

#[test]
fn our_own_save_is_not_an_outside_edit() {
    let (app, path) = with_file("ours", "first\n");
    typed(&app, "second\n");
    press(&app, WindowAction::Save);
    app.look_at_files();
    settle(&app);
    assert_eq!(notice(&app), "");
    assert_eq!((text(&app), file(&path)), ("second\n".into(), "second\n".into()));
}

#[test]
fn an_outside_edit_under_unsaved_changes_is_a_conflict_said_at_once_and_never_written_over() {
    let (app, path) = with_file("conflict-seen", "first\n");
    typed(&app, "mine\n");
    outside_writes(&app, &path, "theirs\n");
    assert!(notice(&app).contains("changed outside yDoc while this document has unsaved changes"), "{}", notice(&app));
    assert_eq!(text(&app), "mine\n", "the person's text stays");
    let refused = act(&app, "save", none()).unwrap_err();
    assert!(refused.contains("changed on disk"), "{refused}");
    assert_eq!(file(&path), "theirs\n");
    // The other version put back by its program: the save may go ahead again.
    outside_writes(&app, &path, "first\n");
    act(&app, "save", none()).expect("the file holds what the document last read again");
    assert_eq!(file(&path), "mine\n");
}

#[test]
fn a_file_deleted_outside_makes_its_text_unsaved_and_the_close_asks() {
    let (app, path) = with_file("gone", "only copy\n");
    assert!(!modified(&app));
    std::fs::remove_file(&path).unwrap();
    app.look_at_files();
    settle(&app);
    assert!(modified(&app), "its text is on no disk now");
    assert!(notice(&app).contains("is not there any more"), "{}", notice(&app));
    app.run_writes();
    assert!(super::draft_file(&app).exists(), "the text is in its draft");
    app.close_requested(yantrik_app::app::WindowId::PRIMARY);
    assert_eq!(state(&app)["prompt_open"], true, "closing asks rather than drop it");
    assert!(app.exit_reason().is_none());
}

#[test]
fn a_file_renamed_while_open_is_followed_to_its_new_name() {
    let (app, from) = with_file("renamed", "body\n");
    let to = from.with_file_name("pricing.md");
    std::fs::rename(&from, &to).unwrap();
    app.look_at_files();
    settle(&app);
    assert_eq!(path_of(&app), Some(to.clone()));
    assert_eq!(notice(&app), format!("This document moved. It is {} now.", to.display()));
    assert!(!modified(&app));
    typed(&app, "body, edited\n");
    press(&app, WindowAction::Save);
    assert_eq!(file(&to), "body, edited\n");
    assert!(!from.exists(), "nothing was written where it used to be");
    assert_eq!(state(&app)["path"], json!(to));
}

#[test]
fn a_save_to_a_moved_file_says_where_it_went_and_save_as_there_keeps_the_work() {
    let (app, from) = with_file("stranded", "body\n");
    typed(&app, "body, edited\n");
    let to = from.with_file_name("elsewhere.md");
    std::fs::rename(&from, &to).unwrap();
    // The watch has not looked yet: the save itself goes looking.
    let refused = act(&app, "save", none()).unwrap_err();
    assert!(refused.contains("is not there any more"), "{refused}");
    assert!(refused.contains("Save As"), "a refusal with a next step in it: {refused}");
    assert_eq!(text(&app), "body, edited\n");
}

#[test]
fn a_document_whose_file_was_deleted_never_traps_the_person_in_the_prompt() {
    let (app, path) = with_file("deleted-prompt", "only copy\n");
    std::fs::remove_file(&path).unwrap();
    app.look_at_files();
    settle(&app);
    app.close_requested(yantrik_app::app::WindowId::PRIMARY);
    // Save cannot write into a file that is not there: it asks where to put the text instead.
    press(&app, WindowAction::Confirm { path: String::new() });
    assert_eq!(app.ui(|ui| ui.get_doc_dialog()), 2, "Save As, from the prompt");
    let kept = path.with_file_name("kept.md");
    press(&app, WindowAction::Confirm { path: kept.display().to_string() });
    assert_eq!(file(&kept), "only copy\n");
    assert_eq!(app.exit_reason(), Some(yantrik_app::app::ExitReason::LastWindowClosed));
}
