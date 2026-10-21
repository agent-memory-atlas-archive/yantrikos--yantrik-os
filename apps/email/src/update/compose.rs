//! Writing: the composer, replies and forwards, sending, and the draft that survives Cancel.
//!
//! The draft is one file, written and removed in jobs, one at a time and in the order they were
//! asked for: a save asked while another is writing waits, and only the latest waits.

use yantrik_app::effects::Effects;
use yantrik_app::jobs::{Concurrency, JobKey, Scope};

use super::{clear_notice, say};
use crate::backend::calls::{self, Outgoing};
use crate::command::{Command, Listing, Work};
use crate::model::{part, FileOp, Keep, Model, SENDING};
use crate::state::{self, Draft};
use crate::{rows, Email};

/// Open the composer with whatever was being written when it was last closed. The composer used
/// to open empty every time and the half-written message was simply gone.
pub fn compose_new(m: &mut Model, fx: &mut Effects<Email>) {
    m.composer.open = true;
    fx.changed(part::STATUS);
    match m.composer.kept.clone() {
        Ok(Some(draft)) => {
            m.composer.text = draft;
            m.composer.status = "Draft restored".into();
            fx.changed(part::COMPOSE_TEXT);
        }
        Ok(None) => {
            m.composer.text = Draft::default();
            m.composer.status.clear();
            fx.changed(part::COMPOSE_TEXT);
        }
        Err(e) => {
            m.composer.status.clear();
            say(m, fx, format!("Could not read the saved draft: {e}"));
        }
    }
}

/// `compose`: the composer opened as Compose opens it, with these filled in. Sending is not
/// offered on the surface on purpose: a draft can be read before it leaves, and mail that has
/// already gone cannot be taken back.
pub fn compose(m: &mut Model, to: String, subject: String, body: String, fx: &mut Effects<Email>) {
    compose_new(m, fx);
    m.composer.text.to = to;
    m.composer.text.subject = subject;
    m.composer.text.body = body;
    fx.changed(part::COMPOSE_TEXT);
}

/// Reply, or reply to all: the composer opened on the message that is open. None of this needs a
/// network call; the message is already on screen.
pub fn reply(m: &mut Model, all: bool, fx: &mut Effects<Email>) {
    let Some(open) = m.open.as_ref() else { return };
    m.composer.text = rows::reply(&open.detail, all);
    m.composer.open = true;
    fx.changed(part::COMPOSE_TEXT);
    fx.changed(part::STATUS);
}

pub fn forward(m: &mut Model, fx: &mut Effects<Email>) {
    let detail = m.open.as_ref().map(|o| o.detail.clone()).unwrap_or_default();
    m.composer.text = rows::forward(&detail);
    m.composer.open = true;
    fx.changed(part::COMPOSE_TEXT);
    fx.changed(part::STATUS);
}

/// What the notice says when a message is offered again at start.
pub const UNSENT_AT_START: &str = "A message was being sent when Email closed, and whether it went is not known. It is kept as a draft: look in Sent before sending it again.";

/// Send what is in the composer. One at a time, and said: Send shows "Sending…" and takes no
/// press while one is out, and a press that arrives anyway is answered on the notice line.
///
/// The text is kept through the framework's draft lane before anything leaves, so a send that
/// fails, panics, or is still waiting for a worker when the window closes leaves it where Compose
/// finds it.
pub fn send(m: &mut Model, mail: Draft, fx: &mut Effects<Email>) {
    let account = m.account();
    if account.is_empty() {
        return say(m, fx, "Cannot send: no email account is configured.");
    }
    if m.composer.sending.is_some() {
        return say(m, fx, "A message is already being sent; wait for it before sending again.");
    }
    m.composer.sending = Some(mail.clone());
    m.composer.text = mail.clone();
    m.sync_status = "Sending\u{2026}".into();
    fx.changed(part::STATUS);
    fx.checkpoint_draft(SENDING, serde_json::to_vec(&mail).unwrap_or_default());
    let service = m.mail.clone();
    fx.spawn_keyed_or(
        JobKey { feature: "send", scope: Scope::App },
        Concurrency::RejectIfRunning,
        Command::Unfinished(Work::Send),
        move |_| {
            let outgoing = Outgoing { to: &mail.to, cc: &mail.cc, bcc: &mail.bcc, subject: &mail.subject, body: &mail.body };
            Command::Sent(calls::send_message(service.as_ref(), &account, &outgoing))
        },
    );
}

pub fn sent(m: &mut Model, outcome: Result<(), String>, fx: &mut Effects<Email>) {
    fx.changed(part::STATUS);
    match outcome {
        Ok(()) => {
            let went = m.composer.sending.take();
            fx.discard_draft(SENDING);
            m.sync_status = "Message sent".into();
            m.composer.status.clear();
            let edited = !m.composer.text.is_empty() && went.as_ref() != Some(&m.composer.text);
            if edited {
                // What was typed after Send stays where it was typed, and is kept.
                let text = m.composer.text.clone();
                file_op(m, FileOp::Save { draft: text, keep: Keep::Quietly }, fx);
                say(
                    m,
                    fx,
                    "The message was sent as it was when Send was pressed. What was changed since is still in the composer, kept as a draft.",
                );
            } else {
                m.composer.open = false;
                // The draft was this message; it has gone.
                file_op(m, FileOp::Clear { after_send: true }, fx);
            }
            let folder = m.folder.clone();
            // The folder again, so what was sent shows; a failure there is said by the listing.
            if let Err(e) = super::mailbox::load_folder(m, &folder, Listing::AfterSend, fx) {
                tracing::warn!(error = %e, "the folder was not read again after a send");
            }
        }
        Err(e) => {
            // The composer stays open, holding what was typed, and so does the draft.
            keep_after_send(m, fx);
            m.sync_status = "Not sent".into();
            say(m, fx, format!("Could not send the message: {e}"));
        }
    }
}

