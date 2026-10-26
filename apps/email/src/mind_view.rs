//! Email as drawn for a Mind (`Boot::for_mind`): Mind View, where `read_mind_view` turns every line
//! on the window into text for an agent.
//!
//! The window shows the framework's one sentence (`yantrik_app::for_mind::HIDDEN`) in place of
//! every sender, address, subject, preview, body, summary and attachment name, in the list, the
//! reader and the thread. `describe` withholds the same in Private mode, so the window gives no
//! more than it does. Folders, counts, dates and flags stay.
//!
//! Visual only: `describe` and the intents are unchanged by `for_mind`.
//!
//! Where it would carry mail out, the command is refused ([`refuses`]): send, reply and forward.
//! A search matches nothing, since nothing it could match is shown. The person's saved draft is
//! never opened, saved over or cleared from here, and notices do not quote a subject.

use crate::command::{Ask, Command};

/// A command a Mind's window cannot run, by the framework's read-only rule. The companion's
/// summary and suggested reply read the message and put what they make of it on the window; Open
/// original writes the sender's HTML to a file and opens it; Send, Reply and Forward carry mail
/// out or quote it; Save draft and the composer's own edits would write over the person's draft
/// with text the window cannot show.
pub fn refuses(command: &Command) -> bool {
    matches!(
        command,
        Command::OpenOriginal
            | Command::Send(_)
            | Command::Reply { .. }
            | Command::Forward
            | Command::SaveDraft(_)
            | Command::Edited { .. }
            | Command::Ask(Ask::Summarize | Ask::SuggestReply)
    )
}

/// How a notice names a message: by its subject for the person, and for a Mind by a phrase that
/// carries no mail.
pub fn named(for_mind: bool, subject: &str) -> String {
    if for_mind { "this message".to_string() } else { format!("\u{201c}{subject}\u{201d}") }
}
