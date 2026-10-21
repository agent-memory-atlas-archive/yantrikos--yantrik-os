//! A note whose file is gone never traps the person (#925, decision 1). Its text is on no disk, so
//! it is unsaved; but every way off it asks, through the close prompt's three answers, rather than
//! refusing as a save over it would:
//!
//! - Opening another note, making one, Refresh, Trash, Import: *Save as new* keeps the text as a
//!   new note and then goes on; *Don't save* lets the note go and goes on; *Cancel* stays.
//! - Closing its window, or quitting: the close prompt, worded for a file that is gone, whose
//!   *Save as new* keeps the text as a new note before the window closes (closing.rs).
//! - The deleted bar's own *Let it go* (*Discard*, when the person changed the note): the person
//!   deleted the file, and this honours it once they say so. The document goes from every window,
//!   with its draft, and the library lists it no more; said once, and nothing is offered later.

use yantrik_app::documents::{Choice, DocumentId, OutsideState};
use yantrik_app::effects::Effects;

use super::closing::{Prompt, CLOSE_DIALOG};
use super::navigate::show_note;
use super::{drafts, outside, said, show, window, windows};
use crate::command::{Resolution, Step, WindowAction};
use crate::model::{Model, Pending};
use crate::view::render::{CHROME, OUTSIDE, STATUS};
use crate::Notes;

/// Whether `action` leaves a note whose file is gone, and so asks first.
pub fn asks(m: &Model, action: &WindowAction) -> bool {
    action.navigates() && m.current().is_some_and(|c| *c.outside() == OutsideState::Deleted)
}

/// Asks, in the focused window, before `action` leaves its note whose file is gone.
pub fn ask(m: &mut Model, fx: &mut Effects<Notes>, action: WindowAction) {
    m.leaving = Some(Pending { step: Step::Window(action), window: m.pane.window, refuse_if_unsaved: false });
    m.pane.dialog = CLOSE_DIALOG;
    fx.changed(CHROME);
}

/// The person answered the prompt. The answer comes from the window that asked, so it acts there.
pub fn answer(m: &mut Model, fx: &mut Effects<Notes>, choice: Choice) {
    let Some(leaving) = put_down(m, fx) else { return };
    match choice {
        Choice::Cancel => {}
        // What waited goes ahead once the kept note has landed (`after_store`); a keep that
        // fails, or cannot start, takes it with it and says why.
        Choice::Save if m.busy.is_some() => say(m, fx, said::WAIT_FOR_THE_STORE),
        Choice::Save => {
            m.pending = Some(leaving);
            outside::resolve(m, fx, Resolution::KeepMine);
            if m.busy.is_none() {
                m.pending = None;
            }
        }
        Choice::DontSave => {
            let Some(sentence) = let_go(m, fx) else { return };
            if let Step::Window(action) = leaving.step {
                window::act(m, fx, action);
            }
            // Said once, unless what went ahead had something of its own to say.
            if m.notice.is_empty() {
                say(m, fx, &sentence);
            }
        }
    }
}

/// Escape, a close, or a quit: the prompt is put away, and nothing goes ahead.
pub fn put_away(m: &mut Model, fx: &mut Effects<Notes>) {
    put_down(m, fx);
}

fn put_down(m: &mut Model, fx: &mut Effects<Notes>) -> Option<Pending> {
    let leaving = m.leaving.take()?;
    if let Some(pane) = m.pane_of_mut(leaving.window).filter(|p| p.dialog == CLOSE_DIALOG) {
        pane.dialog = 0;
    }
    fx.changed(CHROME);
    Some(leaving)
}

/// The prompt's words while it asks.
pub fn prompt(m: &Model) -> Option<Prompt> {
    let leaving = m.leaving.as_ref()?;
    let c = m.pane_of(leaving.window).and_then(|p| m.current_in(p))?;
    let (heading, sentence) = said::gone_prompt(&c.title(), said::Leaving::Note, c.edited_since_gone());
    let saves = m.vault.read_only().is_none();
    Some(Prompt { heading, sentence, saves, save: if saves { said::SAVE_AS_NEW } else { said::SAVE } })
}

/// The deleted bar's *Let it go*, or *Discard*: the focused window's note whose file is gone.
pub fn let_go_pressed(m: &mut Model, fx: &mut Effects<Notes>) {
    match let_go(m, fx) {
        Some(sentence) => say(m, fx, &sentence),
        None => say(m, fx, said::NOTHING_TO_LET_GO),
    }
}

/// Lets the focused window's note whose file is gone go: its draft removed, its document closed in
/// every window that shows it, and with it its card (`show_note`). Returns what is said of it;
/// `None`, and nothing done, when the note's file is not gone.
fn let_go(m: &mut Model, fx: &mut Effects<Notes>) -> Option<String> {
    let c = m.current().filter(|c| *c.outside() == OutsideState::Deleted)?;
    let (doc, sentence): (DocumentId, String) = (c.doc.id, said::let_go(&c.title(), c.edited_since_gone()));
    if let Some(d) = m.documents.get_mut(doc) {
        drafts::discard(d, fx);
    }
    show_note(m, fx, None);
    windows::follow_to(m, fx, doc, None);
    show(m, fx);
    fx.changed(OUTSIDE);
    Some(sentence)
}

fn say(m: &mut Model, fx: &mut Effects<Notes>, sentence: &str) {
    m.notice = sentence.into();
    m.lasting_notice = false;
    fx.changed(STATUS);
}
