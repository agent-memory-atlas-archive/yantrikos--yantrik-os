//! Saving the open note through its document: one save in flight, the edits made meanwhile in
//! one follow-up, a failed save not retried by the autosave until something changes, and a file
//! changed outside never written over.

use yantrik_app::app::WindowId;
use yantrik_app::documents::{BeginSave, DocumentId, Durability, Fingerprint, Observed, SaveError, SaveOutcome, SaveTicket};
use yantrik_app::effects::Effects;

use super::{after_store, begin, failed, refreshed, relist, AUTOSAVE, WORKER_STOPPED};
use crate::command::Command;
use crate::model::{Model, Work};
use crate::note::{Note, NoteText};
use crate::store::{SaveFailed, SaveJob, Saved, CHANGED_ON_DISK};
use crate::view::render::{MODEL_ONLY, STATUS};
use crate::Notes;

/// Who asked for a save.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    /// The debounce: a save that failed is not tried again until the note changes.
    Autosave,
    /// The person, a close, or an action waiting on it: a save that failed is tried again.
    Asked,
}

/// Saves the open note, if it has edits not on disk.
pub fn save(m: &mut Model, fx: &mut Effects<Notes>, how: How) {
    // The store does one thing at a time; a save already in flight takes the edits as its
    // follow-up below.
    if m.busy.is_some_and(|w| w != Work::Save) {
        return;
    }
    fx.stop_timer(AUTOSAVE);
    if !m.ready || !m.current().is_some_and(|c| c.unsaved() && !c.trash()) {
        return;
    }
    let Some(doc) = m.current_id() else { return };
    let Some((d, _)) = m.current_mut() else { return };
    // Notes says unsaved by comparing with the file; the document counts edits. Every change is
    // an edit of the document and a landed save sets what the file holds to the ticket's content,
    // so unsaved (checked above) implies dirty. If they ever parted, no save would start and the
    // caller would wait out its deadline. A test fails on it; a running Notes says so in its log
    // and goes on, because a panic here would take the unsaved text with it.
    if !d.dirty() {
        if cfg!(test) {
            panic!("Notes' unsaved note must be a dirty document");
        }
        tracing::error!("an unsaved note is not a dirty document; no save is started for it");
    }
    let begun = match how {
        How::Autosave => d.begin_save(),
        How::Asked => d.retry_save(),
    };
    match begun {
        BeginSave::Ticket(ticket) => spawn(m, fx, doc, ticket),
        // In flight: one follow-up is owed. Clean or failed: nothing to do until a change.
        BeginSave::InFlight | BeginSave::Clean | BeginSave::Failed => {}
        // The file changed or went outside: the store's own refusal, said as it always was.
        BeginSave::Blocked => failed(m, fx, CHANGED_ON_DISK.into()),
    }
}

fn spawn(m: &mut Model, fx: &mut Effects<Notes>, doc: DocumentId, ticket: SaveTicket<NoteText>) {
    let Some(file) = m.files.get(&doc) else { return };
    let seq = ticket.seq;
    let job = SaveJob {
        id: file.id.clone(),
        on_disk: file.on_disk.clone(),
        meta_unsure: file.meta_unsure.clone(),
        modified: file.modified,
        ticket,
    };
    // A save that ended without an answer may have written anything: not called saved.
    let written = Fingerprint::of_bytes(job.ticket.content.text.as_bytes());
    let lost = Command::SaveLanded {
        doc,
        seq,
        result: Err(SaveFailed {
            error: SaveError::after_writing(Durability::Unknown, written, WORKER_STOPPED),
            meta_unsure: None,
        }),
    };
    begin(m, fx, Work::Save, lost, move |vault| Command::SaveLanded { doc, seq, result: vault.save(&job) });
}

/// A save of `doc` at `seq` landed.
pub fn landed(m: &mut Model, fx: &mut Effects<Notes>, doc: DocumentId, seq: u64, result: Result<Saved, SaveFailed>) {
    if m.busy == Some(Work::Save) {
        m.busy = None;
    }
    let Some(d) = m.documents.get_mut(doc) else { return };
    match result {
        Ok(saved) => {
            // Who else wrote the file while ours was in flight is the document's to know; telling
            // the person is the conflict UI's (step 8d). Today's behaviour is kept: nothing said.
            let SaveOutcome::Saved { follow_up, replaced: _ } = d.save_landed(seq, Ok(saved.written.clone())) else {
                return;
            };
            // From the new baseline: a look the watch took before the save is dropped, and our
            // own write is never reported as an outside edit.
            if let Some((key, path, seen)) = d.watch_entry() {
                fx.watch(&key, path, seen);
            }
            let Some(file) = m.files.get_mut(&doc) else { return };
            file.on_disk = Some(saved.content.clone());
            file.meta_unsure = None;
            file.modified = saved.modified;
            let entry = Note { id: file.id.clone(), content: saved.content, modified: saved.modified, trash: false, stored: true };
            match m.library.iter_mut().find(|n| n.id == entry.id && !n.trash) {
                Some(n) => *n = entry,
                None => m.library.push(entry),
            }
            m.notice.clear();
            refreshed(m, fx);
            relist(m, fx);
            if let Some(ticket) = follow_up {
                spawn(m, fx, doc, ticket);
            }
            after_store(m, fx);
        }
        Err(failure) => {
            let sentence = failure.error.sentence.clone();
            if matches!(d.save_landed(seq, Err(failure.error)), SaveOutcome::Stale) {
                return;
            }
            if let (Some(file), Some(meta)) = (m.files.get_mut(&doc), failure.meta_unsure) {
                file.meta_unsure = Some(meta);
            }
            failed(m, fx, sentence);
        }
    }
}

/// The person asked the window to close: now, or once the open note is saved.
pub fn close(m: &mut Model, fx: &mut Effects<Notes>, window: WindowId) {
    if m.busy.is_some() {
        m.notice = "Please wait for the current operation before closing.".into();
        fx.changed(STATUS);
    } else if m.unsaved() {
        m.quitting = Some(window);
        save(m, fx, How::Asked);
    } else {
        fx.close(window);
    }
}

/// The watch saw an open note's file change. The document classifies it (an outside edit, a
/// conflict, a deletion, or our own write); nothing is said until a save meets it, as before.
pub fn seen(m: &mut Model, fx: &mut Effects<Notes>, key: &str, seen: Observed) {
    let Some(d) = m.documents.by_key(key).and_then(|id| m.documents.get_mut(id)) else { return };
    d.observe(seen);
    if let Some((key, path, seen)) = d.rewatch() {
        fx.watch(&key, path, seen);
    }
    fx.changed(MODEL_ONLY);
}
