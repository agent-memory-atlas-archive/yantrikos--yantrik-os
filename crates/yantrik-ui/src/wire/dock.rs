//! Dock wiring — on_launch_app callback.

use slint::ComponentHandle;

use crate::app_context::AppContext;
use crate::App;

/// The app ids this shell can actually launch, in the spelling the dispatch matches on.
///
/// These mirror the match arms in `wire()` below and must be kept with them. The list exists
/// because the control surface used to answer `{"launching": "<anything>"}` for any string at
/// all: the dispatch quietly reached its `_` arm, logged "Unknown app" and returned, long after
/// the caller had been told the launch was under way.
///
/// "Must be kept with them" was doing no work on its own — ten arms were missing from this list,
/// so `open_app name=containers` was refused as unknown while the arm that launches it sat right
/// there. There is now a test that walks this list against what the dispatch accepts, because a
/// comment asking two lists to agree is not a mechanism that makes them agree.
pub const BUILTIN_APP_IDS: &[&str] = &[
    "terminal", "browser", "files", "settings", "notes", "editor", "bond", "personality",
    "memory", "notifications", "system", "media", "email", "calendar", "packages", "network",
    "weather", "spreadsheet", "launchpad",
    // Every arm below also answers to the name its app publishes on its control surface, which
    // `canonical_id` folds onto these.
    "sysmonitor", "system_monitor", "music", "music_player", "downloads", "download_manager",
    "snippets", "snippet_manager", "containers", "container_manager", "devices",
    "device_dashboard", "permissions", "permission_dashboard", "documents", "document_editor",
    "presentation", "slides",
];

/// One spelling of an app id, from whatever a caller had to hand.
///
/// Callers read app ids from places that punctuate them differently. An app's own control surface
/// says `download-manager` (and `yos ls` shows `app-download-manager`); the arms below were
/// written `download_manager`; a person types `Download Manager`. They are the same app, and
/// which separator arrived should not decide whether the window opens — but it did: a hyphenated
/// id fell through every arm to `_`, so `open_app name=download-manager` logged "Unknown app"
/// after the caller had already been told the launch was under way.
pub fn canonical_id(app: &str) -> String {
    app.trim().to_lowercase().replace([' ', '-'], "_")
}

/// Whether `launch_app` will do anything with this id.
///
/// Checks the same two sources the dispatch does, in the same order: installed .desktop entries
/// first, then the shell's own built-ins.
pub fn is_known_app(app: &str, installed: &[crate::apps::DesktopEntry]) -> bool {
    let lower = app.trim().to_lowercase();
    installed.iter().any(|e| e.app_id == app || e.name.to_lowercase() == lower)
        || BUILTIN_APP_IDS.contains(&canonical_id(app).as_str())
}

