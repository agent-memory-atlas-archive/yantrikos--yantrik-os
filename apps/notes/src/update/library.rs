//! The store's other jobs: reading the library, the Trash, an import and an export, and what the
//! window does when each lands.

use std::path::PathBuf;

use yantrik_app::effects::Effects;

use super::navigate::show_note;
use super::{after_store, begin, failed, show, WORKER_STOPPED};
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

/// The library as read. The open note stays open, read afresh, if its file is still there.
pub fn loaded(m: &mut Model, fx: &mut Effects<Notes>, result: Result<Loaded, String>) {
    m.busy = None;
    match result {
        Ok(loaded) => {
            let key = m.current().map(|c| (c.id().to_string(), c.trash()));
            m.library = loaded.notes;
            let again = key.and_then(|(id, trash)| m.library.iter().find(|n| n.id == id && n.trash == trash).cloned());
            // Closed first, so the note read afresh is a fresh document and not the old one.
            show_note(m, fx, None);
            show_note(m, fx, again);
            m.ready = true;
            m.notice = loaded.notice;
            show(m, fx);
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
/// shown again.
pub fn opened(m: &mut Model, fx: &mut Effects<Notes>, result: Result<Note, String>) {
    m.busy = None;
    match result {
        Ok(note) => {
            m.library.retain(|a| a.id != note.id);
            m.library.push(note.clone());
            show_note(m, fx, Some(note));
            m.pane.folder = "all".into();
            m.pane.query.clear();
            m.pane.dialog = 0;
            m.notice.clear();
            fx.changed(CHROME);
            landed(m);
            show(m, fx);
            after_store(m, fx);
        }
        Err(e) => failed(m, fx, e),
    }
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
