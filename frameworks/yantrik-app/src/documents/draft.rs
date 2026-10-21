//! Drafts per document, ordered by `seq`, and the rule that decides what to offer back at a
//! start: the newest of the file, an interrupted save's journal, and the draft — and only one
//! that differs from what the file already says.

use serde::{Deserialize, Serialize};

use super::Document;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DraftState {
    None,
    /// A draft of this `seq` is asked for (or on disk).
    Kept { seq: u64 },
    /// The draft could not be kept: recovery after a crash is not protected, and the person
    /// is told.
    Failed { sentence: String },
}

/// What a draft file holds: the app's content, with what it was a draft of and when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope<C> {
    pub version: u32,
    /// The document's `Source::key()`.
    pub key: String,
    /// The file it is a draft of, if any, so a recovered draft can be shown beside it.
    pub path: Option<String>,
    pub seq: u64,
    /// Seconds since the epoch, when it was made.
    pub at: u64,
    pub content: C,
}

/// The draft envelope's version.
pub const ENVELOPE_VERSION: u32 = 1;

impl<C: Clone + Serialize> Document<C> {
    /// The draft to keep for this document now, if one is owed (it is dirty and the draft is
    /// older than the edits). The app passes the bytes to `fx.save_draft(name, bytes)` and then
    /// calls [`Document::draft_asked`].
    pub fn draft_owed(&self, now: u64) -> Option<(String, Vec<u8>)> {
        if !self.dirty() {
            return None;
        }
        if let DraftState::Kept { seq } = self.draft {
            if seq >= self.seq {
                return None;
            }
        }
        let envelope = Envelope {
            version: ENVELOPE_VERSION,
            key: self.source.key(),
            path: self.source.path().map(|p| p.display().to_string()),
            seq: self.seq,
            at: now,
            content: self.content.clone(),
        };
        let bytes = serde_json::to_vec(&envelope).ok()?;
        Some((self.source.key(), bytes))
    }

    pub fn draft_asked(&mut self) {
        self.draft = DraftState::Kept { seq: self.seq };
    }

    pub fn draft_failed(&mut self, sentence: impl Into<String>) {
        self.draft = DraftState::Failed { sentence: sentence.into() };
    }

    /// Whether the draft can be discarded: a save of its `seq` or later has committed.
    pub fn draft_superseded(&self) -> bool {
        match self.draft {
            DraftState::Kept { seq } => self.saved_seq >= seq,
            DraftState::Failed { .. } => !self.dirty(),
            DraftState::None => false,
        }
    }

    pub fn draft_discarded(&mut self) {
        self.draft = DraftState::None;
    }
}

/// What a start should do with a document's file, its journal and its draft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recovered<C> {
    /// Nothing differs from the file: open it as it is.
    File,
    /// Offer this text back, from a draft.
    Draft(C),
    /// Offer this text back, from an interrupted save.
    Journal(C),
}

/// The precedence at a start. Each candidate is `(content, seconds since the epoch)`. A
/// candidate equal to the file's content is not offered; of the rest, the newest wins, and a
/// draft wins a tie (it is the later of the two writes an edit makes).
pub fn recover<C: PartialEq + Clone>(
    file: Option<(&C, u64)>,
    journal: Option<(C, u64)>,
    draft: Option<(C, u64)>,
) -> Recovered<C> {
    let differs = |c: &C| file.map_or(true, |(f, _)| f != c);
    let journal = journal.filter(|(c, _)| differs(c));
    let draft = draft.filter(|(c, _)| differs(c));
    match (journal, draft) {
        (None, None) => Recovered::File,
        (Some((j, _)), None) => Recovered::Journal(j),
        (None, Some((d, _))) => Recovered::Draft(d),
        (Some((j, jt)), Some((d, dt))) => {
            if jt > dt {
                Recovered::Journal(j)
            } else {
                Recovered::Draft(d)
            }
        }
    }
}