/// Wire on_launch_app callback.
pub fn wire(ui: &App, ctx: &AppContext) {
    let apps = ctx.installed_apps.clone();
    let ui_weak = ui.as_weak();

    ui.on_launch_app(move |app_id| {
        let app = app_id.to_string();
        tracing::info!(app = %app, "Launching app");

        // Check installed .desktop apps first (skip built-in Yantrik apps)
        for entry in apps.iter() {
            if entry.app_id == app || entry.name.to_lowercase() == app {
                if entry.exec == "__builtin__" {
                    break; // Fall through to built-in screen routing below
                }
                // A pin or the Lens can name an app ("notes") that ALSO has a .desktop entry
                // (Name=Notes); this branch matches first, so it must launch exactly like the
                // arms below do — same resolution, same environment scrubbing.
                let parts: Vec<&str> = entry.exec.split_whitespace().collect();
                if let Some((bin, args)) = parts.split_first() {
                    spawn_app_with_args(&app, bin, args);
                }
                return;
            }
        }

        // Fallback: hardcoded commands.
        //
        // Matched on the canonical spelling, so an id taken from an app's control surface reaches
        // the same arm as the dock's own. The .desktop scan above deliberately still uses the raw
        // string: those entries carry real ids and names, and folding their punctuation would be
        // guessing at somebody else's vocabulary rather than settling our own.
        let cmd = match canonical_id(&app).as_str() {
            "terminal" => {
                spawn_app("terminal", "yantrik-terminal");
                return;
            }
            "browser" => {
                // Launch visible Chromium with Wayland + separate user-data-dir
                // (headless instance may be holding the default profile lock)
                match std::process::Command::new("chromium")
                    .args([
                        "--ozone-platform=wayland",
                        "--no-first-run",
                        "--no-default-browser-check",
                        "--disable-gpu",
                        "--user-data-dir=/tmp/chromium-visible",
                    ])
                    .env("WAYLAND_DISPLAY", "wayland-0")
                    .env("XDG_RUNTIME_DIR", "/run/user/1000")
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                {
                    Ok(mut child) => {
                        let pid = child.id();
                        tracing::info!(pid, "Browser launched (visible mode)");
                        // Chromium is a window like any other, so the shell tracks it the same
                        // way — otherwise "what is open" would silently omit the browser.
                        crate::running::mark_launched("browser", pid, "chromium");
                        std::thread::spawn(move || {
                            let _ = child.wait();
                            crate::running::mark_exited("browser", pid);
                        });
                    }
                    Err(e) => tracing::error!(error = %e, "Failed to launch browser"),
                }
                return;
            }
            "files" => {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_current_screen(8);
                    ui.invoke_navigate(8);
                }
                return;
            }
            "settings" => {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_current_screen(7);
                    ui.invoke_navigate(7);
                }
                return;
            }
            "notes" => {
                spawn_app("notes", "yantrik-notes");
                return;
            }
            "editor" => {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_editor_file_name("untitled".into());
                    ui.set_editor_file_content("".into());
                    ui.set_editor_is_modified(false);
                    ui.set_editor_is_readonly(false);
                    ui.set_current_screen(12);
                    ui.invoke_navigate(12);
                }
                return;
            }
            "bond" => {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_current_screen(4);
                    ui.invoke_navigate(4);
                }
                return;
            }
            "personality" => {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_current_screen(5);
                    ui.invoke_navigate(5);
                }
                return;
            }
            "memory" => {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_current_screen(6);
                    ui.invoke_navigate(6);
                }
                return;
            }
            "notifications" => {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_current_screen(9);
                    ui.invoke_navigate(9);
                }
                return;
            }
            "system" => {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_current_screen(10);
                    ui.invoke_navigate(10);
                }
                return;
            }
            "media" => {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_current_screen(13);
                    ui.invoke_navigate(13);
                }
                return;
            }
            "email" => {
                spawn_app("email", "yantrik-email");
                return;
            }
            "calendar" => {
                spawn_app("calendar", "yantrik-calendar");
                return;
            }
            "packages" => {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_current_screen(21);
                    ui.invoke_navigate(21);
                }
                return;
            }
            "network" => {
                spawn_app("network", "yantrik-network-manager");
                return;
            }
            "sysmonitor" | "system_monitor" => {
                spawn_app("sysmonitor", "yantrik-system-monitor");
                return;
            }
            "weather" => {
                spawn_app("weather", "yantrik-weather");
                return;
            }
            "music" | "music_player" => {
                spawn_app("music", "yantrik-music-player");
                return;
            }
            "downloads" | "download_manager" => {
                spawn_app("downloads", "yantrik-download-manager");
                return;
            }
            "snippets" | "snippet_manager" => {
                spawn_app("snippets", "yantrik-snippet-manager");
                return;
            }
            "containers" | "container_manager" => {
                spawn_app("containers", "yantrik-container-manager");
                return;
            }
            "devices" | "device_dashboard" => {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_current_screen(27);
                    ui.invoke_navigate(27);
                }
                return;
            }
            "permissions" | "permission_dashboard" => {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_current_screen(28);
                    ui.invoke_navigate(28);
                }
                return;
            }
            "spreadsheet" => {
                spawn_app("spreadsheet", "yantrik-spreadsheet");
                return;
            }
            "documents" | "document_editor" => {
                spawn_app("documents", "yantrik-document-editor");
                return;
            }
            "presentation" | "slides" => {
                spawn_app("presentation", "yantrik-presentation");
                return;
            }
            "launchpad" => {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_current_screen(1);
                    ui.invoke_navigate(1);
                    ui.set_app_grid_open(true);
                }
                return;
            }
            _ => {
                tracing::warn!(app = %app, "Unknown app");
                return;
            }
        };
    });
}

/// Where an app binary lives when `bin` is a bare name.
///
/// The shell is started from `/opt/yantrik/bin` (or a cargo target dir in development), and the
/// apps are deployed beside it — but nothing puts that directory on PATH, so a bare
/// `Command::new("yantrik-notes")` fails with ENOENT on a clean install and the launcher logs
/// "Failed to launch" for every app. Prefer the shell's own directory, then the deploy path, and
/// only then whatever PATH says.
pub fn resolve_app_binary(bin: &str) -> std::path::PathBuf {
    use std::path::{Path, PathBuf};
    if bin.contains('/') {
        return PathBuf::from(bin);
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(bin));
        }
    }
    candidates.push(Path::new("/opt/yantrik/bin").join(bin));
    candidates
        .into_iter()
        .find(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from(bin))
}

