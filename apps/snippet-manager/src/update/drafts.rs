//! The person's unsaved text, in the framework's draft lane, one draft per snippet; and the
//! recovery bar, which offers back what a draft holds until the person chooses.
//!
//! A snippet's text is drafted while the editor holds typing the store has not taken, debounced,
//! and at once when the app ends or a save of it is refused. A draft goes once what it holds is
//! kept and every write has landed. One that the editor has moved away from while its text is
//! still not kept (its save was refused, or a mind deleted the snippet under it) is offered back
//! on the bar, as a draft found at the next start is (`lib.rs`). Nothing is put back until the
//! person chooses: *Restore* keeps the text in its snippet (*Restore as new* for one no longer
//! kept), *Discard* throws it away. Notes offers its drafts the same way.

use yantrik_app::Refused;

use super::changes::commit;
use super::{fail, written, Fx};
use crate::command::{After, Command};
use crate::model::{draft_name, Draft, Editor, Model, Offer, Why, PART_DETAIL, PART_LIST, PART_RECOVERY};
use crate::store::ALL;

/// The editor's text drafted while it is unsaved; drafts let go once kept, or offered once the
/// editor has left them unkept. `now`: written at once rather than after the debounce.
pub(super) fn keep(m: &mut Model, fx: &mut Fx, now: bool) {
    let editor = m.editor.clone();
    let typing = editor.id >= 0 && (m.editor_dirty() || m.offered(editor.id).is_some());
    if typing && (now || m.drafted(editor.id) != Some(&editor)) {
        put(fx, &editor, now);
        // The editor's text is the newest there is for its snippet.
        m.drafts.insert(editor.id, Draft { text: editor.clone(), typed: m.typed });
    }
    // A draft goes only once a write has positively landed: one still in flight could be refused
    // and give the text back, and after a write that did not finish, memory is not known to be on
    // disk until the file has been read again (#925 N-B, #920 E-L7).
    if !m.lane.idle() || m.reloading > 0 {
        return;
    }
    let ids: Vec<i32> = m.drafts.keys().copied().collect();
    for id in ids {
        if (id == editor.id && typing) || m.offers.iter().any(|o| o.id == id) {
            continue;
        }
        let text = &m.drafts[&id].text;
        let kept = m.library.get(id).is_some() && !m.library.differs(id, &text.patch());
        if kept || id == editor.id {
            // Kept, or the editor holds this snippet as it is kept: the person's text is what the
            // store has.
            forget(m, fx, id);
        } else {
            m.offers.push(Offer { id, why: Why::NotSaved });
            fx.changed(PART_RECOVERY);
        }
    }
}

/// A text the store did not take, kept now: its draft written at once. A text older than the one
/// the draft lane already holds for its snippet is not: the newest typing always wins (#938
/// review, M1). Says whether it was held.
pub(super) fn hold(m: &mut Model, fx: &mut Fx, text: &Editor, typed: u64) -> bool {
    if m.drafts.get(&text.id).is_some_and(|d| d.typed > typed) {
        return false;
    }
    put(fx, text, true);
    m.drafts.insert(text.id, Draft { text: text.clone(), typed });
    true
}

/// The draft of `id` removed, and anything it offered with it.
pub(super) fn forget(m: &mut Model, fx: &mut Fx, id: i32) {
    if m.drafts.remove(&id).is_some() {
        fx.discard_draft(&draft_name(id));
    }
    if m.offers.iter().any(|o| o.id == id) {
        m.offers.retain(|o| o.id != id);
        fx.changed(PART_RECOVERY);
    }
}

fn put(fx: &mut Fx, text: &Editor, now: bool) {
    let Ok(bytes) = serde_json::to_vec(text) else { return };
    if now {
        fx.checkpoint_draft(&draft_name(text.id), bytes);
    } else {
        fx.save_draft(&draft_name(text.id), bytes);
    }
}

