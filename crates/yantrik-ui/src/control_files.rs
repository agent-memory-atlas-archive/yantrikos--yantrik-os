//! The file browser, operable as data.
//!
//! The shell already *describes* the Files screen — path, entries, selection, free space — so an
//! agent can see what a directory holds without a screenshot. What it could not do was move: open
//! a folder, go up, make a directory, delete a file. Those are the other half of "operate the OS
//! as data", and the callbacks the file-browser UI drives are right there to reuse, so an agent
//! and a person navigate through exactly the same code.
//!
//! Every action here runs on the Files screen. If the shell is showing something else, the action
//! switches to it first, because operating the file browser *is* being on it, and a describe of
//! the result would report the wrong screen otherwise.

use slint::ComponentHandle;
use yantrik_app_runtime::control::{Action, App as ControlSurface, Param};

use crate::App;

/// The Files screen id.
const FILES_SCREEN: i32 = 8;

/// Put the shell on the Files screen if it is not already, so the browser state is live and a
/// following describe reports the directory rather than whatever else was up.
fn ensure_files_screen(ui: &App) {
    if ui.get_current_screen() != FILES_SCREEN {
        ui.set_current_screen(FILES_SCREEN);
        ui.invoke_navigate(FILES_SCREEN);
    }
}

/// The directory and how much it holds, after an action — enough for a caller to know where it
/// landed without a second round trip; the full listing is one `describe shell` away.
fn where_now(ui: &App) -> serde_json::Value {
    use slint::Model;
    let entries = ui.get_file_browser_entries();
    serde_json::json!({
        "path": ui.get_file_browser_path().to_string(),
        "entries": entries.row_count(),
        "free_space": ui.get_file_free_space_text().to_string(),
    })
}

/// Whether the current listing has an entry by this name, so an action can refuse a name that is
/// not there and say so, rather than invoke a callback that quietly does nothing.
fn has_entry(ui: &App, name: &str) -> bool {
    use slint::Model;
    let entries = ui.get_file_browser_entries();
    (0..entries.row_count())
        .filter_map(|i| entries.row_data(i))
        .any(|e| e.name == name)
}

/// Add the file-browser actions to the shell's control surface.
pub fn actions(surface: ControlSurface, ui: &App) -> ControlSurface {
    let for_go = ui.as_weak();
    let for_enter = ui.as_weak();
    let for_open = ui.as_weak();
    let for_up = ui.as_weak();
    let for_folder = ui.as_weak();
    let for_file = ui.as_weak();
    let for_delete = ui.as_weak();

    let up = |w: &slint::Weak<App>| w.upgrade().ok_or_else(|| "the shell is gone".to_string());

    surface
        .action(
            Action::new("files_go", "Open the Files screen at an absolute path")
                .arg(Param::text("path").describe("An absolute directory path, e.g. /home/user or /tmp")),
            move |args| {
                let ui = up(&for_go)?;
                let path = args["path"].as_str().unwrap_or_default().trim().to_string();
                if path.is_empty() {
                    return Err("`path` is empty".into());
                }
                ensure_files_screen(&ui);
                ui.invoke_file_navigate_to_path(path.clone().into());
                Ok(serde_json::json!({ "went_to": path, "now": where_now(&ui) }))
            },
        )
        .action(
            Action::new("files_enter", "Enter a subdirectory of the current one by name")
                .arg(Param::text("name").describe("A directory name shown in the current listing")),
            move |args| {
                let ui = up(&for_enter)?;
                let name = args["name"].as_str().unwrap_or_default().trim().to_string();
                ensure_files_screen(&ui);
                if !has_entry(&ui, &name) {
                    return Err(format!("nothing called `{name}` in {}", ui.get_file_browser_path()));
                }
                ui.invoke_file_navigate_dir(name.clone().into());
                Ok(serde_json::json!({ "entered": name, "now": where_now(&ui) }))
            },
        )
        .action(
            // Opening a file leaves the Files screen for a viewer/editor/player, which is the
            // point; the result says where it went so a caller is not surprised the next describe
            // is not the file browser.
            Action::new("files_open", "Open a file in the current directory (image, text, audio)")
                .arg(Param::text("name").describe("A file name shown in the current listing")),
            move |args| {
                let ui = up(&for_open)?;
                let name = args["name"].as_str().unwrap_or_default().trim().to_string();
                ensure_files_screen(&ui);
                if !has_entry(&ui, &name) {
                    return Err(format!("nothing called `{name}` in {}", ui.get_file_browser_path()));
                }
                ui.invoke_file_open(name.clone().into());
                Ok(serde_json::json!({ "opened": name, "screen": crate::control::screen_name(ui.get_current_screen()) }))
            },
        )
        .action(
            Action::new("files_up", "Go up to the parent directory"),
            move |_| {
                let ui = up(&for_up)?;
                ensure_files_screen(&ui);
                ui.invoke_file_go_up();
                Ok(serde_json::json!({ "now": where_now(&ui) }))
            },
        )
        .action(
            Action::new("files_new_folder", "Create a folder in the current directory")
                .arg(Param::text("name").describe("The new folder's name")),
            move |args| {
                let ui = up(&for_folder)?;
                let name = args["name"].as_str().unwrap_or_default().trim().to_string();
                if name.is_empty() {
                    return Err("`name` is empty".into());
                }
                ensure_files_screen(&ui);
                ui.invoke_file_create_folder(name.clone().into());
                Ok(serde_json::json!({ "created_folder": name, "now": where_now(&ui) }))
            },
        )
        .action(
            Action::new("files_new_file", "Create an empty file in the current directory")
                .arg(Param::text("name").describe("The new file's name")),
            move |args| {
                let ui = up(&for_file)?;
                let name = args["name"].as_str().unwrap_or_default().trim().to_string();
                if name.is_empty() {
                    return Err("`name` is empty".into());
                }
                ensure_files_screen(&ui);
                ui.invoke_file_create_file(name.clone().into());
                Ok(serde_json::json!({ "created_file": name, "now": where_now(&ui) }))
            },
        )
        .action(
            // Deleting a file is work a person cannot get back, so it is `dangerous`; the caller's
            // ceiling decides whether it is allowed.
            Action::new("files_delete", "Delete a file or folder in the current directory")
                .risk("dangerous")
                .arg(Param::text("name").describe("The name to delete, shown in the current listing")),
            move |args| {
                let ui = up(&for_delete)?;
                let name = args["name"].as_str().unwrap_or_default().trim().to_string();
                ensure_files_screen(&ui);
                if !has_entry(&ui, &name) {
                    return Err(format!("nothing called `{name}` in {}", ui.get_file_browser_path()));
                }
                ui.invoke_file_delete(name.clone().into());
                Ok(serde_json::json!({ "deleted": name, "now": where_now(&ui) }))
            },
        )
}
