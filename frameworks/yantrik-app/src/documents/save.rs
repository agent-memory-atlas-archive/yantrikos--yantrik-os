//! The save state machine: one save in flight per document, at most one follow-up owed, no
//! retry by itself after a failure, and the durability of a failure said honestly.

use super::{Document, DocumentId, Fingerprint, Observed, OutsideState};

/// How much of a failed save may have reached the disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Durability {
    /// Nothing changed on disk: the file is as it was before the save began.
    NotWritten,
    /// The content landed and its companion data (metadata) did not; the app's journal holds
    /// the rest.
    ContentOnly,
    /// A step after the rename failed (a directory sync): the new content may or may not
    /// survive a power loss. Treated as not saved.
    Unknown,
}

/// Why a save did not commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveError {
    pub durability: Durability,
    /// The sentence for the person, in the app's words.
    pub sentence: String,
    /// The file on disk no longer matches the baseline: an outside edit got there first.
    pub conflict: bool,
}

impl SaveError {
    pub fn new(durability: Durability, sentence: impl Into<String>) -> SaveError {
        SaveError { durability, sentence: sentence.into(), conflict: false }
    }

    pub fn conflict(sentence: impl Into<String>) -> SaveError {
        SaveError { durability: Durability::NotWritten, sentence: sentence.into(), conflict: true }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveState {
    Idle,
    Saving { seq: u64 },
    Failed { seq: u64, error: SaveError },
}

/// What a save job is given: the content as it was at `seq`, and the baseline it must find.
#[derive(Debug, Clone)]
pub struct SaveTicket<C> {
    pub document: DocumentId,
    pub seq: u64,
    pub content: C,
    pub baseline: Option<Fingerprint>,
}

/// What `begin_save` decided.
#[derive(Debug, Clone)]
pub enum BeginSave<C> {
    /// Start this save.
    Ticket(SaveTicket<C>),
    /// Nothing to save.
    Clean,
    /// A save is in flight; one follow-up is now owed.
    InFlight,
    /// The last save of this very content failed; an autosave does not retry it on its own.
    Failed,
    /// The file changed or went outside; the person must choose first.
    Blocked,
}

/// What a save's outcome did.
#[derive(Debug, Clone)]
pub enum SaveOutcome<C> {
    Saved,
    /// Saved, and the edits made meanwhile need the follow-up save this ticket is for.
    FollowUp(SaveTicket<C>),
    Failed,
    /// Saved, but another program wrote the file while ours was in flight and ours replaced it.
    OverwroteOutsideEdit,
    /// For a save this document no longer expects (an older one, after a newer outcome).
    Stale,
}

impl<C: Clone> Document<C> {
    /// An autosave or a save the app asks for on its own. A failed save of unchanged content is
    /// not retried here, so a read-only disk does not raise a notice every tick.
    pub fn begin_save(&mut self) -> BeginSave<C> {
        // Blocked says more than Failed: the person has a choice to make, not a disk to fix.
        if !self.blocked() {
            if let SaveState::Failed { seq, .. } = self.save {
                if seq == self.seq {
                    return BeginSave::Failed;
                }
            }
        }
        self.start()
    }

    /// The person asked to save: a failure is retried.
    pub fn retry_save(&mut self) -> BeginSave<C> {
        self.start()
    }

    /// The file changed or went outside: no save until the person chooses.
    pub fn blocked(&self) -> bool {
        matches!(self.outside, OutsideState::Conflict { .. } | OutsideState::Deleted)
    }

    fn start(&mut self) -> BeginSave<C> {
        if self.blocked() {
            return BeginSave::Blocked;
        }
        if matches!(self.save, SaveState::Saving { .. }) {
            if self.dirty() {
                self.follow_up = true;
            }
            return BeginSave::InFlight;
        }
        if !self.dirty() && self.baseline.is_some() {
            return BeginSave::Clean;
        }
        self.save = SaveState::Saving { seq: self.seq };
        BeginSave::Ticket(SaveTicket {
            document: self.id,
            seq: self.seq,
            content: self.content.clone(),
            baseline: self.baseline.clone(),
        })
    }

    /// The save of `seq` ended. `Ok` carries the fingerprint of what is now on disk.
    pub fn save_landed(&mut self, seq: u64, result: Result<Fingerprint, SaveError>) -> SaveOutcome<C> {
        match self.save {
            SaveState::Saving { seq: in_flight } if in_flight == seq => {}
            _ => return SaveOutcome::Stale,
        }
        let seen = self.seen_while_saving.take();
        match result {
            Ok(written) => {
                let before = self.baseline.replace(written.clone());
                self.saved_seq = self.saved_seq.max(seq);
                self.remember_write(written.clone());
                self.save = SaveState::Idle;
                // A file seen during the save that was neither the old file nor ours existed
                // between our check and our rename: ours replaced it, and the person hears so.
                let overwrote = matches!(&seen, Some(Observed::File(f)) if *f != written && Some(f) != before.as_ref());
                if matches!(self.outside, OutsideState::Changed { .. }) {
                    self.outside = OutsideState::Clean;
                }
                if self.follow_up && self.dirty() {
                    self.follow_up = false;
                    if let BeginSave::Ticket(t) = self.start() {
                        return SaveOutcome::FollowUp(t);
                    }
                }
                self.follow_up = false;
                if overwrote {
                    SaveOutcome::OverwroteOutsideEdit
                } else {
                    SaveOutcome::Saved
                }
            }
            Err(error) => {
                self.follow_up = false;
                if error.conflict {
                    self.outside = OutsideState::Conflict { now: None };
                }
                self.save = SaveState::Failed { seq, error };
                // A sighting during a save that failed is judged now, against the old baseline.
                if let Some(seen) = seen {
                    self.observe(seen);
                }
                SaveOutcome::Failed
            }
        }
    }
}
