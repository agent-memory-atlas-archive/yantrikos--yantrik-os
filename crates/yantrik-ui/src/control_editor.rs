//! The text editor, operable as data.
//!
//! With the file browser an agent can create an empty file; with this it can write one. The
//! editor already holds the document — content, name, modified flag — and already knows how to
//! save it; this exposes the three verbs that were missing over the socket: start a document,
//! set its text, and write it to disk. Together with `files_*` it closes the loop an agent needs
//! most often — make a file, put something in it, save it — without a screenshot or a keystroke.

use slint::ComponentHandle;
use yantrik_app_runtime::control::{Action, App as ControlSurface, Param};

use crate::App;

/// The text-editor screen id.
const EDITOR_SCREEN: i32 = 12;

/// How much of the document `describe` carries: a generous preview, not the whole file. A model
/// asking "what is in this editor" wants to see it, but a megabyte of source is the transcript
/// the control surface exists to avoid; the true length travels beside the preview.
const CONTENT_PREVIEW: usize = 4000;

fn ensure_editor_screen(ui: &App) {
    if ui.get_current_screen() != EDITOR_SCREEN {
        ui.set_current_screen(EDITOR_SCREEN);
        ui.invoke_navigate(EDITOR_SCREEN);
    }
}

/// The open document as data: name, a preview, and the counts a caller reasons about.
pub fn state(ui: &App) -> serde_json::Value {
    let content = ui.get_editor_file_content().to_string();
    let chars = content.chars().count();
    let lines = content.lines().count().max(if content.is_empty() { 0 } else { 1 });
    let preview: String = if chars > CONTENT_PREVIEW {
        content.chars().take(CONTENT_PREVIEW).collect()
    } else {
        content.clone()
    };
    serde_json::json!({
        "name": ui.get_editor_file_name().to_string(),
        "modified": ui.get_editor_is_modified(),
        "readonly": ui.get_editor_is_readonly(),
        "chars": chars,
        "lines": lines,
        "content": preview,
        "content_truncated": chars > CONTENT_PREVIEW,
    })
}

/// One line for the summary when the editor is up.
pub fn summary(ui: &App) -> String {
    let name = ui.get_editor_file_name();
    let name = if name.is_empty() { "untitled".to_string() } else { name.to_string() };
    let modified = if ui.get_editor_is_modified() { ", unsaved" } else { "" };
    let words = ui.get_editor_file_content().split_whitespace().count();
    format!("Yantrik — editing \"{name}\", {words} words{modified}")
}

/// Add the editor actions to the shell's control surface.
pub fn actions(surface: ControlSurface, ui: &App) -> ControlSurface {
    let for_new = ui.as_weak();
    let for_set = ui.as_weak();
    let for_append = ui.as_weak();
    let for_save = ui.as_weak();
    let for_saveas = ui.as_weak();
    let up = |w: &slint::Weak<App>| w.upgrade().ok_or_else(|| "the shell is gone".to_string());

    surface
        .action(
            Action::new("editor_new", "Open the text editor with a fresh, empty document"),
            move |_| {
                let ui = up(&for_new)?;
                ensure_editor_screen(&ui);
                ui.invoke_editor_new_tab();
                Ok(serde_json::json!({ "editor": state(&ui) }))
            },
        )
        .action(
            Action::new("editor_set_content", "Replace the whole document with this text")
                .arg(Param::text("text").describe("The full new contents of the document")),
            move |args| {
                let ui = up(&for_set)?;
                let text = args["text"].as_str().unwrap_or_default().to_string();
                ensure_editor_screen(&ui);
                ui.set_editor_file_content(text.clone().into());
                ui.invoke_editor_content_changed(ui.get_editor_file_content());
                Ok(serde_json::json!({ "editor": state(&ui) }))
            },
        )
        .action(
            Action::new("editor_append", "Add text to the end of the document")
                .arg(Param::text("text").describe("Text to append")),
            move |args| {
                let ui = up(&for_append)?;
                let add = args["text"].as_str().unwrap_or_default();
                ensure_editor_screen(&ui);
                let mut content = ui.get_editor_file_content().to_string();
                content.push_str(add);
                ui.set_editor_file_content(content.into());
                ui.invoke_editor_content_changed(ui.get_editor_file_content());
                Ok(serde_json::json!({ "editor": state(&ui) }))
            },
        )
        .action(
            // Deferred is not needed — the save runs and returns — but it can *fail to happen*:
            // a document with no path pops the Save As dialog and writes nothing, so the honest
            // answer there is "not saved, name it" rather than a success.
            Action::new("editor_save", "Save the document to the file it came from"),
            move |_| {
                let ui = up(&for_save)?;
                ui.invoke_editor_save();
                if !ui.get_editor_save_error().is_empty() { return Err(ui.get_editor_save_error().to_string()); }
                if ui.get_editor_show_save_dialog() {
                    // Untitled: the UI is now asking for a name. Close it and tell the caller how.
                    ui.set_editor_show_save_dialog(false);
                    return Err(
                        "this document has no file yet — use editor_save_as with a path".into(),
                    );
                }
                Ok(serde_json::json!({
                    "saved": ui.get_editor_file_name().to_string(),
                    "editor": state(&ui),
                }))
            },
        )
        .action(
            Action::new("editor_save_as", "Write the document to a specific path")
                .arg(Param::text("path").describe("An absolute file path, e.g. /home/user/notes.txt")),
            move |args| {
                let ui = up(&for_saveas)?;
                let path = args["path"].as_str().unwrap_or_default().trim().to_string();
                if path.is_empty() {
                    return Err("`path` is empty".into());
                }
                let p = std::path::Path::new(&path);
                let filename = match p.file_name().and_then(|f| f.to_str()) {
                    Some(f) if !f.is_empty() => f.to_string(),
                    _ => return Err(format!("`{path}` has no file name")),
                };
                let dir = p
                    .parent()
                    .map(|d| d.to_string_lossy().to_string())
                    .filter(|d| !d.is_empty())
                    .unwrap_or_else(|| ".".to_string());
                ensure_editor_screen(&ui);
                ui.invoke_editor_save_as(dir.clone().into(), filename.clone().into());
                // The save-as handler reports a bad directory through this field rather than
                // failing; surface it as the error it is.
                let err = ui.get_editor_save_error().to_string();
                if !err.is_empty() {
                    return Err(err);
                }
                Ok(serde_json::json!({ "saved_to": path, "editor": state(&ui) }))
            },
        )
}
