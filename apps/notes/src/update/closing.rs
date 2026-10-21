//! Close as a document flow (design §2, "Close is a document flow"; §3 "Close cancelled"),
//! through the framework's [`CloseFlow`]. Closing the window, or quitting, asks about the notes
//! with edits not on disk, once: *Save*, *Don't save* or *Cancel*. Nothing unsaved closes at once.
//!
//! - *Cancel* keeps everything as it was and saves nothing.
//! - *Save* saves the latest edits, retrying a save that failed before, and closes only once they
//!   are on disk. While it waits, editing goes on and the save that closes is of the latest edit.
//!   A save that fails, or cannot start because an outside change waits on the person's choice,
//!   keeps the window open and says why.
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
use super::{drafts, show};
use crate::model::{Current, Model};
use crate::update::said;
use crate::view::render::{CHROME, STATUS};
use crate::Notes;

/// The dialog number of the close prompt.
pub const CLOSE_DIALOG: i32 = 4;

/// The person asked the window to close.
pub fn requested(m: &mut Model, fx: &mut Effects<Notes>, window: WindowId) {
    let closing = m.documents.closing_window(window);
    ask(m, fx, Some(window), closing);
}

/// The app was asked to end: every note with edits not on disk, asked about once.
pub fn quit(m: &mut Model, fx: &mut Effects<Notes>) {
    let closing = m.documents.quitting();
    ask(m, fx, None, closing);
}

/// The session is ending: there is no one to ask. The open note's draft is written now rather
/// than after its debounce, so the next start offers back whatever was not saved.
pub fn logging_out(m: &mut Model, fx: &mut Effects<Notes>) {
    if let Some(d) = m.current_id().and_then(|id| m.documents.get_mut(id)) {
        if let Some((name, bytes)) = d.draft_owed(drafts::now_ms()) {
            fx.checkpoint_draft(&name, bytes);
            d.draft_asked();
        }
    }
    fx.exit();
}

fn ask(m: &mut Model, fx: &mut Effects<Notes>, window: Option<WindowId>, closing: Closing) {
    // One close at a time: a second while the first asks or saves changes nothing. A save that
    // failed ends the flow (`failed`), so the person who asks again is asked again.
    if m.closing.is_some() {
        return;
    }
    let asked: Vec<DocumentId> = match closing {
        Closing::Now => Vec::new(),
        Closing::Ask(documents) => documents.into_iter().filter(|d| unsaved(m, *d)).collect(),
    };
    if asked.is_empty() && unstored(m) == 0 {
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
        m.pane.dialog = 0;
        fx.changed(CHROME);
    }
    apply(m, fx, next);
}

/// A save the close waits on landed, and the store is idle: closed if everything asked about is
/// on disk, saved again if it was edited meanwhile.
pub fn progress(m: &mut Model, fx: &mut Effects<Notes>) {
    let Some(flow) = m.closing.as_mut().filter(|f| f.stage == Stage::SavingThenClose) else { return };
    let next = flow.progress(&m.documents, None);
    apply(m, fx, next);
}

/// A save the close waits on failed, or could not start: the window stays, and the notice says
/// why (the caller set it).
pub fn failed(m: &mut Model, sentence: &str) {
    if let Some(flow) = m.closing.as_mut().filter(|f| f.stage == Stage::SavingThenClose) {
        flow.progress(&m.documents, Some(sentence.into()));
        m.closing = None;
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

/// *Save*: the asked notes saved, the close waiting on their landing.
fn save_then_close(m: &mut Model, fx: &mut Effects<Notes>, documents: &[DocumentId]) {
    if !documents.iter().any(|d| unsaved(m, *d)) {
        let window = m.closing.take().and_then(|f| f.window);
        return close(m, fx, window);
    }
    // Retried even after a failure: a close must not wait on an autosave that will not come.
    saving::save(m, fx, How::Asked);
    let waiting = m.closing.as_ref().is_some_and(|f| f.stage == Stage::SavingThenClose);
    if waiting && m.busy.is_none() {
        // Nothing started and nothing refused it (the library is not open): the window stays.
        m.notice = said::NOT_SAVED_NOW.into();
        fx.changed(STATUS);
        failed(m, said::NOT_SAVED_NOW);
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

fn close(m: &mut Model, fx: &mut Effects<Notes>, window: Option<WindowId>) {
    m.closing = None;
    match window {
        Some(window) => fx.close(window),
        None => fx.exit(),
    }
}

/// Notes listed in the library that are on no disk and in no open document: another program's
/// version that could not be written. Each is kept as a draft, and the prompt says so.
fn unstored(m: &Model) -> usize {
    let open = m.current().map(|c| c.id().to_string());
    m.library.iter().filter(|n| !n.stored && !n.trash && open.as_deref() != Some(n.id.as_str())).count()
}

fn unsaved(m: &Model, id: DocumentId) -> bool {
    match (m.documents.get(id), m.files.get(&id)) {
        (Some(doc), Some(file)) => Current { doc, file }.unsaved(),
        _ => false,
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
    let (heading, sentence) = said::close_prompt(&titles, flow.window.is_none(), read_only, unstored(m));
    Some((heading, sentence, !titles.is_empty() && !read_only))
}

/// Escape, or the dialog put away by the window: the person cancelled the prompt.
pub fn put_away(m: &mut Model, fx: &mut Effects<Notes>) {
    if m.closing.as_ref().is_some_and(|f| f.stage == Stage::Asking) {
        answer(m, fx, Choice::Cancel);
    }
}
