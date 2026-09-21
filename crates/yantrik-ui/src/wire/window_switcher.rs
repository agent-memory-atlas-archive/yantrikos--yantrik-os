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

        // wlrctl exits non-zero when nothing matched, which is the interesting case: the taskbar
        // is showing a window the compositor does not have under that name.
        if !crate::windows::present(&title) {
            tracing::warn!(
                title = %title,
                "no window matched that title; the taskbar and the compositor disagree"
            );
        }
    });
}
