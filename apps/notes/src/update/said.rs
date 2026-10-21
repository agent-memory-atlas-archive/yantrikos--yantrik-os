//! What Notes says about saving, in the person's words: why a save did not commit and how much
//! of it reached the disk, that a save replaced another program's write, and what an outside
//! change asks of the person. Kept in one place so the notice line, the outside bar and a mind's
//! refusal always agree.

use yantrik_app::documents::{Durability, OutsideState, Replaced, SaveError};

/// The footer while saving waits on the person's choice about an outside change.
pub const WAITING_ON_CHOICE: &str = "Not saved · changed outside Notes";

/// Why a store job ended without an answer (it panicked, or the pool refused it), as the reason
/// in a sentence below.
pub const JOB_LOST: &str = "the storage worker stopped";

/// The notice for a save that did not commit, by how much of it reached the disk. Each one ends
/// with what Save does now, because nothing retries by itself.
pub fn save_failed(error: &SaveError) -> String {
    let why = reason(&error.sentence);
    match error.durability {
        Durability::NotWritten => {
            format!("Not saved: {why}. The file is unchanged and your text is still here; Save tries again.")
        }
        Durability::ContentOnly => format!(
            "Only the text was saved: {why}. The notebook, tags and pin were not; Save tries again, \
             and if Notes closes first they come back in a recovered note."
        ),
        Durability::Unknown => format!(
            "Saving did not finish: {why}. The note may not be safe on disk yet; your text is still \
             here, and Save tries again."
        ),
    }
}

/// Why a save was not even tried, or was refused by the store's own check: the person chooses
/// what happens to the two versions first.
pub fn blocked(outside: &OutsideState) -> &'static str {
    match outside {
        OutsideState::Deleted => {
            "Not saved: this note's file was moved or deleted. Save as new keeps your text as a new note."
        }
        _ => "Not saved: another program changed this note. Keep mine as a copy, Take theirs, or Compare them first.",
    }
}

/// What the outside bar says, or `None` when there is nothing for the person to choose. An
/// outside change to a note with nothing unsaved is not a choice: Refresh reads it, as before.
pub fn outside(outside: &OutsideState) -> Option<&'static str> {
    match outside {
        OutsideState::Conflict { .. } => {
            Some("Another program changed this note while you were editing. Nothing has been overwritten.")
        }
        OutsideState::Deleted => Some("This note's file was moved or deleted outside Notes. Your text is still here."),
        OutsideState::Clean | OutsideState::Changed { .. } => None,
    }
}

/// A save landed, and replaced what another program did to the file while it was in flight.
pub fn replaced(what: Replaced) -> &'static str {
    match what {
        Replaced::OutsideEdit => {
            "Saved. Another program changed this note while it was being saved, and this save replaced that change."
        }
        Replaced::Deletion => {
            "Saved. This note's file was moved or deleted while it was being saved, and this save wrote it again."
        }
    }
}

pub const TOOK_THEIRS: &str =
    "Now showing the version on disk. Your text was kept as a separate note with the same title.";
pub const KEPT_MINE: &str = "Your text is now a separate note. The original is left as the other program wrote it.";
pub const SAVED_AS_NEW: &str = "Your text is saved as a new note.";
/// Take theirs found no file: there was nothing to take, so the person's text is what is shown.
pub const NOTHING_TO_TAKE: &str =
    "The file was moved or deleted, so there was nothing to take. Your text is saved as a new note.";
/// Take theirs landed after more typing: the newer text is not replaced unasked.
pub const TYPED_MEANWHILE: &str = "You kept typing, so nothing was replaced. Your earlier text is also kept as \
     a separate note; choose again when you are ready.";

/// The window's text could not be kept as a note of its own, so nothing else was done.
pub fn not_kept(error: &str) -> String {
    format!(
        "Your text could not be kept as a separate note ({}), so this note was left as it is.",
        reason(error)
    )
}

/// The window's text is kept as a note, but the other version could not be read to replace it.
pub fn theirs_unread(error: &str) -> String {
    format!(
        "Your text was kept as a separate note, but the version on disk could not be read ({}), so \
         nothing was replaced.",
        reason(error)
    )
}

/// The version on disk could not be read for a comparison.
pub fn compare_unread(error: &str) -> String {
    format!("The version on disk could not be read ({}), so there is nothing to compare.", reason(error))
}

/// A reason from the store or the disk, as the middle of a sentence: no closing stop, and a
/// leading capital lowered unless it starts an acronym ("Permission denied" reads "permission
/// denied"; "UTF-8" stays).
fn reason(sentence: &str) -> String {
    let trimmed = sentence.trim().trim_end_matches('.');
    let mut chars = trimmed.chars();
    match (chars.next(), chars.next()) {
        (Some(first), second) if first.is_uppercase() && !second.is_some_and(char::is_uppercase) => {
            first.to_lowercase().chain(trimmed[first.len_utf8()..].chars()).collect()
        }
        _ => trimmed.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::reason;

    #[test]
    fn a_reason_reads_as_the_middle_of_a_sentence() {
        assert_eq!(reason("Permission denied (os error 13)"), "permission denied (os error 13)");
        assert_eq!(
            reason("A note can contain up to 256 KiB and 10,000 lines."),
            "a note can contain up to 256 KiB and 10,000 lines"
        );
        assert_eq!(reason("UTF-8 only"), "UTF-8 only");
        assert_eq!(reason("injected failure at Write"), "injected failure at Write");
        assert_eq!(reason(""), "");
    }
}
