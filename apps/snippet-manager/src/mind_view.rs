//! Snippets as drawn for a Mind (`Boot::for_mind`): Mind View, where `read_mind_view` turns every
//! line on the window into text for an agent.
//!
//! The window shows [`HIDDEN`] where a snippet's code would be, in the editor and in the list's
//! previews and rows, and keeps titles, tags and language. Nothing there can change or copy the
//! code: the commands that would are refused ([`refuses`]).

use yantrik_app::rail::RailCommand;

use crate::command::Command;

/// What the window shows in place of any code where a Mind may be looking. It says only that the
/// code is hidden: the window is masked from its display as well as from the marker, and a window
/// masked from the display alone must not claim to know a Mind is there.
pub const HIDDEN: &str = "Code is hidden in this window.";

/// The code the window shows: the code itself for the person, [`HIDDEN`] for a Mind.
pub fn shown(for_mind: bool, code: &str) -> String {
    if for_mind { HIDDEN.to_string() } else { code.to_string() }
}

/// A command a Mind's window cannot run, with the reason: typing and saving would write the
/// masked text over the real code, and copying and explaining would carry the real code out of
/// the window.
pub fn refuses(command: &Command) -> Option<&'static str> {
    match command {
        Command::Edited(_) | Command::Save(_) => {
            Some("The code cannot be edited while a Mind is looking.")
        }
        Command::Copy(_) | Command::CopyNamed(_) => {
            Some("The code cannot be copied while a Mind is looking.")
        }
        // The rail's chip sends the code to the Mind in its prompt; the rail offers nothing
        // here, and this is the second lock on the same door.
        Command::AiExplain | Command::Rail(RailCommand::Activate(_)) => {
            Some("The code cannot be explained while a Mind is looking.")
        }
        _ => None,
    }
}
