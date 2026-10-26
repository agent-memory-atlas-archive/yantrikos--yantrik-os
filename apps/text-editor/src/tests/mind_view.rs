//! The Text Editor drawn for a Mind (Mind View): `read_mind_view` reads the window as text, so the
//! window holds no document text. File names stay; the field takes no typing; a search finds
//! nothing; and the gutter, colours and counts say nothing about the text.

use yantrik_app::for_mind::{HIDDEN, READ_ONLY};
use yantrik_app::mind::fake::FakeMind;
use yantrik_app::{App as _, Paths};
use yantrik_testing::TestApp;

use super::{opened, scratch, settle, typed};
use crate::command::Command;
use crate::TextEditor;

const SENTINEL: &str = "SENTINEL-editor-mind-view-3d9c";

/// A Text Editor drawn for a Mind (or not) with a file holding the sentinel open.
fn with_document(for_mind: bool) -> (TestApp<TextEditor>, std::path::PathBuf) {
    let paths = Paths::temporary(TextEditor::ID);
    let app = if for_mind {
        TestApp::<TextEditor>::start_for_mind(paths, Vec::new(), std::sync::Arc::new(FakeMind::new()))
    } else {
        TestApp::<TextEditor>::start_at(paths, Vec::new())
    };
    settle(&app);
    let path = scratch("mind-view").join("plan.rs");
    opened(&app, &path, &format!("fn main() {{\n    // {SENTINEL}\n    let s = \"{SENTINEL}\";\n}}\n"));
    (app, path)
}

/// Every string the window holds that a screen reader or OCR could see.
fn window_text(app: &TestApp<TextEditor>) -> String {
    app.ui(|ui| {
        let tabs = ui.get_tabs();
        let mut all = vec![
            ui.get_content().to_string(),
            ui.get_numbers().to_string(),
            ui.get_keywords().to_string(),
            ui.get_strings().to_string(),
            ui.get_comments().to_string(),
            ui.get_document_title().to_string(),
            ui.get_path_label().to_string(),
            ui.get_language().to_string(),
            ui.get_ending().to_string(),
            ui.get_view_status().to_string(),
            ui.get_notice().to_string(),
            ui.get_recovery_status().to_string(),
            ui.get_query().to_string(),
        ];
        all.extend((0..slint::Model::row_count(&tabs)).map(|i| slint::Model::row_data(&tabs, i).unwrap().title.to_string()));
        all.join("\n")
    })
}

#[test]
fn a_mind_view_window_holds_no_document_text_but_keeps_the_file_name() {
    let (app, _) = with_document(true);
    let text = window_text(&app);
    assert!(!text.contains(SENTINEL), "{text}");
    assert!(text.contains("plan.rs"), "the name stays: {text}");
    assert_eq!(app.ui(|ui| ui.get_content().to_string()), HIDDEN);
    assert!(app.ui(|ui| ui.get_masked()), "the field takes no keys");
    assert!(!app.ui(|ui| ui.get_has_highlights()), "and no colours spell the text");
    assert_eq!(app.ui(|ui| ui.get_line_count()), 0);
}

#[test]
fn a_normal_window_still_shows_the_document() {
    let (app, _) = with_document(false);
    assert!(window_text(&app).contains(SENTINEL));
    assert!(!app.ui(|ui| ui.get_masked()));
}

#[test]
fn typing_in_a_mind_view_window_changes_nothing() {
    let (app, path) = with_document(true);
    let before = super::file(&path);
    typed(&app, HIDDEN);
    assert_eq!(app.last_refusal().unwrap().sentence, READ_ONLY);
    assert!(!super::modified(&app));
    assert!(super::text(&app).contains(SENTINEL), "the document is as it was");
    assert_eq!(super::file(&path), before);
}

#[test]
fn a_search_in_a_mind_view_window_finds_nothing() {
    let (app, _) = with_document(true);
    app.run(Command::Find { query: SENTINEL.into(), match_case: false });
    assert_eq!(app.ui(|ui| ui.get_match_count()), 0);
    assert!(!window_text(&app).contains(SENTINEL), "and the query is the only place the word is");
}

#[test]
fn masking_is_visual_only_describe_and_calls_are_as_for_a_normal_instance() {
    let mut got = Vec::new();
    for for_mind in [false, true] {
        let (app, path) = with_document(for_mind);
        let d = app.describe();
        let read = super::act(&app, "read", super::none()).unwrap();
        let find = super::act(&app, "find", serde_json::json!({ "text": SENTINEL })).unwrap();
        // The scratch folder differs per run; the name of the file does not.
        let text = serde_json::json!([d["summary"], d["state"], read, find]).to_string();
        got.push(text.replace(&path.parent().unwrap().display().to_string(), "<dir>"));
    }
    assert!(got[0].contains(SENTINEL), "a Mind still reads the document through the calls");
    assert_eq!(got[0], got[1]);
}
