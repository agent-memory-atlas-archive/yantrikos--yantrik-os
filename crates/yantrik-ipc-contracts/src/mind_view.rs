//! How an app learns it is drawn in Mind View, where an agent reads the screen as text.

/// Set (to `1`) by the shell on every app it launches onto a mind's display
/// (`MindDisplay::mind_view`). An app reads it at start; it can only turn masking on.
pub const FOR_MIND_VAR: &str = "YANTRIK_FOR_MIND";

/// The person's own Wayland display. A window on any other is Mind View's, whatever the marker
/// says.
pub const PERSON_DISPLAY: &str = "wayland-0";
