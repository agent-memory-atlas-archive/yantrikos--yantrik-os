//! The end of a run: the window closed, a logout or a quit, and the editor's typing kept in the
//! draft lane so neither loses it.
//!
//! The old app had no question to ask on close and asks none now. What it lost was the typing in
//! the editor that no button had committed yet, and any write still running when the window went.
//! Now the typing is in the framework's draft lane, written within its debounce of the last key
//! and at once when the app ends, and offered back at the next start (`lib.rs`); and the end
//! waits for the work in flight (`update`'s tail ends the app once `work` is 0).

use std::time::Duration;

use yantrik_app::app::WindowId;

use super::Fx;
use crate::model::{Model, DRAFT, NOTHING};

/// How long a closed window's work may take before the framework ends the app anyway
/// (`fx.linger`'s backstop, which logs that it did). Each write is a local file, so this is
/// reached only by a disk that stopped answering, or a clipboard that never returned.
pub const CLOSE_BACKSTOP: Duration = Duration::from_secs(10);

/// The last window closed with work in flight or typing unsaved: the typing goes to the draft
/// lane now, the window closes at once, as the person asked, and the process runs on without it
/// until the work lands. A second launch meanwhile opens it again (`Event::Presented`).
pub(super) fn closing(m: &mut Model, window: WindowId, fx: &mut Fx) {
    keep_draft_now(m, fx);
    fx.close(window);
    if m.work > 0 {
        m.closing = true;
        fx.linger(CLOSE_BACKSTOP);
    }
}

/// A logout (a signal, the display going) or a person's quit. Nothing asks first: a logout has
/// nobody there to answer. The typing goes to the draft lane in this command, and the app ends
/// once the work in flight has landed; after a signal the framework still ends it at its own
/// deadline, whatever is in flight.
pub(super) fn ending(m: &mut Model, fx: &mut Fx) {
    keep_draft_now(m, fx);
    m.ending = true;
}

/// A second launch opened the window again while a close waited: the close is over. The work in
/// flight lands as any other, and new work is taken again. A logout's end is not a close and is
/// not cancelled.
pub(super) fn reopened(m: &mut Model, fx: &mut Fx) {
    fx.changed(NOTHING);
    m.closing = false;
}

/// The editor's typing, in the draft lane while it is unsaved; the draft removed once the typing
/// is kept and every write has landed.
pub(super) fn keep_draft(m: &mut Model, fx: &mut Fx) {
    keep(m, fx, m.closing || m.ending, false);
}

/// The typing written now, whatever was handed over before: the end is a meaningful moment, and
/// a debounced save still waiting is replaced by this one.
fn keep_draft_now(m: &mut Model, fx: &mut Fx) {
    keep(m, fx, true, true);
}

fn keep(m: &mut Model, fx: &mut Fx, now: bool, force: bool) {
    if m.orphan.is_some() {
        // The last run's typing is on disk as the draft until it is kept as a snippet.
        return;
    }
    if m.editor_dirty() {
        if m.drafted.as_ref() == Some(&m.editor) && !force {
            return;
        }
        let Ok(bytes) = serde_json::to_vec(&m.editor) else { return };
        if now {
            fx.checkpoint_draft(DRAFT, bytes);
        } else {
            fx.save_draft(DRAFT, bytes);
        }
        m.drafted = Some(m.editor.clone());
    } else if m.drafted.is_some() && m.lane.idle() {
        // Kept by the store, and the store's file has it: a write still in flight could yet fail
        // and give the typing back to the editor, so the draft waits for it.
        fx.discard_draft(DRAFT);
        m.drafted = None;
    }
}
