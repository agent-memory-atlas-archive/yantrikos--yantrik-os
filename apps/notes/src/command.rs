//! Every way the model changes: what the window, the surface, the timers and the store's jobs ask.

use yantrik_app::app::WindowId;
use yantrik_app::documents::{DocumentId, Observed};

use crate::note::{Note, NoteText};
use crate::store::{Loaded, Reread, Resolved, SaveFailed, Saved};

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
    /// An outside change resolved (`resolution`) for document `doc`, whose edits were at `seq`.
    /// Boxed: it carries two notes, and every command is moved through the app's queue.
    Resolved { doc: DocumentId, seq: u64, resolution: Resolution, result: Result<Box<Resolved>, String> },
    /// The file's version read for a comparison: `None` when the file is gone.
    Compared { doc: DocumentId, result: Result<Option<NoteText>, String> },
    /// A note with nothing unsaved, read again after another program changed it. `Err`: our own
    /// last write could not be kept first, so nothing was read. Boxed, as `Resolved` is.
    Reread { doc: DocumentId, result: Result<Box<Reread>, String> },
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
    /// The outside bar's choices: *Keep mine as a copy* (*Save as new* for a deleted file),
    /// *Take theirs*, *Compare*, *Not now*.
    KeepMine,
    TakeTheirs,
    Compare,
    NotNow,
}

/// How an outside change to the open note is resolved. Either way the window's text is kept as a
/// note of its own first, and the other program's file is never written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// The window moves to the kept note; the original is left as the file holds it.
    KeepMine,
    /// The window's note takes the file's text; the kept note is the recovery.
    TakeTheirs,
}

impl Resolution {
    /// What the kept note's file name starts with: a copy, as *Save a copy* names one, or a
    /// recovered note, as the journal's replay names one.
    pub fn kept_prefix(self) -> &'static str {
        match self {
            Resolution::KeepMine => "copy",
            Resolution::TakeTheirs => "recovered",
        }
    }
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
            "keep-mine" => WindowAction::KeepMine,
            "take-theirs" => WindowAction::TakeTheirs,
            "compare" => WindowAction::Compare,
            "not-now" => WindowAction::NotNow,
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
    KeepMineAsCopy,
    TakeTheirs,
    Compare,
    /// A required argument given empty: refused, in the window's notice too.
    Refuse(String),
}
