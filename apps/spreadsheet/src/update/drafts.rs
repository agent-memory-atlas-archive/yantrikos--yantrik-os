//! The draft: a sheet with anything unsaved has its text in the framework's draft lane, so a
//! crash, a `kill -9` or a logout loses at most the last moment of typing, and the next start opens
//! it again. A sheet saved, or typed back to its file's content, has no draft.
//!
//! Writing a draft means encoding the whole sheet, which for a large sheet is not something to do
//! at every keystroke. So an edit only arms a short timer (arming it again restarts it), and the
//! draft is made when typing pauses; the lane writes it after its own debounce. A close or a
//! logout makes it at once, in the command that answers.

use std::time::Duration;

use yantrik_app::documents::DraftState;
use yantrik_app::effects::Effects;
use yantrik_app::timers::TimerId;

use super::say;
use crate::command::Command;
use crate::files::drafts::{now_ms, SheetDraft, VERSION};
use crate::model::Model;
use crate::YSheets;

/// How long the sheet stands still before its draft is made.
pub const PAUSE: Duration = Duration::from_millis(250);

const TIMER: TimerId = TimerId("ysheets.draft");

/// What the draft holds now.
fn draft_of(m: &Model) -> SheetDraft {
    SheetDraft {
        version: VERSION,
        path: m.path().map(Into::into),
        rows: m.sheet().used_rows(),
        on_disk: m.doc.on_disk.clone(),
        active: m.active.name(),
        at: now_ms(),
    }
}

/// Whether the sheet's draft is behind its edits.
fn owed(m: &Model) -> bool {
    let (seq, name) = (m.d().seq, &m.doc.draft);
    match &m.d().draft {
        DraftState::Kept { seq: kept } => *kept < seq,
        DraftState::Failed { .. } => m.draft_failed_at.get(name).is_none_or(|at| *at < seq),
        DraftState::None => true,
    }
}

/// After every command: a draft asked for when the sheet is unsaved and its draft is older than its
/// edits, and removed when nothing is unsaved. A draft that could not be written is tried again at
/// the sheet's next edit, not at every command.
pub fn sync(m: &mut Model, fx: &mut Effects<YSheets>) {
    if m.given_up {
        return;
    }
    if m.modified() {
        if owed(m) {
            fx.after(TIMER, PAUSE, Command::DraftDue);
        }
    } else if m.d().draft != DraftState::None {
        let name = m.doc.draft.clone();
        fx.stop_timer(TIMER);
        fx.discard_draft(&name);
        m.draft_discarded();
    }
}

/// The sheet has stood still: its draft is made now, if it is still owed.
pub fn due(m: &mut Model, fx: &mut Effects<YSheets>) {
    if m.given_up || !m.modified() || !owed(m) {
        return;
    }
    let name = m.doc.draft.clone();
    fx.save_draft(&name, draft_of(m).bytes());
    m.draft_failed_at.remove(&name);
    m.draft_asked();
}

/// The draft written now, not after its pause: ySheets is about to end. Whether there was anything
/// unsaved to keep.
pub fn checkpoint_now(m: &mut Model, fx: &mut Effects<YSheets>) -> bool {
    if m.given_up || !m.modified() {
        return false;
    }
    let name = m.doc.draft.clone();
    fx.checkpoint_draft(&name, draft_of(m).bytes());
    m.draft_asked();
    true
}

/// A draft reached the disk. The lane says so; nothing waits on it.
pub fn saved(_: &mut Model, _: &str) {}

/// A draft could not be written: recovery after a crash is not protected, and the person is told.
pub fn not_saved(m: &mut Model, name: &str, why: &str) {
    say(m, format!("Draft recovery failed: {why}"));
    if m.doc.draft == name {
        let seq = m.d().seq;
        m.draft_failed_at.insert(name.to_string(), seq);
        m.draft_failed(why);
    }
}

/// The sheet is gone: its draft with it.
pub fn forget(m: &mut Model, fx: &mut Effects<YSheets>, name: &str) {
    fx.stop_timer(TIMER);
    fx.discard_draft(name);
    m.draft_failed_at.remove(name);
}

/// The first command: the sheet's file watched, when it came back from a draft with one.
pub fn started(m: &mut Model, fx: &mut Effects<YSheets>) {
    if let Some((key, path, seen)) = m.d().watch_entry() {
        fx.watch(&key, path, seen);
    }
}
