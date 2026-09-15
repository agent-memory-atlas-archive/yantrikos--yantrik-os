//! What windows are open.
//!
//! The shell knows its own children, because it launched them (see [`crate::running`]). That
//! registry is the source of truth here: it is reliable, needs no subprocess, and answers in the
//! same app-id vocabulary a caller uses to open things. `wlrctl toplevel list` remains only as a
//! fallback for the case the registry cannot cover — windows the shell did not start, in a
//! development session or an unusual setup — and it never runs when the shell has launched
//! something itself.

/// A running window on the desktop.
pub struct WindowEntry {
    pub title: String,
    pub app_id: String,
    pub icon_char: String,
    pub subtitle: String,
}

/// The windows the shell has open.
///
/// From the launch registry first — the authoritative account of what the shell started and what
/// is still alive. Only if that is empty (nothing launched through the shell) does it fall back
/// to asking the compositor, so a development session where apps are started by hand still shows
/// something rather than nothing.
pub fn list_windows() -> Vec<WindowEntry> {
    let ours = shell_windows();
    if !ours.is_empty() {
        return ours;
    }
    wlrctl_windows()
}

/// The windows the shell itself has open, from the launch registry only — never a subprocess.
///
/// This is what `describe shell` reports, so it has to be two things the full `list_windows` is
/// not required to be: cheap, because it runs on the UI thread inside the describe closure under
/// a few-second budget, and correct on every screen, because an app stays open when the shell
/// navigates away from the desktop and a describe from the files screen must still see it. The
/// `wlrctl` fallback exists for the taskbar in a bare development session; it has no place on the
/// answer to "what is open".
pub fn shell_windows() -> Vec<WindowEntry> {
    crate::running::running()
        .into_iter()
        .map(|app| {
            let app_id = app.app_id;
            WindowEntry {
                title: display_name(&app_id),
                icon_char: icon_for_app(&app_id).to_string(),
                subtitle: String::new(),
                app_id,
            }
        })
        .collect()
}

/// A human name for one of our app ids: `notes` → `Notes`, `system-monitor` → `System monitor`.
fn display_name(app_id: &str) -> String {
    match app_id {
        "terminal" => "Terminal".to_string(),
        "browser" => "Browser".to_string(),
        "notes" => "Notes".to_string(),
        "email" => "Email".to_string(),
        "calendar" => "Calendar".to_string(),
        "network" => "Network".to_string(),
        "sysmonitor" => "System Monitor".to_string(),
        "weather" => "Weather".to_string(),
        "music" => "Music".to_string(),
        "downloads" => "Downloads".to_string(),
        "snippets" => "Snippets".to_string(),
        "containers" => "Containers".to_string(),
        "spreadsheet" => "Spreadsheet".to_string(),
        "documents" => "Documents".to_string(),
        "presentation" => "Presentation".to_string(),
        // Unknown id (a .desktop app the shell launched): title-case its first segment.
        other => {
            let mut c = other.replace(['-', '_'], " ");
            if let Some(first) = c.get_mut(0..1) {
                first.make_ascii_uppercase();
            }
            c
        }
    }
}

/// Ask the compositor directly. The fallback path, used only when the shell has launched nothing
/// itself. Parses `wlrctl toplevel list` and guesses an app id from each title — the old,
/// unreliable behaviour, kept for development sessions and nothing more.
fn wlrctl_windows() -> Vec<WindowEntry> {
    let output = match std::process::Command::new("wlrctl")
        .args(["toplevel", "list"])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };

    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let title = line.trim().to_string();
            let app_id = derive_app_id(&title);
            let icon_char = icon_for_app(&app_id).to_string();
            let subtitle = derive_context(&title, &app_id);
            WindowEntry {
                title,
                app_id,
                icon_char,
                subtitle,
            }
        })
        .collect()
}

/// Derive a normalized app_id from a window title (fallback path only).
fn derive_app_id(title: &str) -> String {
    let lower = title.to_lowercase();
    if lower.contains("foot") || lower.contains("terminal") {
        "terminal".to_string()
    } else if lower.contains("firefox") || lower.contains("chromium") || lower.contains("browser")
    {
        "browser".to_string()
    } else if lower.contains("file") || lower.contains("pcmanfm") || lower.contains("thunar") {
        "files".to_string()
    } else if lower.contains("yantrik") {
        "yantrik".to_string()
    } else {
        lower
            .split_whitespace()
            .next()
            .unwrap_or("unknown")
            .to_string()
    }
}

/// Map app_id to a single-char icon.
pub fn icon_for_app(app_id: &str) -> &'static str {
    match app_id {
        "terminal" => ">_",
        "browser" => "W",
        "files" => "F",
        "notes" => "\u{270E}",
        "email" => "@",
        "calendar" => "\u{25A6}",
        "weather" => "\u{2600}",
        "music" => "\u{266A}",
        "sysmonitor" => "\u{25C9}",
        "network" => "N",
        "spreadsheet" => "YS",
        "documents" => "YD",
        "presentation" => "YP",
        "yantrik" => "Y",
        _ => "?",
    }
}

/// Derive a contextual subtitle from a window title (fallback path only).
/// Terminal: extract CWD from "user@host:/path" pattern.
/// Browser: extract site name from "Page Title - Site" pattern.
/// Files: extract current directory.
fn derive_context(title: &str, app_id: &str) -> String {
    match app_id {
        "terminal" => {
            if let Some(idx) = title.find(':') {
                let path = title[idx + 1..].trim();
                if !path.is_empty() {
                    return path.to_string();
                }
            }
            String::new()
        }
        "browser" => {
            let sep = if title.contains(" - ") {
                " - "
            } else if title.contains(" — ") {
                " — "
            } else {
                return String::new();
            };
            title.rsplit(sep).next()
                .filter(|s| !s.eq_ignore_ascii_case("firefox") && !s.eq_ignore_ascii_case("chromium"))
                .unwrap_or("")
                .to_string()
        }
        "files" => {
            if title.contains('/') {
                title.rsplit('/').next().unwrap_or("").to_string()
            } else {
                String::new()
            }
        }
        _ => String::new(),
    }
}
