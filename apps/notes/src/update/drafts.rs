//! The open note's draft, kept while it has edits not on disk (design §2, "Drafts are per
//! document, ordered by `seq`").
//!
//! After every command the document is asked whether a draft is owed: one is, when it is dirty
//! and its last draft is older than its edits. The framework writes it through its drafts lane
//! after a debounce, so typing is not a write per key, and a newer draft of the same note takes
//! the place of one still waiting. It is removed once a save of the same edits or later ones has
//! landed. A draft that could not be written is said, because recovery after a crash is then not
//! protected.

use std::path::Path;

use yantrik_app::documents::draft::ENVELOPE_VERSION;
use yantrik_app::documents::{Document, DraftState, Envelope, Source};
use yantrik_app::effects::Effects;

use crate::model::Model;
use crate::note::NoteText;
use crate::update::said;
use crate::view::render::STATUS;
use crate::Notes;

/// Brings the open note's draft up to date: kept when owed, removed when its edits are saved. A
/// draft that could not be written is tried again at the next edit, not at every command: the
/// failure itself arrives as a command, and must not ask for the same write again.
pub fn sync(m: &mut Model, fx: &mut Effects<Notes>) {
    let Some(id) = m.current_id() else { return };
    let Some(d) = m.documents.get_mut(id) else { return };
    if d.draft_superseded() {
        fx.discard_draft(&d.source.key());
        d.draft_discarded();
        return;
    }
    if m.draft_failed_at == Some((id, d.seq)) {
        return;
    }
    if let Some((name, bytes)) = d.draft_owed(now_ms()) {
        fx.save_draft(&name, bytes);
        d.draft_asked();
    }
}

/// A document is closing: a draft it kept is removed when nothing in it is unsaved. A document
/// closed with edits not on disk keeps its draft, so a start can offer them back.
pub fn closing(document: &Document<NoteText>, fx: &mut Effects<Notes>) {
    if document.draft != DraftState::None && !document.dirty() {
        fx.discard_draft(&document.source.key());
    }
}

/// The person chose to drop a document's edits: its draft goes with them.
pub fn discard(document: &mut Document<NoteText>, fx: &mut Effects<Notes>) {
    fx.discard_draft(&document.source.key());
    document.draft_discarded();
}

/// The latest draft of `name` reached the disk: a notice that said it could not is answered.
pub fn saved(m: &mut Model, fx: &mut Effects<Notes>, name: &str) {
    let open = m.current().is_some_and(|c| c.doc.source.key() == name);
    if open && m.notice.starts_with(said::DRAFT_NOT_KEPT_START) {
        m.notice.clear();
        fx.changed(STATUS);
    }
}

/// A draft of `name` could not be written. Said when it is the open note's and its edits are
/// still not on disk; a draft of a note already saved or closed protects nothing now.
pub fn not_saved(m: &mut Model, fx: &mut Effects<Notes>, name: &str, why: &str) {
    let Some(id) = m.current_id() else { return };
    let Some(d) = m.documents.get_mut(id) else { return };
    if d.source.key() != name || !d.dirty() {
        return;
    }
    let sentence = said::draft_not_kept(why);
    d.draft_failed(sentence.clone());
    m.draft_failed_at = Some((id, d.seq));
    m.notice = sentence;
    m.lasting_notice = false;
    fx.changed(STATUS);
}

/// A draft of a note that is no open document (another program's version that could not be
/// written as a note): its name and bytes, in the envelope a start reads. It was never on disk,
/// so it has no baseline, and putting it back creates its file.
pub(super) fn envelope(path: &Path, content: &NoteText) -> Option<(String, Vec<u8>)> {
    let source = Source::File(path.to_path_buf());
    let envelope = Envelope {
        version: ENVELOPE_VERSION,
        key: source.key(),
        path: Some(path.display().to_string()),
        seq: 1,
        at: now_ms(),
        content: content.clone(),
        baseline: None,
    };
    Some((source.key(), serde_json::to_vec(&envelope).ok()?))
}

/// Milliseconds since the epoch, as a draft's envelope counts them.
pub(super) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}