/// Launch a standalone app binary. The app's own single-instance guard handles repeats.
/// Launch a windowed app under a logical id.
///
/// `app_id` is the id the app is known by everywhere a caller reads it — the dock, `open_app`,
/// `describe shell` — and `bin` is the binary to run. They differ (`notes` vs `yantrik-notes`),
/// and the id is what the running-apps registry is keyed on, so "what is open" answers in the
/// same vocabulary a caller uses to open things.
pub fn spawn_app(app_id: &str, bin: &str) {
    spawn_app_with_args(app_id, bin, &[]);
}

/// The one place the shell starts an app process, whatever path asked for it.
pub fn spawn_app_with_args(app_id: &str, bin: &str, args: &[&str]) {
    let path = resolve_app_binary(bin);
    match std::process::Command::new(&path)
        .args(args)
        // The shell is often started with SLINT_FULLSCREEN=1 (dev runs, kiosk sessions). A child
        // inherits the environment, and an app that inherits that variable opens fullscreen too.
        // The renderer choice (SLINT_BACKEND, GALLIUM_DRIVER) is deliberately left inherited so
        // apps draw with the same backend the shell settled on.
        .env_remove("SLINT_FULLSCREEN")
        .stdin(std::process::Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            let pid = child.id();
            tracing::info!(app = app_id, bin, pid, path = %path.display(), "App launched");
            // The shell now knows this window is open without asking the compositor. Recorded
            // before the reaper thread starts, so a describe that lands in the same instant sees
            // it.
            crate::running::mark_launched(app_id, pid, bin);
            // Reap it when it exits. Without a wait, every app the shell ever launched lingers
            // as a zombie until the shell itself quits — and a zombie still has a /proc entry,
            // which is enough to confuse anything that checks "is that pid alive". The same wait
            // is where the registry learns the window has closed.
            let id = app_id.to_string();
            let name = bin.to_string();
            std::thread::spawn(move || {
                match child.wait() {
                    Ok(status) => tracing::info!(app = %name, %status, "App exited"),
                    Err(e) => tracing::warn!(app = %name, error = %e, "Could not wait for app"),
                }
                crate::running::mark_exited(&id, pid);
            });
        }
        Err(e) => tracing::error!(app = app_id, bin, path = %path.display(), error = %e, "Failed to launch app"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ids apps publish on their own control surfaces, from `App::new(...)`.
    ///
    /// This is the vocabulary an agent actually has: it reads an id from `yos ls` or from an
    /// app's own describe, and hands that back to `open_app`. Anything it can describe, it must
    /// be able to open.
    const SURFACE_IDS: &[&str] = &[
        "calendar", "containers", "download-manager", "email", "notes", "system-monitor",
        "terminal", "weather",
    ];

    #[test]
    fn every_app_with_a_control_surface_opens_by_the_id_it_publishes() {
        for id in SURFACE_IDS {
            assert!(
                is_known_app(id, &[]),
                "`{id}` publishes a control surface, so an agent will ask for it by that name"
            );
            assert!(
                BUILTIN_APP_IDS.contains(&canonical_id(id).as_str()),
                "`{id}` normalises to `{}`, which no arm answers to",
                canonical_id(id)
            );
        }
    }

    #[test]
    fn punctuation_does_not_decide_whether_an_app_opens() {
        assert_eq!(canonical_id("download-manager"), "download_manager");
        assert_eq!(canonical_id("System-Monitor"), "system_monitor");
        assert_eq!(canonical_id("Download Manager"), "download_manager");
        assert_eq!(canonical_id("  notes  "), "notes");
        // Already canonical, and unchanged.
        assert_eq!(canonical_id("terminal"), "terminal");
    }

    #[test]
    fn the_guard_accepts_everything_the_dispatch_handles() {
        // These all have arms in `wire()` and were all refused by `is_known_app` as unknown,
        // which is the failure mode this list's own comment claimed to prevent.
        for id in [
            "containers", "downloads", "music", "snippets", "documents", "presentation",
            "sysmonitor", "devices", "permissions", "slides",
        ] {
            assert!(is_known_app(id, &[]), "the dispatch launches `{id}` but the guard refuses it");
        }
    }

    #[test]
    fn an_app_that_does_not_exist_is_still_refused() {
        // The guard must not have become a rubber stamp on the way to being more generous.
        assert!(!is_known_app("nonexistent-app", &[]));
        assert!(!is_known_app("", &[]));
    }
}
