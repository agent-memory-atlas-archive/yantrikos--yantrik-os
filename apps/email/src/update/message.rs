//! One message: opening it, its read and star flags, deleting and archiving it, its thread, and
//! its HTML original. Each change is asked of the mail server in a job, and the answer is what
//! the server shows afterwards, read back, never the request's opinion of itself.

use serde_json::json;
use yantrik_app::effects::Effects;
use yantrik_app::intents::Origin;
use yantrik_app::jobs::{Concurrency, JobKey, Scope};
use yantrik_app::Refused;
use yantrik_ipc_contracts::email::{EmailDetail, EmailSummary};

use super::mailbox::{change_counts_of, set_rows};
use super::pick::{subject_at, target};
use super::{clear_notice, say};
use crate::backend::calls;
use crate::command::{Command, Flag, Pick, Removal, Target, Work};
use crate::model::{part, Model};
use crate::{rows, state, Email};

/// Open a message and mark it read, reporting the flags the mail server has afterwards.
pub fn open(m: &mut Model, pick: &Pick, fx: &mut Effects<Email>) -> Result<(), Refused> {
    if matches!(pick, Pick::Key(key) if *key < 0) {
        return Ok(());
    }
    let target = match target(m, pick) {
        Ok(target) => target,
        // A row that is not there, pressed: said where the person is looking.
        Err(e) if fx.origin() == Origin::Local => {
            say(m, fx, e);
            return Ok(());
        }
        Err(e) => return Err(Refused::new("no_message", e)),
    };
    let account = m.account();
    let mail = m.mail.clone();
    fx.spawn_keyed_or(
        JobKey { feature: "open", scope: Scope::App },
        Concurrency::ReplacePrevious,
        Command::Unfinished(Work::Open),
        move |cancel| {
            // Another message was opened before this one started; its result would be dropped.
            if cancel.is_set() {
                return Command::Fetched { target, outcome: Err(super::mailbox::REPLACED.to_string()) };
            }
            // From the folder the row was listed from: the same UID read out of INBOX was a
            // different message, which is what opening mail outside INBOX used to show (#275).
            let outcome = calls::get_message(mail.as_ref(), &account, &target.folder, &target.id)
                .map_err(|e| format!("Could not open that message: {e}"));
            Command::Fetched { target, outcome }
        },
    );
    Ok(())
}

pub fn fetched(m: &mut Model, target: Target, outcome: Result<EmailDetail, String>, fx: &mut Effects<Email>) {
    let detail = match outcome {
        Ok(detail) => detail,
        Err(e) => {
            // A person's press is answered on the notice line; a caller gets the sentence back.
            if fx.origin() == Origin::Local {
                say(m, fx, e.clone());
            }
            fx.settle(Err(Refused::new("not_opened", e)));
            return;
        }
    };
    m.open = Some(rows::open(&target.id, &target.folder, &detail));
    fx.changed(part::OPEN);
    m.answer = json!({
        "row": target.shown.map_or(0, |r| r + 1),
        "subject": detail.subject,
        "from": detail.from,
        "read": detail.is_read,
    });

    // Reading a message marks it read on the mail server. A failure to do that is said, so the
    // row's unread dot is never left there without an explanation.
    if !detail.is_read {
        // The flag goes to the mailbox the message is in, and the unread count that moves is
        // that mailbox's — the same reason the fetch above names its folder.
        let account = m.account();
        let mail = m.mail.clone();
        fx.spawn_or(Command::Unfinished(Work::Change), move |_| {
            let outcome = calls::mark_read(mail.as_ref(), &account, &target.folder, &target.id, true);
            Command::MarkedOnOpen { id: target.id, folder: target.folder, outcome }
        });
    } else {
        clear_notice(m, fx);
    }
}

pub fn marked_on_open(m: &mut Model, id: &str, folder: &str, outcome: Result<(), String>, fx: &mut Effects<Email>) {
    match outcome {
        Ok(()) => {
            if let Some(row) = m.rows.iter_mut().find(|r| r.id == id) {
                row.item.is_read = true;
            }
            change_counts_of(m, folder, |c| c.after_read_change(false, true), fx);
            clear_notice(m, fx);
        }
        Err(e) => say(m, fx, format!("Opened the message; could not mark it read: {e}")),
    }
}

