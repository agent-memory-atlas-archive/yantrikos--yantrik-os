//! Close as a document flow (design §2, "Close is a document flow"; §3 "Close cancelled"),
//! through the framework's [`CloseFlow`]. Closing a window, or quitting, asks about the notes
//! with edits not on disk, once: *Save*, *Don't save* or *Cancel*. Nothing unsaved closes at once.
//! A window asks only about a note no other window shows: closing one of two windows on a note
//! asks nothing, and the note stays open, edits and all, in the other. Quitting asks once over
//! every note with edits not on disk, in every window.
//!
//! - *Cancel* keeps everything as it was and saves nothing.
//! - *Save* saves the latest edits, retrying a save that failed before, and closes only once they
//!   are on disk: every note asked about, one after another, as the store does one thing at a
//!   time. While it waits, editing goes on and the save that closes is of the latest edit. A save
//!   that fails, or cannot start because an outside change waits on the person's choice, keeps
//!   everything open and says why, and which note.
//! - *Don't save* drops the edits and their draft, and closes.
//!
//! A second close while one waits asks nothing and saves nothing more. A library open read-only
//! cannot save, so the prompt offers only *Don't save* and *Cancel*, and says Export keeps a copy.

use yantrik_app::app::WindowId;
use yantrik_app::documents::close::Stage;
use yantrik_app::documents::{Choice, CloseFlow, Closing, DocumentId};
use yantrik_app::effects::Effects;

use super::navigate::show_note;
use super::saving::{self, How};
use super::{drafts, show, windows};
use crate::model::{Current, Model};
use crate::update::said;
use crate::view::render::{CHROME, STATUS};
use crate::Notes;

/// The dialog number of the close prompt.
pub const CLOSE_DIALOG: i32 = 4;

/// The person asked a window to close.
pub fn requested(m: &mut Model, fx: &mut Effects<Notes>, window: WindowId) {
    m.keep_unsaved_dirty();
    let closing = m.documents.closing_window(window);
    ask(m, fx, Some(window), closing);
}

/// The app was asked to end: every note with edits not on disk, asked about once.
pub fn quit(m: &mut Model, fx: &mut Effects<Notes>) {
    m.keep_unsaved_dirty();
    let closing = m.documents.quitting();
    ask(m, fx, None, closing);
}

/// The session is ending: there is no one to ask. Every open note's draft is written now rather
/// than after its debounce, so the next start offers back whatever was not saved.
pub fn logging_out(m: &mut Model, fx: &mut Effects<Notes>) {
    let open: Vec<DocumentId> = m.documents.iter().map(|d| d.id).collect();
    for id in open {
        if let (Some(d), Some(file)) = (m.documents.get_mut(id), m.files.get(&id)) {
            if let Some((name, bytes)) = drafts::owed(d, file) {
                fx.checkpoint_draft(&name, bytes);
                d.draft_asked();
            }
        }
    }
    fx.exit();
}

fn ask(m: &mut Model, fx: &mut Effects<Notes>, window: Option<WindowId>, closing: Closing) {
    let asked: Vec<DocumentId> = match closing {
        Closing::Now => Vec::new(),
        Closing::Ask(documents) => documents.into_iter().filter(|d| m.unsaved_doc(*d)).collect(),
    };
    let nothing_to_ask = asked.is_empty() && unstored_at_close(m, window) == 0;
    // One close at a time: a second while the first asks or saves changes nothing. A save that
    // failed ends the flow (`failed`), so the person who asks again is asked again. Another
    // window with nothing to ask still closes: only the window whose close is asking waits.
    if let Some(flow) = &m.closing {
        let another_window = window.is_some() && flow.window.is_some() && flow.window != window;
        if !(another_window && nothing_to_ask) {
            return;
        }
    }
    if nothing_to_ask {
        // A store job in flight would land on a closed window: the window waits for it, in words.
        // Quitting does not; the framework's shutdown waits for the job within its grace.
        if m.busy.is_some() && window.is_some() {
            m.notice = said::WAIT_BEFORE_CLOSING.into();
            fx.changed(STATUS);
            return;
        }
        return close(m, fx, window);
    }
    m.closing = Some(CloseFlow::new(window, asked));
    m.pane.dialog = CLOSE_DIALOG;
    fx.changed(CHROME);
}

/// The person answered the prompt (its buttons, or Escape for *Cancel*).
pub fn answer(m: &mut Model, fx: &mut Effects<Notes>, choice: Choice) {
    let Some(flow) = m.closing.as_mut() else { return };
    let asking = flow.stage == Stage::Asking;
    let next = flow.answer(choice);
    if asking {
        // The prompt goes from the window that showed it.
        m.panes_mut().filter(|p| p.dialog == CLOSE_DIALOG).for_each(|p| p.dialog = 0);
        fx.changed(CHROME);
    }
    apply(m, fx, next);
}

