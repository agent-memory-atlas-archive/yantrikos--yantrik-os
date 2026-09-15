//! The machine can update itself, and an agent can ask it to.
//!
//! `yantrik-update` (a script shipped in `/opt/yantrik/bin`) does the real work: read the release
//! manifest, download the channel's bundle, verify its sha256, back up the current binaries, swap
//! them in, restart the shell, and roll back if the new shell does not answer. This exposes two
//! of its verbs on the shell's control surface so the whole thing is drivable the same way
//! everything else in this OS is — without a terminal and without a person.
//!
//! `check_update` is safe: it only reads. `apply_update` is `dangerous` and defers, because it
//! replaces the running system and then restarts it — the caller must watch the version settle,
//! not treat the call as the update.

use std::process::Command;

use slint::ComponentHandle;
use yantrik_app_runtime::control::{Action, App as ControlSurface, Param};

use crate::App;

/// Where the updater lives: beside this binary. The shell is started from `/opt/yantrik/bin`, and
/// nothing puts that on `PATH`, so a bare `Command::new("yantrik-update")` would ENOENT on a
/// clean install — the same trap the app launcher already learned.
fn update_bin() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("yantrik-update")))
        .unwrap_or_else(|| std::path::PathBuf::from("yantrik-update"))
}

/// Strip ANSI colour so the updater's human output is clean JSON when it travels over the socket.
fn plain(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut s = String::with_capacity(line.len());
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                // Skip a CSI escape: ESC [ ... letter.
                while let Some(&n) = chars.peek() {
                    chars.next();
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                s.push(c);
            }
        }
        let s = s.trim().to_string();
        if !s.is_empty() {
            out.push(s);
        }
    }
    out
}

/// Add the update actions to the shell's control surface.
pub fn actions(surface: ControlSurface, _ui: &App) -> ControlSurface {
    surface
        .action(
            Action::new(
                "check_update",
                "Ask the release server whether a newer build is available",
            )
            .risk("safe")
            .arg(
                Param::text("channel")
                    .describe("Release channel: stable, beta, or nightly (default: the configured one)")
                    .optional(),
            ),
            move |args| {
                let bin = update_bin();
                if !bin.exists() {
                    return Err(format!(
                        "no updater at {} — this machine cannot check for updates",
                        bin.display()
                    ));
                }
                let mut cmd = Command::new(&bin);
                cmd.arg("check");
                if let Some(channel) = args["channel"].as_str().filter(|c| !c.trim().is_empty()) {
                    cmd.args(["--channel", channel.trim()]);
                }
                let out = cmd
                    .output()
                    .map_err(|e| format!("could not run the updater: {e}"))?;
                // The updater's exit code is the answer: 0 current, 10 an update is available,
                // anything else an error (server unreachable, channel unpublished).
                let code = out.status.code().unwrap_or(-1);
                let detail = plain(&String::from_utf8_lossy(&out.stdout));
                let stderr = plain(&String::from_utf8_lossy(&out.stderr));
                match code {
                    0 => Ok(serde_json::json!({
                        "update_available": false,
                        "up_to_date": true,
                        "detail": detail,
                    })),
                    10 => Ok(serde_json::json!({
                        "update_available": true,
                        "up_to_date": false,
                        "detail": detail,
                    })),
                    _ => Err(format!(
                        "update check failed: {}",
                        stderr.last().or_else(|| detail.last()).cloned().unwrap_or_default()
                    )),
                }
            },
        )
        .action(
            // Dangerous and deferred, both meant. It replaces every binary on the machine and
            // restarts the shell you are talking to; the socket you asked on goes away and comes
            // back. The updater verifies the download and rolls back a shell that will not start,
            // so the machine cannot be left dark — but the caller still must watch the build
            // settle rather than believe the launch was the landing.
            Action::new(
                "apply_update",
                "Download, verify, and install the channel's latest build, then restart",
            )
            .risk("dangerous")
            .defers()
            .arg(
                Param::text("channel")
                    .describe("Release channel to install from (default: the configured one)")
                    .optional(),
            )
            .arg(
                Param::flag("force")
                    .describe("Reinstall even if the channel build matches what is installed")
                    .optional(),
            ),
            move |args| {
                let bin = update_bin();
                if !bin.exists() {
                    return Err(format!(
                        "no updater at {} — this machine cannot update itself",
                        bin.display()
                    ));
                }
                // Detached in its own session with `setsid`, because the very next thing it does
                // is kill this shell. A child in the shell's process group would die with it, mid
                // swap, which is the one way this operation could brick the machine.
                let mut cmd = Command::new("setsid");
                cmd.arg(&bin).arg("apply");
                let channel = args["channel"]
                    .as_str()
                    .filter(|c| !c.trim().is_empty())
                    .map(|c| c.trim().to_string());
                if let Some(ref c) = channel {
                    cmd.args(["--channel", c]);
                }
                if args["force"].as_bool() == Some(true) {
                    cmd.arg("--force");
                }
                cmd.stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null());
                cmd.spawn()
                    .map_err(|e| format!("could not start the updater: {e}"))?;
                Ok(serde_json::json!({
                    "applying": true,
                    "channel": channel.unwrap_or_else(|| "configured".to_string()),
                    "watch": "the shell will restart; reconnect and `describe shell`, or run `yantrik-update status`, to see the new build. A shell that fails to come up is rolled back automatically.",
                }))
            },
        )
}