/// Mark a message read, or set its star: the change is asked for, then the folder is listed
/// again, so what is reported is an observation of the mailbox rather than of the request.
pub fn mark(m: &mut Model, pick: &Pick, flag: Flag, toggle: bool, fx: &mut Effects<Email>) -> Result<(), Refused> {
    if matches!(pick, Pick::Key(key) if *key < 0) {
        return Ok(());
    }
    let target = match target(m, pick) {
        Ok(target) => target,
        // A press on a row that is not there changes nothing and says nothing, as before.
        Err(_) if fx.origin() == Origin::Local => return Ok(()),
        Err(e) => return Err(Refused::new("no_message", e)),
    };
    // The star turns round from the row's own flag. It was hardcoded to `true`, so the star
    // could be put on and never taken off.
    let wanted = match flag {
        Flag::Read => true,
        Flag::Starred => !toggle || !target.flagged,
    };
    let account = m.account();
    let folder = m.folder.clone();
    let mail = m.mail.clone();
    fx.spawn_or(Command::Unfinished(Work::Change), move |_| {
        // In the mailbox the row was listed from: the same UID starred in INBOX was a different
        // message's star (#288).
        let call = match flag {
            Flag::Read => calls::mark_read(mail.as_ref(), &account, &target.folder, &target.id, wanted),
            Flag::Starred => calls::mark_starred(mail.as_ref(), &account, &target.folder, &target.id, wanted),
        };
        let listed = call.is_ok().then(|| calls::list_messages(mail.as_ref(), &account, &folder, 1));
        Command::Marked { target, flag, call, listed }
    });
    Ok(())
}

pub fn marked(
    m: &mut Model,
    target: Target,
    flag: Flag,
    call: Result<(), String>,
    listed: Option<Result<Vec<EmailSummary>, String>>,
    fx: &mut Effects<Email>,
) {
    if let Err(e) = call {
        let text = match flag {
            Flag::Read => format!("Could not mark that message read: {e}"),
            Flag::Starred => format!("Could not flag that message: {e}"),
        };
        say(m, fx, text.clone());
        return fx.settle(Err(Refused::new("not_changed", text)));
    }
    let observed = match observe(m, &target.id, listed, flag, fx) {
        Ok(observed) => observed,
        Err(e) => return fx.settle(Err(Refused::new("not_observed", e))),
    };
    if flag == Flag::Read {
        change_counts_of(m, &target.folder, |c| c.after_read_change(target.was_read, observed), fx);
    }
    clear_notice(m, fx);
    let row = target.shown.unwrap_or_default();
    let key = match flag {
        Flag::Read => "read",
        Flag::Starred => "flagged",
    };
    m.answer = json!({ "row": row + 1, "subject": subject_at(m, row), key: observed });
    fx.settle(Ok(()));
}

/// The folder read back, and one message's flag read off what came.
fn observe(
    m: &mut Model,
    id: &str,
    listed: Option<Result<Vec<EmailSummary>, String>>,
    flag: Flag,
    fx: &mut Effects<Email>,
) -> Result<bool, String> {
    let folder = m.folder.clone();
    let messages = match listed {
        Some(Ok(messages)) => messages,
        Some(Err(e)) => {
            let text = format!("The change was made and the folder could not be read back: {e}");
            say(m, fx, text.clone());
            return Err(text);
        }
        None => return Err(format!("the message is no longer in {folder}")),
    };
    set_rows(m, &messages, fx);
    m.rows
        .iter()
        .find(|r| r.id == id)
        .map(|r| match flag {
            Flag::Read => r.item.is_read,
            Flag::Starred => r.item.is_flagged,
        })
        .ok_or_else(|| format!("the message is no longer in {folder}"))
}

/// Delete a message, or archive it, and check it is gone. Neither can be taken back, so the
/// message is always the row's own and its folder is always named (#288).
pub fn remove(m: &mut Model, key: i32, archive: bool, fx: &mut Effects<Email>) {
    if key < 0 {
        return;
    }
    let target = match target(m, &Pick::Key(key)) {
        Ok(target) => target,
        Err(e) => {
            tracing::warn!(error = %e, "{} failed", if archive { "archive" } else { "delete" });
            return;
        }
    };
    // The archive folder as this account names it, not Gmail's, which was hardcoded: "[Gmail]/All
    // Mail" is not a folder on any other provider, so archiving on an Outlook or a private IMAP
    // account failed every time and said nothing.
    let how = if archive { Removal::Move(m.archive_folder()) } else { Removal::Delete };
    let account = m.account();
    let folder = m.folder.clone();
    let mail = m.mail.clone();
    fx.spawn_or(Command::Unfinished(Work::Change), move |_| {
        let call = match &how {
            Removal::Delete => calls::delete_message(mail.as_ref(), &account, &target.folder, &target.id),
            Removal::Move(to) => calls::move_message(mail.as_ref(), &account, &target.folder, &target.id, to),
        };
        let listed = call.is_ok().then(|| calls::list_messages(mail.as_ref(), &account, &folder, 1));
        Command::Removed { target, how, call, listed }
    });
}