/// A save the close waits on landed, and the store is idle: closed if everything asked about is
/// on disk, the next one saved if not (or the same one again, if it was edited meanwhile).
pub fn progress(m: &mut Model, fx: &mut Effects<Notes>) {
    let Some(flow) = m.closing.as_mut().filter(|f| f.stage == Stage::SavingThenClose) else { return };
    let next = flow.progress(&m.documents, None);
    apply(m, fx, next);
}

/// A save failed, or could not start. When a close waits on it, the close is over and everything
/// stays open. Returns what the notice says: `sentence`, with which note it is about when the
/// close asked about more than one.
pub fn failed(m: &mut Model, sentence: String) -> String {
    let Some(flow) = m.closing.as_mut().filter(|f| f.stage == Stage::SavingThenClose) else { return sentence };
    let several = flow.asked.len() > 1;
    flow.progress(&m.documents, Some(sentence.clone()));
    m.closing = None;
    match m.current().map(|c| c.title()) {
        Some(title) if several => said::close_failed(&title, &sentence),
        _ => sentence,
    }
}

fn apply(m: &mut Model, fx: &mut Effects<Notes>, next: yantrik_app::documents::close::Next) {
    use yantrik_app::documents::close::Next;
    match next {
        Next::Stay => m.closing = None,
        Next::Wait => {}
        Next::Save(documents) => save_then_close(m, fx, &documents),
        Next::Close { discard } => {
            if !discard.is_empty() {
                drop_edits(m, fx, &discard);
            }
            let window = m.closing.take().and_then(|f| f.window);
            close(m, fx, window);
        }
    }
}

/// *Save*: the asked notes saved, one at a time, each in a window that shows it; the close waits
/// on their landing, and `progress` starts the next.
fn save_then_close(m: &mut Model, fx: &mut Effects<Notes>, documents: &[DocumentId]) {
    let Some(next) = documents.iter().copied().find(|d| m.unsaved_doc(*d)) else {
        let window = m.closing.take().and_then(|f| f.window);
        return close(m, fx, window);
    };
    // Retried even after a failure: a close must not wait on an autosave that will not come.
    saving::save_document(m, fx, next, How::Asked);
    let waiting = m.closing.as_ref().is_some_and(|f| f.stage == Stage::SavingThenClose);
    if waiting && m.busy.is_none() {
        // Nothing started and nothing refused it (the library is not open): the window stays.
        m.notice = failed(m, said::NOT_SAVED_NOW.into());
        fx.changed(STATUS);
    }
}

/// *Don't save*: the edits and their draft dropped; the note is shown as its file holds it.
fn drop_edits(m: &mut Model, fx: &mut Effects<Notes>, documents: &[DocumentId]) {
    for id in documents {
        if let Some(d) = m.documents.get_mut(*id) {
            drafts::discard(d, fx);
        }
    }
    let again = m.current().and_then(|c| m.library.iter().find(|n| n.id == c.id() && n.trash == c.trash()).cloned());
    show_note(m, fx, None);
    show_note(m, fx, again);
    show(m, fx);
}

/// A window closes, its view with it; or, for `None`, the app ends. A close that asked has been
/// taken off `Model::closing` by now; another window's that is still asking stays.
fn close(m: &mut Model, fx: &mut Effects<Notes>, window: Option<WindowId>) {
    match window {
        Some(window) => {
            windows::forget(m, fx, window);
            fx.close(window);
        }
        None => fx.exit(),
    }
}

/// Notes listed in the library that are on no disk and in no open document: another program's
/// version that could not be written. Each is kept as a draft, and the prompt says so.
fn unstored(m: &Model) -> usize {
    let open: Vec<&str> = m.files.values().map(|f| f.id.as_str()).collect();
    m.library.iter().filter(|n| !n.stored && !n.trash && !open.contains(&n.id.as_str())).count()
}

/// The same, counted only when this close ends the app (quitting, or the last window). Closing
/// one of two windows leaves them listed in the window that stays.
fn unstored_at_close(m: &Model, window: Option<WindowId>) -> usize {
    if window.is_none() || m.windows().len() < 2 {
        unstored(m)
    } else {
        0
    }
}

/// The prompt's words for the notes asked about, and whether it offers *Save* (there is something
/// to save, and a library that may be written).
pub fn prompt(m: &Model) -> Option<(String, String, bool)> {
    let flow = m.closing.as_ref().filter(|f| f.stage == Stage::Asking)?;
    let titles: Vec<String> = flow
        .asked
        .iter()
        .filter_map(|id| Some(Current { doc: m.documents.get(*id)?, file: m.files.get(id)? }.title()))
        .collect();
    let read_only = m.vault.read_only().is_some();
    let unstored = unstored_at_close(m, flow.window);
    let (heading, sentence) = said::close_prompt(&titles, flow.window.is_none(), read_only, unstored);
    Some((heading, sentence, !titles.is_empty() && !read_only))
}

/// Escape, or the dialog put away by the window: the person cancelled the prompt.
pub fn put_away(m: &mut Model, fx: &mut Effects<Notes>) {
    if m.closing.as_ref().is_some_and(|f| f.stage == Stage::Asking) {
        answer(m, fx, Choice::Cancel);
    }
}