/// The bar's choices, about the first text it offers.
pub(super) fn choose(m: &mut Model, command: Command, fx: &mut Fx) {
    let Some(offer) = m.offers.first().copied() else { return };
    let Some(draft) = m.drafts.get(&offer.id).cloned() else {
        m.offers.retain(|o| o.id != offer.id);
        fx.changed(PART_RECOVERY);
        return;
    };
    match command {
        Command::ShowOffer => {
            if m.library.get(offer.id).is_some() {
                super::browsing::move_to(m, fx, offer.id, false);
            }
        }
        Command::RestoreOffer => restore(m, fx, offer, draft),
        Command::DiscardOffer => {
            forget(m, fx, offer.id);
            if m.editor.id == offer.id {
                // The editor shows the snippet as it is kept again.
                m.select(offer.id);
            }
        }
        _ => {}
    }
    fx.changed(PART_RECOVERY);
    fx.changed(PART_DETAIL);
    fx.changed(PART_LIST);
}

/// *Restore*: the text back into its snippet, opened and kept; *Restore as new* for a snippet no
/// longer kept. The typing in the editor goes in first, as any move of the editor keeps it. The
/// draft stays until what it holds is on disk (`keep`), so a refused write offers it again.
fn restore(m: &mut Model, fx: &mut Fx, offer: Offer, draft: Draft) {
    let Draft { text, typed } = draft;
    let mut restore = m.restore_point();
    let committed = match commit(m) {
        Ok(committed) => committed,
        Err(why) => return fail(m, fx, &why),
    };
    m.offers.retain(|o| o.id != offer.id);
    let outcome = if m.library.get(offer.id).is_some() {
        m.select(offer.id);
        m.editor = Editor { id: offer.id, ..text };
        commit(m).map(|_| ())
    } else {
        m.library.create(&text.title, &text.language, &text.code, &text.tags, ALL).map(|id| {
            m.select(id);
            // The draft moves to the new snippet's name before the old one goes, so the text is
            // in a draft until the new snippet is on disk; and if its write is refused, it is
            // this text the refusal keeps and says is kept (#938 review, L1).
            let moved = Editor { id, ..text.clone() };
            hold(m, fx, &moved, typed);
            forget(m, fx, offer.id);
            restore.typing = moved;
            restore.typed = typed;
        })
    };
    match outcome {
        Ok(()) => written::write(m, fx, After::Answer(serde_json::Value::Null), restore),
        Err(why) => {
            m.offers.insert(0, offer);
            fail(m, fx, &why);
            if committed {
                written::write(m, fx, After::Say(why), restore);
            }
        }
    }
}

/// The bar's button, as the window sends it.
pub fn choice(action: &str) -> Result<Command, Refused> {
    match action {
        "show" => Ok(Command::ShowOffer),
        "restore" => Ok(Command::RestoreOffer),
        "discard" => Ok(Command::DiscardOffer),
        other => Err(Refused::new("no_such_choice", format!("the recovery bar has no `{other}`"))),
    }
}

/// What the bar says and offers: the sentence, and 1 when its snippet is open in the editor, 2
/// when it is kept and not open (Show), 3 when it is no longer kept (Restore as new). `None`: no
/// bar.
pub fn bar(m: &Model) -> Option<(String, i32)> {
    let offer = m.offers.first()?;
    let text = m.drafted(offer.id)?;
    let title = format!("\u{201c}{}\u{201d}", text.title);
    let (state, said) = match (m.library.get(offer.id).is_some(), m.editor.id == offer.id, offer.why) {
        (false, _, _) => (3, format!("Your unsaved changes to {title} are kept, but that snippet is no longer kept.")),
        (true, true, Why::Closed) => (1, format!("Snippets closed before your changes to {title} were kept. They are in the editor, not saved yet.")),
        (true, false, Why::Closed) => (2, format!("Snippets closed before your changes to {title} were kept. They are kept as a draft.")),
        (true, true, Why::NotSaved) => (1, format!("Your changes to {title} could not be saved. They are in the editor and kept as a draft.")),
        (true, false, Why::NotSaved) => (2, format!("Your changes to {title} could not be saved. They are kept as a draft.")),
    };
    let more = m.offers.len();
    let said = if more > 1 { format!("{said} (1 of {more})") } else { said };
    Some((said, state))
}
