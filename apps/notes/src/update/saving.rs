//! Saving the open note through its document: one save in flight, the edits made meanwhile in
//! one follow-up, a failed save not retried by the autosave until something changes and said with
//! how much of it reached the disk, and a file changed outside never written over: the person
//! chooses what happens to the two versions first (outside.rs).

use yantrik_app::app::WindowId;
use yantrik_app::documents::{BeginSave, DocumentId, Durability, Fingerprint, SaveError, SaveOutcome, SaveTicket};
use yantrik_app::effects::Effects;

use super::{after_store, begin, failed, refreshed, relist, said, save_state, AUTOSAVE};
use crate::command::Command;
use crate::model::{Model, Work};
use crate::note::{Note, NoteText};
use crate::store::{SaveFailed, SaveJob, Saved};
use crate::view::render::{OUTSIDE, STATUS};
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
        BeginSave::Blocked => blocked(m, fx, how),
    }
}

/// The file changed or went outside, and the person has not chosen yet. The autosave says so in
/// the footer only: the bar already asks, or the person put it away with *Not now*. A save the
/// person (or a close, or an action) asked for is refused in words and brings the bar back.
fn blocked(m: &mut Model, fx: &mut Effects<Notes>, how: How) {
    let Some(sentence) = m.current().map(|c| said::blocked(c.outside())) else { return };
    match how {
        How::Autosave => {
            m.status = save_state(m).into();
            fx.changed(STATUS);
        }
        How::Asked => {
            m.pane.outside_later = false;
            fx.changed(OUTSIDE);
            failed(m, fx, sentence.into());
        }
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
            error: SaveError::after_writing(Durability::Unknown, written, said::JOB_LOST),
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
            let SaveOutcome::Saved { follow_up, replaced } = d.save_landed(seq, Ok(saved.written.clone())) else {
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
            // Another program's write that this save replaced is said, and stays said while the
            // person types on; a failure's notice is answered by this landing.
            if let Some(what) = replaced {
                m.notice = said::replaced(what).into();
                m.lasting_notice = true;
            } else if !m.lasting_notice {
                m.notice.clear();
            }
            refreshed(m, fx);
            relist(m, fx);
            if let Some(ticket) = follow_up {
                spawn(m, fx, doc, ticket);
            }
            after_store(m, fx);
        }
        Err(failure) => {
            let conflict = failure.error.conflict;
            let mut sentence = said::save_failed(&failure.error);
            if matches!(d.save_landed(seq, Err(failure.error)), SaveOutcome::Stale) {
                return;
            }
            if conflict {
                // The store's check found the file changed: the person chooses, as for the watch.
                sentence = said::blocked(&d.outside).into();
                m.pane.outside_later = false;
            }
            if let (Some(file), Some(meta)) = (m.files.get_mut(&doc), failure.meta_unsure) {
                file.meta_unsure = Some(meta);
            }
            // A sighting the save was holding is judged now, and may be the bar's news.
            fx.changed(OUTSIDE);
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
