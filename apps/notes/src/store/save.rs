//! Saving a note: the journal first, then the store's check that the files are still what the note
//! was read from, then the text, then the sidecar, then the journal removed. Each failure says
//! honestly how much reached the disk (`Durability`), so the document model never calls a note
//! saved that is not, and never mistakes our own bytes for an outside edit.

use std::path::Path;

use yantrik_app::documents::{Durability, Fingerprint, SaveError, SaveTicket};

use super::records::Record;
use super::{read, validate, Vault, CHANGED_ON_DISK, LIMIT, META_LIMIT};
use crate::note::{now, NoteText};

/// One save of an open note: the document's ticket, and what the note's files held when it was
/// read or last written.
#[derive(Clone)]
pub struct SaveJob {
    pub id: String,
    pub ticket: SaveTicket<NoteText>,
    /// The note as its files hold it; `None` for a note never saved.
    pub on_disk: Option<NoteText>,
    /// A sidecar of ours a failed save may have left on disk instead of `on_disk`'s.
    pub meta_unsure: Option<String>,
    pub modified: u64,
}

/// A save that landed.
#[derive(Debug, Clone)]
pub struct Saved {
    pub content: NoteText,
    /// The fingerprint of the `.md` as written: the document's new baseline.
    pub written: Fingerprint,
    pub modified: u64,
}

/// A save that did not land, and the sidecar it may have left behind.
#[derive(Debug, Clone)]
pub struct SaveFailed {
    pub error: SaveError,
    pub meta_unsure: Option<String>,
}

impl SaveFailed {
    fn wrote_nothing(sentence: impl Into<String>) -> SaveFailed {
        SaveFailed { error: SaveError::new(sentence), meta_unsure: None }
    }
}

impl Vault {
    /// Saves an open note. The store refuses to write over a file that is neither what the note
    /// was read from nor a write of ours (`SaveTicket::may_replace`), and over a sidecar that is
    /// neither, with [`CHANGED_ON_DISK`].
    pub fn save(&self, job: &SaveJob) -> Result<Saved, SaveFailed> {
        let content = &job.ticket.content;
        let record = Record::of(&job.id, content, job.on_disk.as_ref(), job.modified);
        let baseline_meta = job.on_disk.as_ref().map(|d| d.meta.as_str()).unwrap_or_default();
        self.write_note(
            &job.id,
            content,
            Some(&record),
            |text| job.ticket.may_replace(text),
            |meta| meta == baseline_meta || Some(meta) == job.meta_unsure.as_deref(),
        )
    }

    /// Writes a note no file holds yet (a restore, an import): refused if anything is there.
    pub(super) fn save_new(&self, id: &str, content: &NoteText) -> Result<Saved, String> {
        let record = Record::of(id, content, None, 0);
        self.write_note(id, content, Some(&record), |text| text.is_none(), str::is_empty)
            .map_err(|f| f.error.sentence)
    }

    /// The text and the sidecar, under the journal when one is given (a recovery writes without:
    /// it is the journal being replayed).
    pub(super) fn write_note(
        &self,
        id: &str,
        content: &NoteText,
        journal: Option<&Record>,
        text_ok: impl FnOnce(Option<&Fingerprint>) -> bool,
        meta_ok: impl FnOnce(&str) -> bool,
    ) -> Result<Saved, SaveFailed> {
        validate(&content.text).map_err(SaveFailed::wrote_nothing)?;
        if content.meta.len() > META_LIMIT {
            return Err(SaveFailed::wrote_nothing("Metadata exceeds 8 KiB."));
        }
        let (text_path, meta_path) = self.files(id).map_err(SaveFailed::wrote_nothing)?;
        // Write-ahead recovery survives a crash between the text and the sidecar.
        if let Some(record) = journal {
            let json = record.json().map_err(SaveFailed::wrote_nothing)?;
            self.put(&self.journal(), &json, true).map_err(|e| SaveFailed::wrote_nothing(e.error))?;
        }
        let on_disk = read(&text_path, LIMIT).map_err(SaveFailed::wrote_nothing)?;
        let meta_on_disk = read(&meta_path, META_LIMIT).map_err(SaveFailed::wrote_nothing)?;
        let seen = on_disk.as_deref().map(|t| Fingerprint::of_bytes(t.as_bytes()));
        if !text_ok(seen.as_ref()) || !meta_ok(meta_on_disk.as_deref().unwrap_or_default()) {
            return Err(SaveFailed { error: SaveError::conflict(CHANGED_ON_DISK), meta_unsure: None });
        }
        let written = Fingerprint::of_bytes(content.text.as_bytes());
        // A file there passed the check, so it is one this save may replace; with none, a create
        // that a file made meanwhile by anyone else makes fail rather than be replaced.
        self.put(&text_path, &content.text, on_disk.is_some()).map_err(|e| SaveFailed {
            error: SaveError::after_writing(e.step.durability(), written.clone(), e.error),
            meta_unsure: None,
        })?;
        self.write_meta(&meta_path, &content.meta).map_err(|(durability, sentence)| SaveFailed {
            meta_unsure: (durability == Durability::Unknown).then(|| content.meta.clone()),
            error: SaveError::after_writing(durability, written.clone(), sentence),
        })?;
        if journal.is_some() {
            // Everything landed; only the journal is left. Kept, it replays as an identical note
            // at the next start, which is dropped; but the save is not called durable.
            self.remove(&self.journal()).map_err(|sentence| SaveFailed {
                error: SaveError::after_writing(Durability::Unknown, written.clone(), sentence),
                meta_unsure: Some(content.meta.clone()),
            })?;
        }
        Ok(Saved { content: content.clone(), written, modified: now() })
    }

    /// The sidecar, written when there is something to say or a sidecar to bring up to date.
    /// A failure says how much of the save landed: the text did, so it is at least content-only.
    fn write_meta(&self, path: &Path, meta: &str) -> Result<(), (Durability, String)> {
        let exists = path.exists();
        if meta.is_empty() && !exists {
            return Ok(());
        }
        self.put(path, meta, exists).map_err(|e| {
            let durability = match e.step.durability() {
                Durability::NotWritten => Durability::ContentOnly,
                other => other,
            };
            (durability, e.error)
        })
    }
}
