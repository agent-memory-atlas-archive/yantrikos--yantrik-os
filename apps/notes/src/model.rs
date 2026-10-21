//! What Notes is: the model every command changes and every render reads.
//!
//! The library is every note as the vault holds it. The open note is a document in the
//! framework's `Documents` table, with a [`NoteFile`] beside it saying what its files hold; the
//! window shows it through a [`Pane`], which owns everything that belongs to the view and not to
//! the note (its undo history, its search, its dialog). Notes has one window, so one pane.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::Value;
use yantrik_app::app::WindowId;
use yantrik_app::documents::{CloseFlow, Document, DocumentId, Documents, Fingerprint, OutsideState, ViewId};
use yantrik_app::intents::InvocationId;

use crate::claim::Claim;
use crate::command::Step;
use crate::note::{Note, NoteText};
use crate::recovery::Recovery;
use crate::store::Vault;
use crate::undo::History;
use crate::view::listing::Listing;
use crate::view::preview::Block;

pub struct Model {
    pub vault: Vault,
    pub library: Vec<Note>,
    pub documents: Documents<NoteText>,
    /// What each open document's files hold, by document.
    pub files: BTreeMap<DocumentId, NoteFile>,
    pub pane: Pane,
    /// The library has been read; until then nothing but `reload` touches it.
    pub ready: bool,
    /// The one store job in flight. The store does one thing at a time, as it always has.
    pub busy: Option<Work>,
    /// What could not be done, in one line; also `describe.notice`.
    pub notice: String,
    /// The notice says something a person may not see while typing on (a save replaced another
    /// program's write): the next save that lands leaves it, and the next action takes it back.
    pub lasting_notice: bool,
    /// The footer's save state, as the last thing that set it said it.
    pub status: String,
    /// An action waiting for the open note to be saved first.
    pub pending: Option<Pending>,
    /// A close or a quit asking about unsaved notes, or waiting on their saves.
    pub closing: Option<CloseFlow>,
    /// Text kept from a run that ended before it was saved, offered back note by note.
    pub recovery: Recovery,
    /// This run's last landed write to each note, by file name: the fingerprint of its text. A
    /// note whose file still holds it is ours to keep if another program replaces it
    /// (update/follow.rs), however often it was closed, reopened or reloaded since.
    pub written: BTreeMap<String, Fingerprint>,
    /// The document, and its edit, whose draft could not be written: tried again at its next edit.
    pub draft_failed_at: Option<(DocumentId, u64)>,
    /// A call from the surface waiting for the work it started.
    pub asked: Option<Asked>,
    /// What the last call from the surface is answered with.
    pub answer: Value,
    /// This copy's hold on the library, let go with the model.
    _claim: Claim,
}

/// What an open note's files hold, beside its document.
#[derive(Debug, Clone)]
pub struct NoteFile {
    /// The note's file name in the vault.
    pub id: String,
    pub trash: bool,
    /// The note as its files hold it; `None` for a note not saved yet. Whether the note is unsaved
    /// is a comparison with this, as it always was.
    pub on_disk: Option<NoteText>,
    /// A sidecar of ours a failed save may have left on disk.
    pub meta_unsure: Option<String>,
    pub modified: u64,
}

/// One store job, by what it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Work {
    Load,
    Save,
    Trash,
    Restore,
    Import,
    Export,
    /// An outside change resolved: the window's text kept as a note, the other version read.
    Resolve,
    /// The other version read, for a comparison.
    Compare,
    /// The open note read again: another program changed it, and nothing here was unsaved.
    Reread,
}

/// An action held until the open note is saved.
#[derive(Clone)]
pub struct Pending {
    pub step: Step,
    /// From the surface, where a save that fails refuses the call ("nothing else was done").
    pub refuse_if_unsaved: bool,
}

/// A call from the surface whose answer waits for its work.
#[derive(Debug, Clone)]
pub struct Asked {
    pub invocation: Option<InvocationId>,
    pub expect: Expect,
    /// The save it waited on failed: the refusal.
    pub refused: Option<String>,
    /// The import, export or comparison it started landed.
    pub landed: bool,
    /// The note the window's text was kept as, when the call resolved an outside change.
    pub kept: Option<String>,
}

/// What the answer to a call from the surface is, once its work is done.
#[derive(Debug, Clone)]
pub enum Expect {
    /// The open note, read back off the disk.
    Written,
    /// The open note, refused unless the disk holds exactly it.
    Saved,
    Trashed { id: String, title: String, path: PathBuf },
    Restored { id: String },
    /// Unsaved text from before Notes closed, put back into its note.
    Recovered { id: String },
    /// Unsaved text from before Notes closed, dropped.
    Discarded { id: String },
    Copied { from: String },
    Reloaded,
    Imported { from: PathBuf },
    Exported { to: PathBuf, text: String },
    /// The file's version, taken into the window in place of the text that was there.
    TookTheirs { id: String },
    /// The window's text beside the file's.
    Compared,
}

