//! A note with nothing unsaved follows its file (decided in review of #915): another program's
//! change is read again and shown, with a quiet line saying so, the way *Take theirs* shows a
//! version (the undo history starts again).
//!
//! Two guards keep this from costing the person anything:
//! - A note with unsaved edits is never replaced this way. A key typed before the read lands
//!   makes it a conflict, judged by the document, and the person chooses (outside.rs).
//! - When what the window holds is our own write from this run, it is kept as a recovered note
//!   before it is replaced. Another program writing from a stale copy would otherwise leave no
//!   copy of the person's last version anywhere, since the undo history is cleared too.

use yantrik_app::documents::{DocumentId, Fingerprint, Observed, OutsideState};
use yantrik_app::effects::Effects;

use super::outside::{say, take_theirs};
use super::{after_store, begin, relist, said, save_state};
use crate::command::{Command, Resolution};
use crate::model::{Model, Work};
use crate::store::Reread;
use crate::view::render::{OUTSIDE, STATUS};
use crate::Notes;

/// The open note's file changed outside and the note has nothing unsaved: it is read again, and
/// [`reread`] shows it. Edits made since the change was seen make it the conflict it now is. While
/// the store is busy, the read waits for it (`reread_owed`).
pub fn follow(m: &mut Model, fx: &mut Effects<Notes>) {
    let Some(c) = m.current() else { return };
    let OutsideState::Changed { now } = c.outside().clone() else { return };
    let (doc, id, unsaved) = (c.doc.id, c.id().to_string(), c.unsaved());
    let ours = c.file.ours.then(|| {
        let kept_id = format!("{}-{}.md", Resolution::TakeTheirs.kept_prefix(), uuid7::uuid7());
        (kept_id, c.content().clone())
    });
    if unsaved {
        if let Some(d) = m.documents.get_mut(doc) {
            d.observe(Observed::File(now));
        }
        m.status = save_state(m).into();
        fx.changed(STATUS);
        fx.changed(OUTSIDE);
        return;
    }
    if m.busy.is_some() {
        m.pane.reread_owed = true;
        return;
    }
    let lost = Command::Reread { doc, result: Err(said::JOB_LOST.into()) };
    begin(m, fx, Work::Reread, lost, move |vault| {
        let ours = ours.as_ref().map(|(kept_id, text)| (kept_id.as_str(), text));
        Command::Reread { doc, result: vault.reread(&id, ours).map(Box::new) }
    });
}

/// The changed note, read again (and our last write kept first, when it was ours). Shown in place
/// of what the window held when nothing there is unsaved; a key typed while the read ran makes it
/// a conflict instead, so nothing typed is replaced. A file found gone is a deletion. When our
/// write could not be kept, nothing was read and nothing is replaced.
pub fn reread(m: &mut Model, fx: &mut Effects<Notes>, doc: DocumentId, result: Result<Box<Reread>, String>) {
    m.busy = None;
    let still_changed =
        m.current().is_some_and(|c| c.doc.id == doc && matches!(c.outside(), OutsideState::Changed { .. }));
    match result.map(|reread| *reread) {
        Err(e) => say(m, fx, &said::ours_not_kept(&e)),
        Ok(Reread { kept, theirs }) => {
            let sentence = if kept.is_some() { said::UPDATED_KEEPING_OURS } else { said::UPDATED_FROM_DISK };
            if let Some(kept) = kept {
                m.library.push(kept);
                relist(m, fx);
            }
            match theirs {
                Err(e) => say(m, fx, &said::reread_failed(&e)),
                Ok(_) if !still_changed => {}
                Ok(Some(theirs)) if !m.unsaved() => take_theirs(m, fx, doc, theirs, sentence),
                Ok(theirs) => {
                    let seen = match &theirs {
                        Some(t) => Observed::File(Fingerprint::of_bytes(t.text.as_bytes())),
                        None => Observed::Gone,
                    };
                    if let Some(d) = m.documents.get_mut(doc) {
                        d.observe(seen);
                    }
                    fx.changed(OUTSIDE);
                }
            }
        }
    }
    m.status = save_state(m).into();
    fx.changed(STATUS);
    after_store(m, fx);
}