pub fn removed(
    m: &mut Model,
    target: Target,
    how: Removal,
    call: Result<(), String>,
    listed: Option<Result<Vec<EmailSummary>, String>>,
    fx: &mut Effects<Email>,
) {
    let subject = &target.subject;
    let what = match &how {
        Removal::Delete => "delete",
        Removal::Move(_) => "move",
    };
    if let Err(e) = call {
        let text = match &how {
            Removal::Delete => format!("Could not delete \u{201c}{subject}\u{201d}: {e}"),
            Removal::Move(to) => format!("Could not move \u{201c}{subject}\u{201d} to {to}: {e}"),
        };
        say(m, fx, text.clone());
        return log_failure(&how, &text);
    }
    let folder = m.folder.clone();
    let messages = match listed {
        Some(Ok(messages)) => messages,
        Some(Err(e)) => {
            let text = format!("The {what} was made and {folder} could not be read back: {e}");
            say(m, fx, text.clone());
            return log_failure(&how, &text);
        }
        None => return,
    };
    set_rows(m, &messages, fx);
    if m.rows.iter().any(|r| r.id == target.id) {
        let text = format!("The mail server reported the {what} of \u{201c}{subject}\u{201d} and it is still in {folder}.");
        say(m, fx, text.clone());
        return log_failure(&how, &text);
    }
    // Only if it was the message being read. Clearing the pane for a row somebody deleted
    // further down the list would take a message off the screen that is still in the mailbox.
    if m.open.as_ref().is_some_and(|o| o.detail.subject == *subject) {
        m.open = None;
        fx.changed(part::OPEN);
    }
    clear_notice(m, fx);
    match &how {
        Removal::Delete => {
            change_counts_of(m, &folder, |c| c.after_removal(target.was_read), fx);
            tracing::info!(subject = %subject, "message deleted");
        }
        Removal::Move(to) => {
            // Both ends of the move: it left this folder and arrived in that one.
            change_counts_of(m, to, |c| c.after_arrival(target.was_read), fx);
            change_counts_of(m, &folder, |c| c.after_removal(target.was_read), fx);
            tracing::info!(subject = %subject, to = %to, "message moved");
        }
    }
}

/// Already on screen; logged so the reason is in the app's own log too.
fn log_failure(how: &Removal, text: &str) {
    match how {
        Removal::Delete => tracing::warn!(error = %text, "delete failed"),
        Removal::Move(_) => tracing::warn!(error = %text, "archive failed"),
    }
}

/// Collapse or expand one message of the open thread.
pub fn toggle_thread(m: &mut Model, idx: i32, fx: &mut Effects<Email>) {
    let entry = usize::try_from(idx).ok().and_then(|i| m.open.as_mut()?.thread.get_mut(i));
    if let Some(entry) = entry {
        entry.is_collapsed = !entry.is_collapsed;
        fx.changed(part::OPEN);
    }
}

/// The sender's HTML, handed to a browser, for the mail whose layout the reading pane's text
/// cannot carry. The file the browser gets has every remote image taken out first: a loaded
/// pixel tells the sender this message was opened, when, and from which address, and a click
/// that only meant "show me the layout" must not send that. The notice says what was removed,
/// because a layout that opens without its pictures is a change the person is owed an
/// explanation of.
pub fn open_original(m: &mut Model, fx: &mut Effects<Email>) {
    let Some((id, html)) = m.open.as_ref().and_then(|o| Some((o.id.clone(), o.html.clone()?))) else {
        return say(m, fx, "There is no message open with an HTML original.");
    };
    let dir = m.files.originals.clone();
    let mail = m.mail.clone();
    fx.spawn_or(Command::Unfinished(Work::Original), move |_| {
        let (stripped, removed) = state::strip_remote_images(&html);
        let path = state::original_html_file(&dir, &id);
        let written = std::fs::create_dir_all(&dir)
            .map_err(|e| format!("could not make {}: {e}", dir.display()))
            .and_then(|()| std::fs::write(&path, stripped).map_err(|e| format!("could not write {}: {e}", path.display())));
        let sentence = match written {
            Err(e) => format!("The original HTML could not be written for the browser: {e}"),
            Ok(()) => match mail.open_in_browser(&path.to_string_lossy()) {
                Ok(()) => state::original_opened_note(removed),
                // Not silence: the file is there, and a person on a machine with no xdg-open
                // handler can still open it by hand if the screen says where it is.
                Err(e) => format!("The original is at {} but the browser could not be opened: {e}", path.display()),
            },
        };
        Command::OriginalOpened(sentence)
    });
}