/// A send that did not go, or whose outcome is not known: the composer's text becomes the draft.
pub fn keep_after_send(m: &mut Model, fx: &mut Effects<Email>) {
    m.composer.sending = None;
    fx.discard_draft(SENDING);
    let text = m.composer.text.clone();
    if !text.is_empty() {
        file_op(m, FileOp::Save { draft: text, keep: Keep::Quietly }, fx);
    }
}

/// A message that was being sent when the app last closed: written to the draft file, its lane
/// copy dropped, and said. Called once the first look has landed, so the look's own notice
/// cannot clear the sentence.
pub fn offer_unsent(m: &mut Model, fx: &mut Effects<Email>) {
    let Some(unsent) = m.composer.unsent_at_start.take() else { return };
    file_op(m, FileOp::Save { draft: unsent, keep: Keep::Quietly }, fx);
    fx.discard_draft(SENDING);
    let sentence = if m.notice.is_empty() { UNSENT_AT_START.to_string() } else { format!("{} {UNSENT_AT_START}", m.notice) };
    say(m, fx, sentence);
}

/// Close the composer and keep what was typed. Closing it used to discard it silently, which is
/// the worst thing a mail client can do to a message that is not finished.
pub fn cancel(m: &mut Model, draft: Draft, fx: &mut Effects<Email>) {
    m.composer.open = false;
    fx.changed(part::STATUS);
    if draft.is_empty() {
        m.composer.status.clear();
        file_op(m, FileOp::Clear { after_send: false }, fx);
        return;
    }
    m.composer.text = draft.clone();
    file_op(m, FileOp::Save { draft, keep: Keep::Closing }, fx);
}

pub fn save_draft(m: &mut Model, draft: Draft, fx: &mut Effects<Email>) {
    if draft.is_empty() {
        m.composer.status.clear();
        return say(m, fx, "There is nothing in the composer to save.");
    }
    m.composer.text = draft.clone();
    file_op(m, FileOp::Save { draft, keep: Keep::Saved }, fx);
}

/// One write or removal of the draft file, in a job; or, while one is running, the next one.
fn file_op(m: &mut Model, op: FileOp, fx: &mut Effects<Email>) {
    if m.composer.writing {
        m.composer.queued = Some(op);
        return;
    }
    m.composer.writing = true;
    let path = m.files.draft.clone();
    fx.spawn_or(Command::Unfinished(Work::Draft), move |_| match op {
        FileOp::Save { draft, keep } => {
            let outcome = state::save_draft(&path, &draft);
            Command::DraftWritten { draft, keep, outcome }
        }
        FileOp::Clear { after_send } => Command::DraftCleared { after_send, outcome: state::clear_draft(&path) },
    });
}

/// The next file operation, once the one before it has landed.
pub fn next_file_op(m: &mut Model, fx: &mut Effects<Email>) {
    m.composer.writing = false;
    if let Some(op) = m.composer.queued.take() {
        file_op(m, op, fx);
    }
}

pub fn draft_written(m: &mut Model, draft: Draft, keep: Keep, outcome: Result<(), String>, fx: &mut Effects<Email>) {
    fx.changed(part::STATUS);
    match (outcome, keep) {
        (Ok(()), Keep::Quietly) => m.composer.kept = Ok(Some(draft)),
        (Ok(()), Keep::Closing) => {
            m.composer.kept = Ok(Some(draft));
            m.composer.status = "Draft saved".into();
            say(m, fx, "The unsent message was kept as a draft; Compose reopens it.");
        }
        (Ok(()), Keep::Saved) => {
            m.composer.kept = Ok(Some(draft));
            m.composer.status = "Draft saved".into();
            clear_notice(m, fx);
        }
        (Err(e), Keep::Closing | Keep::Quietly) => say(m, fx, format!("Could not keep the unsent message: {e}")),
        (Err(e), Keep::Saved) => {
            m.composer.status.clear();
            say(m, fx, format!("Could not save the draft: {e}"));
        }
    }
    next_file_op(m, fx);
}

pub fn draft_cleared(m: &mut Model, after_send: bool, outcome: Result<(), String>, fx: &mut Effects<Email>) {
    match outcome {
        Ok(()) => {
            m.composer.kept = Ok(None);
            if after_send {
                clear_notice(m, fx);
            }
        }
        Err(e) if after_send => say(m, fx, format!("The message was sent; the draft is still on disk: {e}")),
        // Closing an empty composer: a file that could not be removed is reopened by Compose.
        Err(_) => {}
    }
    next_file_op(m, fx);
}
