//! Power menu — wire power actions (lock, suspend, restart, shutdown).
//! Auto-saves workspace before destructive actions (restart, shutdown).

use slint::ComponentHandle;

use crate::app_context::AppContext;
use crate::App;

pub fn wire(ui: &App, ctx: &AppContext) {
    let ui_weak = ui.as_weak();
    let bridge = ctx.bridge.clone();
    ui.on_power_action(move |action| {
        let Some(ui) = ui_weak.upgrade() else { return };
        match action.as_str() {
            "lock" => {
                ui.set_current_screen(3);
                ui.set_lock_error("".into());
                tracing::info!("Screen locked via power menu");
            }
            "suspend" => {
                tracing::info!("Suspending via power menu");
                power("suspend");
            }
            "restart" => {
                // Auto-save workspace before restart
                tracing::info!("Auto-saving workspace before restart");
                bridge.send_message(
                    "Save my current workspace — I'm restarting.".to_string(),
                );
                std::thread::sleep(std::time::Duration::from_secs(2));
                tracing::info!("Restarting via power menu");
                power("reboot");
            }
            "shutdown" => {
                // Auto-save workspace before shutdown
                tracing::info!("Auto-saving workspace before shutdown");
                bridge.send_message(
                    "Save my current workspace — I'm shutting down.".to_string(),
                );
                std::thread::sleep(std::time::Duration::from_secs(2));
                tracing::info!("Shutting down via power menu");
                power("poweroff");
            }
            _ => {
                tracing::warn!(action = action.as_str(), "Unknown power action");
            }
        }
    });
}

/// Suspend, restart or power off through logind, which lets the person at this machine's active
/// session do so without sudo. These used to be `sudo zzz` (an Alpine command that does not exist
/// on the Debian this OS ships, so Suspend did nothing), `sudo reboot` and `sudo poweroff`, which
/// needed the account to have passwordless sudo for everything (#397). Said in the log when it
/// fails, rather than dropped.
fn power(verb: &'static str) {
    std::thread::spawn(move || match std::process::Command::new("systemctl").arg(verb).output() {
        Ok(out) if out.status.success() => {}
        Ok(out) => tracing::warn!(
            verb,
            error = %String::from_utf8_lossy(&out.stderr).trim(),
            "systemctl refused the power action"
        ),
        Err(e) => tracing::warn!(verb, error = %e, "could not run systemctl"),
    });
}
