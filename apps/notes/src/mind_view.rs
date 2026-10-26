//! Notes as drawn for a Mind (`Boot::for_mind`): Mind View, where `read_mind_view` turns every
//! line on the window into text for an agent.
//!
//! The window shows the framework's one sentence (`yantrik_app::for_mind::HIDDEN`) where a note's
//! text would be: the editor, the list's previews, the rendered preview and the other version in
//! the comparison. Titles stay, since the Mind reads them through `describe` anyway. The editor
//! takes no typing, a search matches titles and fields and not text, and Copy is refused
//! ([`refuses`]).
//!
//! Visual only: `describe` and the intents are unchanged by `for_mind`, so a Mind still reads
//! notes through them.

use crate::command::{Command, WindowAction};

/// A command a Mind's window cannot run, by the framework's read-only rule. Typing would write
/// the masked sentence over the real text; copying would carry the real text onto the clipboard.
pub fn refuses(command: &Command) -> bool {
    match command {
        Command::From { command, .. } => refuses(command),
        Command::Edit(_) | Command::Window(WindowAction::Copy) => true,
        _ => false,
    }
}
