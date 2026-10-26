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
//! Where it would carry mail out, the command is refused ([`refuses`]): a reply or forward starts
//! empty, and a search matches nothing, since nothing it could match is shown.

use crate::command::{Ask, Command};

/// A command a Mind's window cannot run, by the framework's read-only rule. The companion's
/// summary and suggested reply read the message and put what they make of it on the window; Open
/// original writes the sender's HTML to a file and opens it.
pub fn refuses(command: &Command) -> bool {
    matches!(command, Command::OpenOriginal | Command::Ask(Ask::Summarize | Ask::SuggestReply))
}
