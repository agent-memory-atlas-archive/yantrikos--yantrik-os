//! An outside change resolved on disk: the text in the window kept as a note of its own, and what
//! the changed note's files hold now. Nothing here writes to the changed note's files: whichever
//! way the person chooses, the other program's version is left exactly as it wrote it.

use super::{read, validate, Vault, LIMIT, META_LIMIT};
use crate::note::{Note, NoteText};

/// What keeping the window's text beside a changed note found.
#[derive(Debug, Clone)]
pub struct Resolved {
    /// The window's text, saved as a new note.
    pub kept: Note,
    /// What the changed note's files hold now: `None` when its file is gone, `Err` when it could
    /// not be read (the kept note is on disk either way).
    pub theirs: Result<Option<NoteText>, String>,
}

impl Vault {
    /// Saves `mine` as a new note named `kept_id`, then reads note `id` as its files hold it now.
    /// The new note is written first, so the person's text is on disk before anything is decided
    /// about it, and it is created, never written over anything.
    pub fn keep_beside(&self, id: &str, kept_id: &str, mine: &NoteText) -> Result<Resolved, String> {
        let saved = self.save_new(kept_id, mine)?;
        let kept = Note { id: kept_id.into(), content: saved.content, modified: saved.modified, trash: false, stored: true };
        Ok(Resolved { kept, theirs: self.read_note(id) })
    }

    /// Note `id` as its files hold it now, held to the limits a loaded note is: `None` when its
    /// text file is gone.
    pub fn read_note(&self, id: &str) -> Result<Option<NoteText>, String> {
        let (text_path, meta_path) = self.files(id)?;
        let Some(text) = read(&text_path, LIMIT)? else { return Ok(None) };
        validate(&text)?;
        let meta = read(&meta_path, META_LIMIT)?.unwrap_or_default();
        Ok(Some(NoteText { text, meta }))
    }
}
