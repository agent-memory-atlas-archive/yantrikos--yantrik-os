//! Text a run of Notes kept and did not save, offered back at the next start (design/notes-gate
//! §2 "Drafts", §3 "Recovery after a forced kill").
//!
//! While a note has edits not on disk, its document's draft is kept through the framework's
//! drafts lane (update/drafts.rs), named by the note's file, in the framework's [`Envelope`]. A
//! run that ends without saving them (a crash, `kill -9`, a power cut) leaves the draft behind.
//! The next start reads every draft that is a draft of a note in this library, and once the
//! library is read, weighs each against the note's file and the interrupted save the store's
//! journal held, if it was of the same note: the newer wins, and only text that differs from the
//! file is offered (update/recovery.rs). Nothing is put back until the person chooses.

use std::path::Path;

use yantrik_app::documents::draft::ENVELOPE_VERSION;
use yantrik_app::documents::{Envelope, Fingerprint, Source};
use yantrik_app::drafts::Drafts;

use crate::note::NoteText;

/// What a start found, and what is offered back.
#[derive(Debug, Default)]
pub struct Recovery {
    /// The drafts read at start, until the library is first read and they can be weighed.
    pub found: Option<Vec<Found>>,
    /// The texts offered back, newest first: the bar offers the first.
    pub offers: Vec<Offer>,
}

/// A draft read at start, of a note in this library.
#[derive(Debug, Clone)]
pub struct Found {
    pub offer: Offer,
    /// When it was written, in milliseconds since the epoch.
    pub at_ms: u64,
    /// The edit it holds.
    pub seq: u64,
}

/// A text offered back for a note.
#[derive(Debug, Clone)]
pub struct Offer {
    /// The note's file name in the vault.
    pub id: String,
    /// The draft's name, removed once the person chooses.
    pub draft: String,
    pub content: NoteText,
    /// The note's file as the draft was written over it; `None` for a note never saved, or a
    /// draft that does not say.
    pub base: Option<Fingerprint>,
}

impl Recovery {
    /// The drafts in `drafts` that are drafts of notes in the vault at `vault`. A draft of
    /// another library (the vault was moved, `YANTRIK_NOTES_DIR` changed), or one this build
    /// cannot read, is left on disk as it is: it is not this library's to offer or to remove.
    pub fn read(drafts: &Drafts, vault: &Path) -> Recovery {
        let mut found: Vec<Found> = drafts.names().filter_map(|name| of_this_library(drafts, name, vault)).collect();
        found.sort_by(|a, b| b.at_ms.cmp(&a.at_ms));
        Recovery { found: Some(found), offers: Vec::new() }
    }

    /// The offer for note `id`, if there is one.
    pub fn offer_for(&self, id: &str) -> Option<&Offer> {
        self.offers.iter().find(|o| o.id == id)
    }
}

fn of_this_library(drafts: &Drafts, name: &str, vault: &Path) -> Option<Found> {
    let envelope: Envelope<NoteText> = drafts.json(name)?;
    let path = Path::new(envelope.path.as_deref()?);
    let id = path.file_name()?.to_str()?.to_string();
    let ours = envelope.version == ENVELOPE_VERSION
        && path.parent() == Some(vault)
        && id.ends_with(".md")
        && envelope.key == name
        && Source::File(vault.join(&id)).key() == name;
    if !ours {
        tracing::info!(draft = name, "a draft that is not of a note in this library is left as it is");
        return None;
    }
    Some(Found {
        offer: Offer { id, draft: name.into(), content: envelope.content, base: envelope.baseline },
        at_ms: envelope.at,
        seq: envelope.seq,
    })
}
