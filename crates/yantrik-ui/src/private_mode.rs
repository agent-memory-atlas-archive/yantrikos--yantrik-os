//! Private mode, the shell's half.
//!
//! While it is on, the Mind is off: no agent sees or does anything on this desktop, and what the
//! person does is not recorded. The rule agents meet lives in the transport
//! (`yantrik_ipc_transport::privacy`), read from `privacy.json` on every call at the mind door and
//! in every surface's dispatch. This module is the switch and what the shell itself pauses:
//!
//! - the built-in companion goes incognito (no memories, no decision model);
//! - the clipboard watcher and the activity feed record nothing;
//! - the Lens does not send a word to an attached mind; it offers to leave Private mode instead;
//! - `describe shell` says so, to the person (an agent cannot read it while private).
//!
//! Only a person turns it on or off: [`person_set_private`] is called from the mode menu's callbacks and
//! nowhere reachable from the control surface, which `control_approvals`' test holds. It lasts
//! until turned off, across restarts: the file is read back at start.

use std::sync::atomic::{AtomicBool, Ordering};

use yantrik_ipc_transport::privacy;

/// What the Lens link on the desktop's offer carries: pressing it leaves Private mode. A reserved
/// value of the message's `run`, which is otherwise a run id (`mind:main#n`) and never this.
pub const LEAVE_LINK: &str = "private:leave";

/// This process's copy of the switch, for the watchers that ask many times a second. The file
/// stays the truth for everyone else.
static ON: AtomicBool = AtomicBool::new(false);

/// Whether the person is in Private mode.
pub fn is_on() -> bool {
    ON.load(Ordering::SeqCst)
}

/// Read the switch back at start. A file that cannot be understood reads as on.
pub fn load() -> bool {
    let on = privacy::is_private();
    ON.store(on, Ordering::SeqCst);
    on
}

/// The person turned Private mode on or off. The file first: an agent is refused from the moment
/// it is written, and if it cannot be written the switch does not claim to have moved.
pub fn person_set_private(on: bool) -> std::io::Result<()> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    privacy::publish(on, now)?;
    ON.store(on, Ordering::SeqCst);
    Ok(())
}

/// What the desktop says in the Lens when the person writes to a mind while private.
pub fn lens_offer(mind: &str) -> String {
    format!(
        "Private mode is on, so {mind} is off: it cannot see or hear anything on this desktop, and \
         your words were not sent. Leave Private mode to talk to it."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lens_offer_says_nothing_was_sent() {
        let said = lens_offer("Hermes");
        assert!(said.contains("Hermes") && said.contains("not sent"), "{said}");
        assert!(!LEAVE_LINK.contains('#'), "never mistaken for a run id");
    }
}
