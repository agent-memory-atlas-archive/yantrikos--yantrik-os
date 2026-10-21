//! The window: its callbacks bound to commands, and how it is drawn.

pub mod listing;
pub mod preview;
pub mod render;

use slint::{ComponentHandle as _, SharedString};
use yantrik_app::cx::Cx;

use crate::command::{Command, WindowAction};
use crate::{Notes, NotesApp};

/// Binds the window's callbacks. Where a callback carries no value, the window's own fields are
/// read here and the command carries them: the model is told what the window shows, and never
/// reads the window itself.
pub fn wire(cx: &Cx<Notes>, ui: &NotesApp) {
    let (run, weak) = (cx.clone(), ui.as_weak());
    ui.on_action(move |id: SharedString| {
        let path = || weak.upgrade().map(|ui| ui.get_path().to_string()).unwrap_or_default();
        if let Some(action) = WindowAction::parse(&id, path) {
            run.run(Command::Window(action));
        }
    });
    ui.on_choose(cx.cmd1(|id: SharedString| Command::Window(WindowAction::open(&id))));
    ui.on_edited(cx.cmd1(|text: SharedString| Command::Edit(text.to_string())));
    let (run, weak) = (cx.clone(), ui.as_weak());
    ui.on_filter(move || {
        if let Some(ui) = weak.upgrade() {
            run.run(Command::Filter { folder: ui.get_folder().to_string(), query: ui.get_query().to_string() });
        }
    });
    let (run, weak) = (cx.clone(), ui.as_weak());
    ui.on_metadata(move || {
        if let Some(ui) = weak.upgrade() {
            run.run(Command::Metadata { notebook: ui.get_notebook().to_string(), tags: ui.get_tags().to_string() });
        }
    });
    let (run, weak) = (cx.clone(), ui.as_weak());
    ui.on_view_changed(move || {
        if let Some(ui) = weak.upgrade() {
            run.run(Command::ViewSynced { focus_mode: ui.get_focus_mode(), dialog: ui.get_dialog() });
        }
    });
}
