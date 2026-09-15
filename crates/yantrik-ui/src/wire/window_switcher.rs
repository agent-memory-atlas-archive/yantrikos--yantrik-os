//! Window switcher — focus a window by title via wlrctl.

use crate::app_context::AppContext;
use crate::App;

/// Focus the window with this title, restoring it first if it was minimized.
///
/// # The bug this carries the fix for
///
/// This used to run `wlrctl toplevel focus <title>`, and wlrctl's matchspec says a match with no
/// key "is assumed to be an app_id". Our Slint windows set a title and no wayland app_id at all,
/// so every click on a taskbar entry asked the compositor to focus an app_id that did not exist,
/// matched nothing, and returned success. Clicking the taskbar did nothing, quietly, for every
/// window — the result was discarded with `let _` so nothing was even logged.
///
/// The key is now given explicitly. `wlrctl` is also waited on rather than spawned and forgotten,
/// so a failure can be reported instead of vanishing.
pub fn wire(ui: &App, _ctx: &AppContext) {
    ui.on_switch_window(move |title| {
        let title = title.to_string();
        tracing::info!(title = %title, "Switching to window");

        // A minimized window cannot take focus while it is still minimized, and wlrctl has no
        // "unminimize" verb — `maximize` is what brings it back onto the screen. Applied only to
        // windows that are actually minimized, so clicking the entry for a visible window does
        // not resize it, which would be its own bug.
        let restore = std::process::Command::new("wlrctl")
            .args(["toplevel", "maximize", &format!("title:{title}"), "state:minimized"])
            .status();
        if let Err(e) = restore {
            tracing::warn!(error = %e, "wlrctl is not available; cannot restore a minimized window");
        }

        match std::process::Command::new("wlrctl")
            .args(["toplevel", "focus", &format!("title:{title}")])
            .status()
        {
            Ok(status) if status.success() => {}
            // wlrctl exits non-zero when nothing matched, which is the interesting case: the
            // taskbar is showing a window the compositor does not have under that name.
            Ok(status) => tracing::warn!(
                title = %title,
                code = status.code().unwrap_or(-1),
                "no window matched that title; the taskbar and the compositor disagree"
            ),
            Err(e) => tracing::warn!(error = %e, "could not run wlrctl to focus a window"),
        }
    });
}
