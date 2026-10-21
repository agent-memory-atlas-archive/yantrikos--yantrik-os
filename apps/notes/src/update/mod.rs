//! The only code that changes the model: every key, button, call from the surface, timer and
//! store job lands here as a [`Command`], and asks for its effects.
//!
//! The helpers below are the moments the window has always been brought up to date at: a note
//! shown, a note changed, the list redrawn, a store job started, a store job failed, and what
//! happens once a store job lands (the action that waited for a save, the close that waited).

use std::time::Duration;

use yantrik_app::effects::Effects;
use yantrik_app::timers::TimerId;
use yantrik_app::Refused;

use crate::command::{Command, Step};
use crate::model::{Cue, Model, Work};
use crate::note::now;
use crate::store::Vault;
use crate::view::listing;
use crate::view::preview;
use crate::view::render::{CUES, EDIT, LIBRARY, MODEL_ONLY, NOTE, PREVIEW, STATUS};
use crate::Notes;

mod editing;
mod library;
mod navigate;
pub mod saving;
mod surface;
mod window;

/// The autosave's debounce: this long after the last change, the open note is saved.
pub const AUTOSAVE: TimerId = TimerId("notes.autosave");
pub const AUTOSAVE_AFTER: Duration = Duration::from_millis(750);

/// Said when a store job ended without an answer (it panicked, or the pool refused it).
const WORKER_STOPPED: &str = "Storage worker stopped. Keep this window open to preserve your draft.";

pub fn update(m: &mut Model, command: Command, fx: &mut Effects<Notes>) -> Result<(), Refused> {
    // A cue is for the render of the command that set it, and for no later one.
    m.pane.cues.clear();
    match command {
        Command::Load => library::load(m, fx),
        Command::Window(action) => window::act(m, fx, action),
        Command::Edit(text) => editing::edit(m, fx, text),
        Command::Metadata { notebook, tags } => editing::metadata(m, fx, notebook, tags),
        Command::Filter { folder, query } => {
            m.pane.folder = folder;
            m.pane.query = query;
            relist(m, fx);
        }
        Command::ViewSynced { focus_mode, dialog } => {
            m.pane.focus_mode = focus_mode;
            m.pane.dialog = dialog;
            fx.changed(MODEL_ONLY);
        }
        Command::Surface(act) => surface::act(m, fx, act),
        Command::Autosave => saving::save(m, fx, saving::How::Autosave),
        Command::CloseRequested(window) => saving::close(m, fx, window),
        Command::FileChanged { key, seen } => saving::seen(m, fx, &key, seen),
        Command::Loaded(result) => library::loaded(m, fx, result),
        Command::SaveLanded { doc, seq, result } => saving::landed(m, fx, doc, seq, result),
        Command::Trashed { id, result } => library::trashed(m, fx, &id, result),
        Command::Restored(result) | Command::Imported(result) => library::opened(m, fx, result),
        Command::Exported(result) => library::exported(m, fx, result),
        #[cfg(test)]
        Command::UseDisk(disk) => {
            m.vault = Vault::new(m.vault.dir().to_path_buf(), disk);
            fx.changed(MODEL_ONLY);
        }
    }
    surface::finish(m, fx);
    Ok(())
}

/// The footer's words for the open note's save state.
fn save_state(m: &Model) -> &'static str {
    match m.current() {
        Some(c) if c.trash() => "Recoverable deletion",
        Some(c) if c.unsaved() => "Unsaved · autosave pending",
        _ => "All changes saved",
    }
}

/// The open note changed: its title, count and save state are said again, and the autosave's
/// debounce starts over.
fn refreshed(m: &mut Model, fx: &mut Effects<Notes>) {
    let Some(current) = m.current() else { return };
    m.status = if current.unsaved() { "Unsaved · autosave pending" } else { "All changes saved" }.into();
    fx.changed(EDIT);
    fx.changed(STATUS);
    fx.after(AUTOSAVE, AUTOSAVE_AFTER, Command::Autosave);
}

