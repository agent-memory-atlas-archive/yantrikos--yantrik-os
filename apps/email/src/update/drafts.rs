//! The draft file: written and removed in jobs, one at a time and in the order they were asked
//! for. A save asked while another is writing waits, and only the latest waits.
//!
//! Beside the draft, a mark says when whether its message was already sent is not known (a send
//! that never came back), so every Compose that opens it says so until it is sent or discarded.
//! The lane copy a send left is dropped only once the draft file holding its text has landed, so
//! the text is never nowhere.

use yantrik_app::effects::Effects;

use super::{clear_notice, say};
use crate::command::{Command, Work};
use crate::files;
use crate::model::{part, FileOp, Keep, Model, SENDING};
use crate::state;
use crate::Email;

/// What the notice says when a message is offered again at start.
pub const UNSENT_AT_START: &str = "A message was being sent when Email closed, and whether it went is not known. It is kept as a draft: look in Sent before sending it again.";

/// What Compose says, every time it opens a draft whose message may already have gone.
pub const UNSURE: &str = "Whether this message was already sent is not known. Look in Sent before sending it again.";

/// The composer's status line for that draft.
pub const UNSURE_STATUS: &str = "Not known whether this was sent";

/// One write or removal of the draft file, in a job; or, while one is running, the next one. A
/// queued operation replaced by a newer one hands its duty to drop the lane copy on.
pub fn file_op(m: &mut Model, op: FileOp, fx: &mut Effects<Email>) {
    if m.composer.writing {
        let release = op.releases_lane() || m.composer.queued.as_ref().is_some_and(FileOp::releases_lane);
        m.composer.queued = Some(op.releasing_lane(release));
        return;
    }
    m.composer.writing = true;
    let path = m.files.draft.clone();
    fx.spawn_or(Command::Unfinished(Work::Draft), move |_| {
        let outcome = match &op {
            FileOp::Save { draft, unsure, .. } => {
                state::save_draft(&path, draft).and_then(|()| files::set_unsure(&path, *unsure))
            }
            FileOp::Clear { .. } => state::clear_draft(&path).and_then(|()| files::set_unsure(&path, false)),
        };
        Command::DraftDone { op, outcome }
    });
}

/// The next file operation, once the one before it has landed.
pub fn next_file_op(m: &mut Model, fx: &mut Effects<Email>) {
    m.composer.writing = false;
    if let Some(op) = m.composer.queued.take() {
        file_op(m, op, fx);
    }
}

/// The lane copy goes once the draft file holds its text, unless a newer send has put its own
/// copy there meanwhile.
fn release_lane(m: &Model, release: bool, fx: &mut Effects<Email>) {
    if release && m.composer.sending.is_none() {
        fx.discard_draft(SENDING);
    }
}

pub fn done(m: &mut Model, op: FileOp, outcome: Result<(), String>, fx: &mut Effects<Email>) {
    fx.changed(part::STATUS);
    let release = op.releases_lane();
    match (op, outcome) {
        (FileOp::Save { draft, keep, unsure, .. }, Ok(())) => {
            m.composer.kept = Ok(Some(draft));
            m.composer.kept_unsure = unsure;
            release_lane(m, release, fx);
            match keep {
                Keep::Quietly => {}
                Keep::Saved => {
                    m.composer.status = "Draft saved".into();
                    clear_notice(m, fx);
                }
                Keep::Closing => {
                    m.composer.status = "Draft saved".into();
                    if let Some(sentence) = closing_sentence(m) {
                        say(m, fx, sentence);
                    }
                }
            }
        }
        (FileOp::Save { keep: Keep::Saved, .. }, Err(e)) => {
            m.composer.status.clear();
            say(m, fx, format!("Could not save the draft: {e}"));
        }
        // The lane copy stays: it is the only copy that lasts.
        (FileOp::Save { .. }, Err(e)) => say(m, fx, format!("Could not keep the unsent message: {e}")),
        (FileOp::Clear { after_send, .. }, Ok(())) => {
            m.composer.kept = Ok(None);
            m.composer.kept_unsure = false;
            release_lane(m, release, fx);
            if after_send {
                clear_notice(m, fx);
            }
        }
        (FileOp::Clear { after_send: true, .. }, Err(e)) => {
            say(m, fx, format!("The message was sent; the draft is still on disk: {e}"))
        }
        // Closing an empty composer: a file that could not be removed is reopened by Compose.
        (FileOp::Clear { .. }, Err(_)) => {}
    }
    next_file_op(m, fx);
}

/// What Cancel's kept draft is said as, which depends on a send pressed before it: still going,
/// landed since (its own sentence already says where the edit went, and stays), or none.
fn closing_sentence(m: &Model) -> Option<&'static str> {
    if m.composer.sending.is_some() {
        Some("What was in the composer is kept as a draft; Compose reopens it. The message is still being sent as it was when Send was pressed.")
    } else if m.composer.closed_during_send {
        None
    } else {
        Some("The unsent message was kept as a draft; Compose reopens it.")
    }
}

/// A message that was being sent when the app last closed: written to the draft file, marked as
/// of unknown outcome, and said. Called once the first look has landed or failed, so the look's
/// own notice cannot clear the sentence. Its lane copy goes when the file has landed.
pub fn offer_unsent(m: &mut Model, fx: &mut Effects<Email>) {
    let Some(unsent) = m.composer.unsent_at_start.take() else { return };
    file_op(m, FileOp::Save { draft: unsent, keep: Keep::Quietly, unsure: true, release_lane: true }, fx);
    let sentence = if m.notice.is_empty() { UNSENT_AT_START.to_string() } else { format!("{} {UNSENT_AT_START}", m.notice) };
    say(m, fx, sentence);
}
