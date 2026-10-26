//! The Text Editor as drawn for a Mind (`Boot::for_mind`): Mind View, where `read_mind_view` turns
//! every line on the window into text for an agent.
//!
//! The field shows the framework's one sentence (`yantrik_app::for_mind::HIDDEN`) in place of the
//! document, with no gutter, colours or line counts, and takes no typing. File names stay, as
//! `describe` gives them to the agent anyway. A search finds nothing, since nothing it could match
//! is shown (the match counter reads 0).
//!
//! Visual only: `describe` and the intents are unchanged by `for_mind`.

use crate::command::Command;

/// A command a Mind's window cannot run, by the framework's read-only rule: typing would write the
/// mask over the document.
pub fn refuses(command: &Command) -> bool {
    matches!(command, Command::Edit(_))
}