/// The pane shows the open note afresh: its undo history starts empty, its fields are the note's,
/// the list is redrawn, and the view scrolls to the top and takes the keys.
fn show(m: &mut Model, fx: &mut Effects<Notes>) {
    m.pane.history.clear();
    let current = m.current();
    let (notebook, tags) = current
        .as_ref()
        .map(|c| (c.content().field("notebook"), c.content().field("tags")))
        .unwrap_or_default();
    m.status = save_state(m).into();
    m.pane.notebook = notebook;
    m.pane.tags = tags;
    fx.changed(NOTE);
    fx.changed(STATUS);
    relist(m, fx);
    if m.pane.preview {
        reblock(m, fx);
    }
    m.pane.cues.push(Cue::Shown);
    fx.changed(CUES);
}

/// The library list, worked out again for the pane's section and search.
fn relist(m: &mut Model, fx: &mut Effects<Notes>) {
    let open = m.current().map(|c| c.as_note());
    m.pane.listing = listing::list(&m.library, open.as_ref(), &m.pane.folder, &m.pane.query, now());
    fx.changed(LIBRARY);
}

/// The preview's blocks, from the open note as it is now. Nothing open leaves them as they were.
fn reblock(m: &mut Model, fx: &mut Effects<Notes>) {
    if let Some(current) = m.current() {
        m.pane.blocks = preview::blocks(current.text());
        fx.changed(PREVIEW);
    }
}

/// Starts the store's one job. `lost` is applied if the job ends without an answer.
fn begin(
    m: &mut Model,
    fx: &mut Effects<Notes>,
    work: Work,
    lost: Command,
    job: impl FnOnce(&Vault) -> Command + Send + 'static,
) {
    m.busy = Some(work);
    m.status = "Working…".into();
    fx.changed(STATUS);
    let vault = m.vault.clone();
    fx.spawn_or(lost, move |_| job(&vault));
}

/// A store job failed: said in the notice, the draft kept, and nothing waits on it any more. A
/// call from the surface that waited for this save to go on is refused.
fn failed(m: &mut Model, fx: &mut Effects<Notes>, sentence: String) {
    if let Some(pending) = m.pending.take() {
        if pending.refuse_if_unsaved {
            if let Some(asked) = m.asked.as_mut() {
                asked.refused = Some(if sentence.is_empty() {
                    "The open note has unsaved changes that could not be written; nothing else was done."
                        .to_string()
                } else {
                    format!("The open note could not be saved, so nothing else was done: {sentence}")
                });
            }
        }
    }
    m.quitting = None;
    m.notice = sentence;
    m.status = "Needs attention · draft kept".into();
    fx.changed(STATUS);
}

/// A store job landed: once the open note is saved, what waited for it goes ahead (the action,
/// then the close); while it is not, it is saved again.
fn after_store(m: &mut Model, fx: &mut Effects<Notes>) {
    // A follow-up save is in flight; its landing decides.
    if m.busy.is_some() {
        return;
    }
    if !m.unsaved() {
        if let Some(pending) = m.pending.take() {
            run(m, fx, pending.step);
        }
        if let Some(window) = m.quitting.take() {
            fx.close(window);
        }
    } else if m.quitting.is_some() || m.pending.is_some() {
        saving::save(m, fx, saving::How::Asked);
    }
}

/// What waited for the open note to be saved.
fn run(m: &mut Model, fx: &mut Effects<Notes>, step: Step) {
    match step {
        Step::Window(action) => window::act(m, fx, action),
        Step::NewNote { title, body } => {
            let _ = navigate::new_note(m, fx, &title, &body);
        }
        Step::Import { path } => window::confirm(m, fx, window::Dialog::Import, &path),
        Step::Export { path } => window::confirm(m, fx, window::Dialog::Export, &path),
    }
}
