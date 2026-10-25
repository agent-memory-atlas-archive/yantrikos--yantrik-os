//! Whether this copy of the app is drawn for a Mind: its window is in Mind View, where
//! `read_mind_view` turns every line on it into text for an agent.
//!
//! An app with content an agent must not read (a snippet's code, a note) masks it when
//! `Boot::for_mind` is set, and keeps what is safe to show: titles, tags, counts.
//!
//! It fails closed. Masking is on when either says so:
//! - the launch carries the marker [`FOR_MIND_VAR`], which the shell sets on every app it starts
//!   on a mind's display; or
//! - the window's Wayland display is not the person's own, by the same comparison `present` makes
//!   of two displays ([`present::elsewhere`]): a launch that lost the marker but landed on Mind
//!   View is still masked.
//!
//! The marker can only turn masking on. Nothing here reads a variable that turns it off.

use crate::present;

pub use yantrik_ipc_contracts::mind_view::{FOR_MIND_VAR, PERSON_DISPLAY};

/// Whether a window with this launch is drawn for a Mind: `marker` is the value of
/// [`FOR_MIND_VAR`] and `display` that of `WAYLAND_DISPLAY`, both as the process has them.
///
/// Any value of the marker but an empty one counts, so `0` and `false` still mask. With no display
/// named the window is not on Wayland and the display rule has nothing to compare.
pub fn drawn_for_mind(marker: Option<&str>, display: Option<&str>) -> bool {
    marker.is_some_and(|m| !m.is_empty()) || present::elsewhere(display, Some(PERSON_DISPLAY))
}

/// [`drawn_for_mind`], for this process.
pub fn from_env() -> bool {
    let marker = std::env::var(FOR_MIND_VAR).ok();
    let display = std::env::var("WAYLAND_DISPLAY").ok();
    drawn_for_mind(marker.as_deref(), display.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_marker_turns_masking_on_whatever_the_display() {
        assert!(drawn_for_mind(Some("1"), Some(PERSON_DISPLAY)));
        assert!(drawn_for_mind(Some("1"), None));
        assert!(drawn_for_mind(Some("0"), None), "no value of it turns masking off");
    }

    #[test]
    fn a_display_that_is_not_the_persons_masks_without_the_marker() {
        assert!(drawn_for_mind(None, Some("wayland-1")));
        assert!(drawn_for_mind(Some(""), Some("wayland-mind")));
    }

    #[test]
    fn the_persons_own_display_and_no_display_do_not_mask() {
        assert!(!drawn_for_mind(None, Some(PERSON_DISPLAY)));
        assert!(!drawn_for_mind(None, None));
        assert!(!drawn_for_mind(Some(""), None));
    }
}
