//! What a start offers back (crate::recovery), and the person's choice about it: *Restore* puts
//! the text back into its note, *Discard* keeps the note as its file holds it.
//!
//! The text goes back into its note as edits, saved by the autosave like any other, only when the
//! note's file is still the one the draft was written over. When the file moved on since (another
//! program wrote it), or is gone, putting it back is the outside-change flow's: the note opens
//! with the text as unsaved edits and the choices the bar offers (*Keep mine as a copy*, *Take
//! theirs*, *Compare*; *Save as new* for a file gone), so neither version is lost.
//!
//! Until the person chooses, the note an offer is for takes no edits: its draft is the only copy
//! of the text offered, and a draft of new edits would be written over it.

use yantrik_app::documents::draft::recover;
use yantrik_app::documents::{Candidate, Fingerprint, OutsideState, Recovered};
use yantrik_app::effects::Effects;

use super::navigate::show_note;
use super::saving::{self, How};
use super::{refreshed, show};
use crate::command::{Act, Command, WindowAction};
use crate::model::Model;
use crate::note::Note;
use crate::recovery::{Found, Offer};
use crate::store::Interrupted;
use crate::update::said;
use crate::view::render::{CONTENT, FIELDS, OUTSIDE, STATUS};
use crate::Notes;

/// The library has been read for the first time: each draft found at start is weighed against
/// its note's file and the interrupted save the journal held, if it was the same note's. The
/// newer of draft and journal wins; one equal to the file is not offered. A draft that loses is
/// removed: the file already holds it, or the interrupted save, newer, came back as a note of
/// its own (`Vault::load`). Returns the sentence that says how many are offered, when the bar's
/// one is not all of them.
pub fn weigh(m: &mut Model, fx: &mut Effects<Notes>, interrupted: Option<&Interrupted>) -> Option<String> {
    let found = m.recovery.found.take()?;
    for Found { offer, at_ms, seq } in found {
        let file = m.library.iter().find(|n| !n.trash && n.id == offer.id).map(|n| &n.content);
        let journal = interrupted
            .filter(|j| j.id == offer.id)
            .map(|j| Candidate { content: j.content.clone(), at_ms: j.at_ms, seq: None });
        let draft = Candidate { content: offer.content.clone(), at_ms, seq: Some(seq) };
        match recover(file, journal, Some(draft)) {
            Recovered::Draft(_) => m.recovery.offers.push(offer),
            Recovered::File | Recovered::Journal(_) => fx.discard_draft(&offer.draft),
        }
    }
    fx.changed(OUTSIDE);
    // The bar names one note; the notice says when there are more.
    (m.recovery.offers.len() > 1).then(|| said::offered(m.recovery.offers.len()))
}

/// The offer the bar shows: the open note's, or else the newest.
pub fn shown(m: &Model) -> Option<&Offer> {
    let open = m.current().map(|c| c.id().to_string());
    open.and_then(|id| m.recovery.offer_for(&id)).or_else(|| m.recovery.offers.first())
}

/// *Restore*: the offered text back in its note, open in the window. Reached through the guards,
/// so the note that was open is saved first.
pub fn restore(m: &mut Model, fx: &mut Effects<Notes>) {
    let Some(offer) = shown(m).cloned() else { return say(m, fx, said::NOTHING_OFFERED) };
    m.recovery.offers.retain(|o| o.id != offer.id);
    let on_disk = m.library.iter().find(|n| !n.trash && n.id == offer.id).cloned();
    let file = on_disk.as_ref().map(|n| Fingerprint::of_bytes(n.content.text.as_bytes()));
    let moved_on = file != offer.base;
    match on_disk {
        Some(note) => show_note(m, fx, Some(note)),
        // Gone: a note of its own again, not on disk until saved, as a note made here is.
        None => show_note(m, fx, Some(Note { id: offer.id.clone(), content: offer.content.clone(), ..Note::default() })),
    }
    show(m, fx);
    let Some((d, _)) = m.current_mut() else { return };
    d.edit(|c| *c = offer.content);
    if moved_on {
        // Never a silent replace of what the file now holds, nor a silent re-creation of a file
        // someone deleted: the person chooses, as for any outside change.
        d.outside = match file {
            Some(now) => OutsideState::Conflict { now: Some(now) },
            None => OutsideState::Deleted,
        };
    }
    fx.changed(CONTENT);
    fx.changed(FIELDS);
    fx.changed(OUTSIDE);
    let sentence = if moved_on { said::RESTORED_BESIDE } else { said::RESTORED };
    refreshed(m, fx);
    say(m, fx, &left(m, sentence));
    // Saved at once, as the person's own Save would be. One the outside bar holds back is
    // refused in its words, and waits on their choice.
    if !moved_on {
        saving::save(m, fx, How::Asked);
    }
}

/// *Discard*: the offered text is dropped and its draft removed; the note is as its file holds it.
pub fn discard(m: &mut Model, fx: &mut Effects<Notes>) {
    let Some(offer) = shown(m).cloned() else { return say(m, fx, said::NOTHING_OFFERED) };
    m.recovery.offers.retain(|o| o.id != offer.id);
    fx.discard_draft(&offer.draft);
    fx.changed(OUTSIDE);
    say(m, fx, &left(m, said::DISCARDED));
}

/// Whether `command` would change a note whose offer the person has not chosen about; if so, it
/// is refused, said, and the editor is put back.
pub fn refused(m: &mut Model, fx: &mut Effects<Notes>, command: &Command) -> bool {
    let Some(id) = m.current().map(|c| c.id().to_string()) else { return false };
    if m.recovery.offer_for(&id).is_none() {
        return false;
    }
    let changes = match command {
        Command::Edit(_) | Command::Metadata { .. } => true,
        Command::Window(action) => {
            matches!(action, WindowAction::Pin | WindowAction::Undo | WindowAction::Redo | WindowAction::Save)
        }
        Command::Surface(act) => matches!(
            act,
            Act::SetTitle(_) | Act::Append(_) | Act::SetContent(_) | Act::Notebook(_) | Act::Tags(_) | Act::Undo | Act::Redo
        ),
        _ => false,
    };
    if !changes {
        return false;
    }
    fx.changed(CONTENT);
    fx.changed(FIELDS);
    if let Command::Surface(_) = command {
        super::surface::refuse(m, fx, said::CHOOSE_FIRST.into());
    } else {
        say(m, fx, said::CHOOSE_FIRST);
    }
    true
}

/// `sentence`, and how many offers are still waiting.
fn left(m: &Model, sentence: &str) -> String {
    match m.recovery.offers.len() {
        0 => sentence.to_string(),
        n => format!("{sentence} {}", said::offered(n)),
    }
}

fn say(m: &mut Model, fx: &mut Effects<Notes>, sentence: &str) {
    m.notice = sentence.into();
    m.lasting_notice = false;
    fx.changed(STATUS);
}
