//! The only code that writes the window, by the part of the model that changed.

use slint::{ModelRc, VecModel};
use yantrik_app::app::Changed;
use yantrik_app::documents::OutsideState;

use crate::model::{Cue, Model};
use crate::note::NoteText;
use crate::update::{closing, recovery, said};
use crate::{BookRow, NoteRow, NotesApp, PreviewBlock};

/// The busy flag, the footer's save state, the notice line, and whether the library takes edits.
pub const STATUS: &str = "status";
/// A note was shown: everything about it, its text and its fields included.
pub const NOTE: &str = "note";
/// The open note changed: its title, word count, unsaved flag. Not its text, which the editor
/// already shows.
pub const EDIT: &str = "edit";
/// The editor's text, when the model put a text there the person did not type.
pub const CONTENT: &str = "content";
/// The notebook and tags fields, when the model set them.
pub const FIELDS: &str = "fields";
/// What the model set of the window's own state: section, search, preview, focus, dialog.
pub const CHROME: &str = "chrome";
pub const LIBRARY: &str = "library";
pub const PREVIEW: &str = "preview";
/// What an outside change asks of the person (the bar, and the file's side of the comparison),
/// and what the recovery bar offers back.
pub const OUTSIDE: &str = "outside";
/// Focus, caret and scroll, once.
pub const CUES: &str = "cues";
/// Only the model changed (what `describe` reads); nothing on screen does.
pub const MODEL_ONLY: &str = "model";

pub fn render(ui: &NotesApp, m: &Model, changed: &Changed) {
    let current = m.current();
    if changed.has(STATUS) {
        ui.set_busy(m.busy.is_some());
        ui.set_status(m.status.clone().into());
        ui.set_notice(m.notice.clone().into());
        // Said with the notice, which every load that finds the library read-only sets.
        ui.set_read_only(m.vault.read_only().is_some());
    }
    if changed.has(NOTE) || changed.has(EDIT) {
        ui.set_opened(current.is_some());
        ui.set_note_title(current.as_ref().map(|c| c.title()).unwrap_or_default().into());
        ui.set_trashed(current.as_ref().is_some_and(|c| c.trash()));
        ui.set_pinned(current.as_ref().is_some_and(|c| c.content().pinned()));
        ui.set_modified(current.as_ref().is_some_and(|c| c.unsaved()));
        ui.set_words(current.as_ref().map(|c| c.content().words()).unwrap_or(0) as i32);
    }
    if changed.has(NOTE) || changed.has(CONTENT) {
        ui.set_content(current.as_ref().map(|c| c.text().to_string()).unwrap_or_default().into());
    }
    if changed.has(NOTE) || changed.has(FIELDS) {
        ui.set_notebook(m.pane.notebook.clone().into());
        ui.set_tags(m.pane.tags.clone().into());
    }
    if changed.has(CHROME) {
        let (heading, sentence) = closing::prompt(m).unwrap_or_default();
        ui.set_close_heading(heading.into());
        ui.set_close_said(sentence.into());
        ui.set_folder(m.pane.folder.clone().into());
        ui.set_query(m.pane.query.clone().into());
        ui.set_preview(m.pane.preview);
        ui.set_focus_mode(m.pane.focus_mode);
        ui.set_dialog(m.pane.dialog);
        ui.set_path(m.pane.path.clone().into());
    }
    if changed.has(NOTE) || changed.has(OUTSIDE) {
        // The bar, unless the person put it away with Not now: 1 a conflict, 2 a deleted file.
        let offered = current
            .as_ref()
            .filter(|_| !m.pane.outside_later)
            .and_then(|c| said::outside(c.outside()).map(|sentence| (c.outside(), sentence)));
        ui.set_outside(match offered {
            Some((OutsideState::Deleted, _)) => 2,
            Some(_) => 1,
            None => 0,
        });
        ui.set_outside_said(offered.map(|(_, sentence)| sentence).unwrap_or_default().into());
        ui.set_theirs(m.pane.theirs.as_ref().map(|t| t.text.clone()).unwrap_or_default().into());
        ui.set_theirs_gone(m.pane.theirs.is_none());
        ui.set_mine_fields(current.as_ref().map(|c| fields_line(c.content())).unwrap_or_default().into());
        ui.set_theirs_fields(m.pane.theirs.as_ref().map(fields_line).unwrap_or_default().into());
        let offered = recovery::shown(m).map(|o| said::offer(&o.content.title()));
        ui.set_recovery_said(offered.unwrap_or_default().into());
        // The note an offer is for takes no keys until the person chooses.
        let waiting = current.as_ref().is_some_and(|c| m.recovery.offer_for(c.id()).is_some());
        ui.set_recovering(waiting);
    }
    if changed.has(LIBRARY) {
        library(ui, m);
    }
    if changed.has(PREVIEW) {
        let blocks: Vec<PreviewBlock> =
            m.pane.blocks.iter().map(|b| PreviewBlock { text: b.text.clone().into(), kind: b.kind }).collect();
        ui.set_blocks(ModelRc::new(VecModel::from(blocks)));
    }
    // Last: a focus or a caret lands on what was just drawn.
    if changed.has(CUES) {
        for cue in &m.pane.cues {
            match *cue {
                Cue::Shown => {
                    ui.invoke_reset_position();
                    ui.invoke_focus_content();
                }
                Cue::CaretAt(offset) => ui.invoke_caret(offset),
                Cue::FocusEditor => ui.invoke_focus_editor(),
                Cue::FocusShortcuts => ui.invoke_focus_shortcuts(),
                Cue::FocusContent => ui.invoke_focus_content(),
            }
        }
    }
}

/// A note's sidecar in one line, for the comparison: two versions can differ there and nowhere
/// else.
fn fields_line(note: &NoteText) -> String {
    let mut parts = Vec::new();
    for (key, label) in [("notebook", "Notebook"), ("tags", "Tags")] {
        let value = note.field(key);
        if !value.is_empty() {
            parts.push(format!("{label}: {value}"));
        }
    }
    if note.pinned() {
        parts.push("Pinned".into());
    }
    if parts.is_empty() {
        "No notebook, tags or pin".into()
    } else {
        parts.join(" · ")
    }
}

fn library(ui: &NotesApp, m: &Model) {
    let listing = &m.pane.listing;
    ui.set_all_count(listing.all_count);
    ui.set_pin_count(listing.pin_count);
    ui.set_trash_count(listing.trash_count);
    let books: Vec<BookRow> = listing
        .books
        .iter()
        .map(|b| BookRow { name: b.name.clone().into(), count: b.count, selected: b.selected })
        .collect();
    ui.set_books(ModelRc::new(VecModel::from(books)));
    let rows: Vec<NoteRow> = listing
        .rows
        .iter()
        .map(|r| NoteRow {
            id: r.id.clone().into(),
            title: r.title.clone().into(),
            preview: r.preview.clone().into(),
            date: r.date.clone().into(),
            pinned: r.pinned,
            selected: r.selected,
        })
        .collect();
    ui.set_notes(ModelRc::new(VecModel::from(rows)));
}
