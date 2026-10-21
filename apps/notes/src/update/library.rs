//! The store's other jobs: reading the library, the Trash, an import and an export, and what the
//! window does when each lands.
//!
//! The editor takes keys while a job runs, so the open note may have text typed meanwhile when a
//! reload, an import or a restore lands. That text is never closed away: the note stays open and
//! unsaved, its autosave owed, and the window says what the job could not do because of it.

use std::path::PathBuf;

use yantrik_app::effects::Effects;

use super::navigate::show_note;
use super::{after_store, begin, failed, refreshed, relist, show, WORKER_STOPPED};
use crate::command::Command;
use crate::model::{Model, Work};
use crate::note::Note;
use crate::store::{Loaded, MAX_NOTES, VAULT_LIMIT};
use crate::view::render::{CHROME, STATUS};
use crate::Notes;

pub fn load(m: &mut Model, fx: &mut Effects<Notes>) {
    begin(m, fx, Work::Load, Command::Loaded(Err(WORKER_STOPPED.into())), |vault| {
        Command::Loaded(vault.load())
    });
}

/// Said when a reload found the open note's file changed while text typed meanwhile was not saved.
const KEPT_OVER_CHANGE: &str =
    "The open note changed on disk while you typed in it. What you typed is kept, unsaved; the note was not read again.";

/// The library as read. The open note stays open: read afresh if its file is still there, or, if
/// it has text typed while the library was read, kept as it is.
pub fn loaded(m: &mut Model, fx: &mut Effects<Notes>, result: Result<Loaded, String>) {
    m.busy = None;
    match result {
        Ok(loaded) => {
            m.vault = m.vault.with_access(loaded.read_only.clone());
            let key = m.current().map(|c| (c.id().to_string(), c.trash()));
            m.library = loaded.notes;
            let again = key.and_then(|(id, trash)| m.library.iter().find(|n| n.id == id && n.trash == trash).cloned());
            let kept = m.unsaved();
            // The file is no longer what the note was read from, so reading it again would have
            // shown something else. What that change means is the outside-change flow's to say;
            // here it is only never read over the text.
            let changed = kept && m.current().is_some_and(|c| again.as_ref().map(|n| &n.content) != c.file.on_disk.as_ref());
            if !kept {
                // Closed first, so the note read afresh is a fresh document and not the old one.
                show_note(m, fx, None);
                show_note(m, fx, again);
            }
            m.ready = true;
            m.notice = sentences([loaded.read_only.as_deref(), Some(loaded.notice.as_str()), changed.then_some(KEPT_OVER_CHANGE)]);
            if kept {
                still_typing(m, fx);
            } else {
                show(m, fx);
            }
            after_store(m, fx);
        }
        Err(e) => failed(m, fx, e),
    }
}

pub fn trash(m: &mut Model, fx: &mut Effects<Notes>) {
    let Some((id, on_disk, modified)) = m
        .current()
        .filter(|c| !c.trash())
        .map(|c| (c.id().to_string(), c.file.on_disk.clone().unwrap_or_else(|| c.content().clone()), c.file.modified))
    else {
        return;
    };
    let lost = Command::Trashed { id: id.clone(), result: Err(WORKER_STOPPED.into()) };
    begin(m, fx, Work::Trash, lost, move |vault| {
        let result = vault.trash(&id, &on_disk, modified);
        Command::Trashed { id, result }
    });
}

pub fn trashed(m: &mut Model, fx: &mut Effects<Notes>, id: &str, result: Result<(), String>) {
    m.busy = None;
    match result {
        Ok(()) => {
            if let Some(n) = m.library.iter_mut().find(|n| n.id == id && !n.trash) {
                n.trash = true;
            }
            show_note(m, fx, None);
            show(m, fx);
            m.notice = "Moved to Trash. You can restore it from the library.".into();
            after_store(m, fx);
        }
        Err(e) => failed(m, fx, e),
    }
}

pub fn restore(m: &mut Model, fx: &mut Effects<Notes>) {
    let Some(note) = m.current().filter(|c| c.trash()).map(|c| c.as_note()) else { return };
    begin(m, fx, Work::Restore, Command::Restored(Err(WORKER_STOPPED.into())), move |vault| {
        Command::Restored(vault.restore(&note))
    });
}

pub fn import(m: &mut Model, fx: &mut Effects<Notes>, path: PathBuf) {
    if m.library.len() >= MAX_NOTES {
        m.notice = "The library supports up to 2,000 notes.".into();
        fx.changed(STATUS);
        return;
    }
    let room = VAULT_LIMIT.saturating_sub(m.library_text());
    begin(m, fx, Work::Import, Command::Imported(Err(WORKER_STOPPED.into())), move |vault| {
        Command::Imported(vault.import(&path, room))
    });
}

pub fn export(m: &mut Model, fx: &mut Effects<Notes>, path: PathBuf) {
    let text = m.current().map(|c| c.text().to_string()).unwrap_or_default();
    begin(m, fx, Work::Export, Command::Exported(Err(WORKER_STOPPED.into())), move |vault| {
        Command::Exported(vault.export(&path, &text))
    });
}

/// A note restored from the Trash, or imported: in the library, open, and the whole library
/// shown again. If the open note has text typed while the job ran, it stays open instead, and the
/// new note waits in the list.
pub fn opened(m: &mut Model, fx: &mut Effects<Notes>, result: Result<Note, String>) {
    m.busy = None;
    match result {
        Ok(note) => {
            m.library.retain(|a| a.id != note.id);
            m.library.push(note.clone());
            let kept = m.unsaved();
            let title = note.title();
            if !kept {
                show_note(m, fx, Some(note));
            }
            m.pane.folder = "all".into();
            m.pane.query.clear();
            m.pane.dialog = 0;
            m.notice.clear();
            fx.changed(CHROME);
            landed(m);
            if kept {
                // A call waiting on it is answered with this too, not with the note left open.
                let sentence = not_opened(&title);
                if let Some(asked) = m.asked.as_mut() {
                    asked.refused = Some(sentence.clone());
                }
                m.notice = sentence;
                still_typing(m, fx);
            } else {
                show(m, fx);
            }
            after_store(m, fx);
        }
        Err(e) => failed(m, fx, e),
    }
}

/// Said when an imported or restored note was not opened over text typed meanwhile.
fn not_opened(title: &str) -> String {
    format!("\"{title}\" is in the library but was not opened: the open note has unsaved text typed meanwhile, so it stays open.")
}

/// The open note kept, with text typed while a job ran: its history and place left alone (it is
/// not shown afresh), the list redrawn round it, and its autosave armed again, since a debounce
/// that ran out while the store was busy saved nothing.
fn still_typing(m: &mut Model, fx: &mut Effects<Notes>) {
    relist(m, fx);
    refreshed(m, fx);
}

/// The sentences that say something, as one line.
fn sentences<'a>(parts: impl IntoIterator<Item = Option<&'a str>>) -> String {
    parts.into_iter().flatten().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ")
}

pub fn exported(m: &mut Model, fx: &mut Effects<Notes>, result: Result<(), String>) {
    m.busy = None;
    match result {
        Ok(()) => {
            m.pane.dialog = 0;
            m.notice = "Exported to the requested file.".into();
            m.status = "All changes saved".into();
            fx.changed(CHROME);
            fx.changed(STATUS);
            landed(m);
            after_store(m, fx);
        }
        Err(e) => failed(m, fx, e),
    }
}

/// The import or export a call from the surface waits on has landed.
fn landed(m: &mut Model) {
    if let Some(asked) = m.asked.as_mut() {
        asked.landed = true;
    }
}