/// The window's view of the library and the open note: everything that is the view's, not the
/// note's.
pub struct Pane {
    pub window: WindowId,
    /// The view on the open note, when one is open.
    pub view: Option<ViewId>,
    pub history: History,
    pub folder: String,
    pub query: String,
    pub preview: bool,
    pub focus_mode: bool,
    /// 0 none, 1 import, 2 export, 3 compare, 4 the close prompt.
    pub dialog: i32,
    /// The file's version beside the window's text in the comparison; `None` when its file is gone.
    pub theirs: Option<NoteText>,
    /// *Not now*: the outside bar is put away until a save is refused or the file changes again.
    /// Saving stays refused all the while.
    pub outside_later: bool,
    /// The open note's file changed while the store was busy: it is read again once it is not.
    pub reread_owed: bool,
    pub path: String,
    /// The notebook and tags fields as typed (the sidecar keeps them cut to length).
    pub notebook: String,
    pub tags: String,
    /// The library list as last drawn: it is redrawn where it always was, not on every key.
    pub listing: Listing,
    pub blocks: Vec<Block>,
    /// What the window is to do once drawn: focus, caret, scroll. Cleared before every command.
    pub cues: Vec<Cue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cue {
    /// A note was shown: scroll to its top and focus what can take keys.
    Shown,
    CaretAt(i32),
    FocusEditor,
    FocusShortcuts,
    FocusContent,
}

/// The note the pane shows: its document and what its files hold.
pub struct Current<'a> {
    pub doc: &'a Document<NoteText>,
    pub file: &'a NoteFile,
}

impl Current<'_> {
    pub fn content(&self) -> &NoteText {
        &self.doc.content
    }

    pub fn text(&self) -> &str {
        &self.doc.content.text
    }

    pub fn title(&self) -> String {
        self.doc.content.title()
    }

    pub fn id(&self) -> &str {
        &self.file.id
    }

    pub fn trash(&self) -> bool {
        self.file.trash
    }

    /// What is known of changes made to the note's file outside Notes.
    pub fn outside(&self) -> &OutsideState {
        &self.doc.outside
    }

    /// Edits not on disk: the content is not what the files hold.
    pub fn unsaved(&self) -> bool {
        let disk = self.file.on_disk.as_ref();
        disk.map(|d| d.text.as_str()) != Some(self.text())
            || disk.map(|d| d.meta.as_str()).unwrap_or("") != self.content().meta
    }

    /// The note as the library lists it, with this view's unsaved content.
    pub fn as_note(&self) -> Note {
        Note {
            id: self.file.id.clone(),
            content: self.doc.content.clone(),
            modified: self.file.modified,
            trash: self.file.trash,
            stored: self.file.on_disk.is_some(),
        }
    }
}

impl Model {
    pub fn new(vault: Vault, claim: Claim, recovery: Recovery) -> Model {
        Model {
            vault,
            library: Vec::new(),
            documents: Documents::new(),
            files: BTreeMap::new(),
            pane: Pane {
                window: WindowId::PRIMARY,
                view: None,
                history: History::default(),
                folder: "all".into(),
                query: String::new(),
                preview: false,
                focus_mode: false,
                dialog: 0,
                theirs: None,
                outside_later: false,
                reread_owed: false,
                path: String::new(),
                notebook: String::new(),
                tags: String::new(),
                listing: Listing::default(),
                blocks: Vec::new(),
                cues: Vec::new(),
            },
            ready: false,
            busy: None,
            notice: String::new(),
            lasting_notice: false,
            // The window's own first words, until the library is asked for.
            status: "Opening your library…".into(),
            pending: None,
            closing: None,
            recovery,
            written: BTreeMap::new(),
            draft_failed_at: None,
            asked: None,
            answer: Value::Null,
            _claim: claim,
        }
    }

    /// The document the pane shows.
    pub fn current_id(&self) -> Option<DocumentId> {
        self.documents.view(self.pane.view?).map(|v| v.document)
    }

    pub fn current(&self) -> Option<Current<'_>> {
        let id = self.current_id()?;
        Some(Current { doc: self.documents.get(id)?, file: self.files.get(&id)? })
    }

    pub fn current_mut(&mut self) -> Option<(&mut Document<NoteText>, &mut NoteFile)> {
        let id = self.current_id()?;
        Some((self.documents.get_mut(id)?, self.files.get_mut(&id)?))
    }

    pub fn unsaved(&self) -> bool {
        self.current().is_some_and(|c| c.unsaved())
    }

    /// Whether what a note's files hold is this run's last landed write to it.
    pub fn ours(&self, file: &NoteFile) -> bool {
        let held = file.on_disk.as_ref().map(|d| Fingerprint::of_bytes(d.text.as_bytes()));
        held.is_some() && self.written.get(&file.id) == held.as_ref()
    }

    /// The live notes, other than `except`, that held exactly `content` when last read: the ones
    /// whose files a job reads to learn whether one still holds it, as file name and length.
    pub fn alike(&self, content: &NoteText, except: &str) -> Vec<(String, usize)> {
        self.library
            .iter()
            .filter(|n| !n.trash && n.id != except && n.content == *content)
            .map(|n| (n.id.clone(), n.content.text.len()))
            .collect()
    }

    /// The library's text, leaving out the open note's own entry.
    pub fn text_beside_current(&self) -> usize {
        let key = self.current().map(|c| (c.id().to_string(), c.trash()));
        self.library
            .iter()
            .filter(|n| key.as_ref().is_none_or(|(id, trash)| *id != n.id || *trash != n.trash))
            .map(|n| n.content.text.len())
            .sum()
    }

    pub fn library_text(&self) -> usize {
        self.library.iter().map(|n| n.content.text.len()).sum()
    }
}
