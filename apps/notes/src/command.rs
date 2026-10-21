//! Every way the model changes: what the window, the surface, the timers and the store's jobs ask.

use yantrik_app::app::WindowId;
use yantrik_app::documents::{DocumentId, Observed};

use crate::note::Note;
use crate::store::{Loaded, SaveFailed, Saved};

#[derive(Clone)]
pub enum Command {
    /// Read the library (at start, and `reload`).
    Load,
    /// One of the window's actions, by the id its button or key sends.
    Window(WindowAction),
    /// The person typed in the editor; the editor already shows this text.
    Edit(String),
    /// The notebook or tags field changed; these are what they now say.
    Metadata { notebook: String, tags: String },
    /// The library's section or search changed in the window.
    Filter { folder: String, query: String },
    /// The window changed its own focus mode or closed its dialog (Escape, Cancel, Ctrl+F).
    ViewSynced { focus_mode: bool, dialog: i32 },
    /// A call from the surface (`app.act`).
    Surface(Act),
    /// The autosave's debounce ran out.
    Autosave,
    CloseRequested(WindowId),
    /// The watch saw an open note's file change.
    FileChanged { key: String, seen: Observed },
    // What the store's jobs found.
    Loaded(Result<Loaded, String>),
    SaveLanded { doc: DocumentId, seq: u64, result: Result<Saved, SaveFailed> },
    Trashed { id: String, result: Result<(), String> },
    Restored(Result<Note, String>),
    Imported(Result<Note, String>),
    Exported(Result<(), String>),
    /// The disk the vault writes through, for a test to make a write fail.
    #[cfg(test)]
    UseDisk(std::sync::Arc<dyn yantrik_app::documents::disk::Disk>),
}

/// The window's actions: its header, its keys, its buttons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowAction {
    Focus,
    Preview,
    Undo,
    Redo,
    Save,
    Reload,
    Pin,
    Trash,
    Restore,
    Copy,
    Import,
    Export,
    /// The dialog's Continue, with the path its field holds.
    Confirm { path: String },
    /// A new note, titled or Untitled.
    New(Option<String>),
    /// Open a library row: its file name, and whether it is in the Trash.
    Open { id: String, trash: bool },
}

impl WindowAction {
    /// The action an id names; `path` is the dialog's field, for `confirm`. `None` for an id the
    /// window does not send.
    pub fn parse(id: &str, path: impl FnOnce() -> String) -> Option<WindowAction> {
        Some(match id {
            "focus" => WindowAction::Focus,
            "preview" => WindowAction::Preview,
            "undo" => WindowAction::Undo,
            "redo" => WindowAction::Redo,
            "save" => WindowAction::Save,
            "reload" => WindowAction::Reload,
            "pin" => WindowAction::Pin,
            "trash" => WindowAction::Trash,
            "restore" => WindowAction::Restore,
            "copy" => WindowAction::Copy,
            "import" => WindowAction::Import,
            "export" => WindowAction::Export,
            "confirm" => WindowAction::Confirm { path: path() },
            "new" => WindowAction::New(None),
            other => {
                if let Some(title) = other.strip_prefix("new:") {
                    WindowAction::New(Some(title.into()))
                } else {
                    WindowAction::open(other.strip_prefix("open:")?)
                }
            }
        })
    }

    /// A library row's id: `trash:<file>` for a note in the Trash.
    pub fn open(key: &str) -> WindowAction {
        match key.strip_prefix("trash:") {
            Some(id) => WindowAction::Open { id: id.into(), trash: true },
            None => WindowAction::Open { id: key.into(), trash: false },
        }
    }

    /// Whether this changes the library, so a library open read-only refuses it. The dialog's
    /// Continue depends on which dialog is open, so the guard decides it (`update::read_only`).
    pub fn writes(&self) -> bool {
        matches!(
            self,
            WindowAction::Save
                | WindowAction::Pin
                | WindowAction::Trash
                | WindowAction::Restore
                | WindowAction::Copy
                | WindowAction::Import
                | WindowAction::New(_)
                | WindowAction::Undo
                | WindowAction::Redo
        )
    }

    /// Whether this leaves the open note, so its edits are saved first.
    pub fn navigates(&self) -> bool {
        matches!(
            self,
            WindowAction::New(_)
                | WindowAction::Open { .. }
                | WindowAction::Reload
                | WindowAction::Trash
                | WindowAction::Import
        )
    }
}

/// What runs once the open note is saved.
#[derive(Debug, Clone)]
pub enum Step {
    Window(WindowAction),
    /// The surface's `new_note`: a title and a body.
    NewNote { title: String, body: String },
    Import { path: String },
    Export { path: String },
}

/// A call from the surface, its arguments read.
#[derive(Debug, Clone)]
pub enum Act {
    NewNote { title: String, body: String },
    OpenNote { wanted: String },
    SetTitle(String),
    Append(String),
    SetContent(String),
    Search(String),
    ReadNotes { query: String, skip: usize },
    SetFolder(String),
    Notebook(String),
    Tags(String),
    Save,
    Trash,
    Restore,
    Copy,
    Reload,
    Preview,
    Focus,
    Undo,
    Redo,
    Import { path: String },
    Export { path: String },
    /// A required argument given empty: refused, in the window's notice too.
    Refuse(String),
}

impl Act {
    /// Whether this call changes the library, so a library open read-only refuses it. An export
    /// does not: it writes outside the vault, and is the way a read-only library's notes get out.
    pub fn writes(&self) -> bool {
        matches!(
            self,
            Act::NewNote { .. }
                | Act::SetTitle(_)
                | Act::Append(_)
                | Act::SetContent(_)
                | Act::Notebook(_)
                | Act::Tags(_)
                | Act::Save
                | Act::Trash
                | Act::Restore
                | Act::Copy
                | Act::Undo
                | Act::Redo
                | Act::Import { .. }
        )
    }
}
