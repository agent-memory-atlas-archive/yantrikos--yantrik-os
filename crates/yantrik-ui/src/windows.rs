//! What windows are open.
//!
//! The shell knows its own children, because it launched them (see [`crate::running`]). That
//! registry is the source of truth here: it is reliable, needs no subprocess, and answers in the
//! same app-id vocabulary a caller uses to open things. `wlrctl toplevel list` remains only as a
//! fallback for the case the registry cannot cover — windows the shell did not start, in a
//! development session or an unusual setup — and it never runs when the shell has launched
//! something itself.

/// The title the shell's own window carries, from `title:` in yantrik-ui-slint/ui/app.slint.
///
/// Kept here so the one place that has to exclude it says why, rather than a bare string buried
/// in a filter.
const SHELL_WINDOW_TITLE: &str = "Yantrik OS";

/// A running window on the desktop.
#[derive(Clone, Debug, PartialEq)]
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

/// The window list for the taskbar's periodic refresh.
///
/// Same answer as [`list_windows`], with one difference that matters on an idle machine: the
/// `wlrctl` fallback is only consulted every few seconds, and the result is remembered in
/// between.
///
/// That fallback only runs when the launch registry is EMPTY — which is the common case, since
/// a desktop with nothing open is most of the time. The taskbar refreshes every three seconds
/// and is now drawn on every screen rather than only the desktop, so without this the shell
/// would spawn twenty subprocesses a minute, forever, to be told nothing is open. Idle cost on
/// this machine has already been fought down once, from 9.7% of a core to 2.1%, and it is not
/// worth giving back to re-ask a question whose answer has not changed.
pub fn list_windows_throttled() -> Vec<WindowEntry> {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    const FALLBACK_EVERY: Duration = Duration::from_secs(9);
    static CACHE: Mutex<Option<(Instant, Vec<WindowEntry>)>> = Mutex::new(None);

    // Anything the shell launched is known without asking anyone.
    let ours = shell_windows();
    if !ours.is_empty() {
        if let Ok(mut c) = CACHE.lock() {
            *c = None; // the fallback's answer is stale the moment we have our own
        }
        return ours;
    }

    let Ok(mut cache) = CACHE.lock() else {
        return wlrctl_windows();
    };
    if let Some((at, cached)) = cache.as_ref() {
        if at.elapsed() < FALLBACK_EVERY {
            return cached.clone();
        }
    }
    let fresh = wlrctl_windows();
    *cache = Some((Instant::now(), fresh.clone()));
    fresh
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

/// The name one of our app ids goes by on screen.
///
/// These are not free-form labels: they are the exact `title:` each app's window declares in
/// `apps/<app>/ui/app.slint`, because this same string is what the taskbar hands to
/// `wlrctl toplevel focus title:…` when the entry is clicked. Five of them used to be the app's
/// short name instead — `Downloads` for a window called "Download Manager", `Music` for "Music
/// Player" — so clicking those entries matched no window and did nothing at all, silently.
///
/// If you rename a window, rename it here. There is a test below that lists both.
fn display_name(app_id: &str) -> String {
    match app_id {
        "terminal" => "Terminal".to_string(),
        "browser" => "Browser".to_string(),
        "notes" => "Notes".to_string(),
        "email" => "Email".to_string(),
        "calendar" => "Calendar".to_string(),
        "network" => "Network Manager".to_string(),
        "sysmonitor" => "System Monitor".to_string(),
        "weather" => "Weather".to_string(),
        "music" => "Music Player".to_string(),
        "downloads" => "Download Manager".to_string(),
        "snippets" => "Snippet Manager".to_string(),
        "containers" => "Container Manager".to_string(),
        "spreadsheet" => "Spreadsheet".to_string(),
        "documents" => "Document Editor".to_string(),
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
        // Not the shell itself. `wlrctl` lists every toplevel on the compositor, and one of them
        // is always this process — so on a machine with nothing launched, the taskbar and the
        // desktop's own "open windows" list both offered to switch you to the desktop you are
        // already looking at.
        .filter(|line| split_toplevel_line(line).0 != SHELL_WINDOW_TITLE)
        .map(|line| {
            // `wlrctl toplevel list` prints `app_id: title`. Reading the whole line as the title
            // put that separator into the name, and our own windows set no wayland app_id at all,
            // so the taskbar showed every one of them as ": Terminal", ": Weather" — a stray colon
            // in front of the name, on the desktop's most-looked-at strip.
            let (title, app_id) = split_toplevel_line(line);
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

/// One `wlrctl toplevel list` line, as `(title, app_id)`.
///
/// The format is `app_id: title`. Reading the whole line as the title put that separator into the
/// name, and our own windows set no wayland app_id at all, so the taskbar showed every one of them
/// as ": Terminal", ": Weather" — a stray colon in front of the name, on the strip of the desktop
/// people look at most.
fn split_toplevel_line(line: &str) -> (String, String) {
    let (declared_id, title) = match line.split_once(':') {
        Some((id, rest)) if !rest.trim().is_empty() => (id.trim(), rest.trim()),
        // A foreign toplevel with no separator at all is all title.
        _ => ("", line.trim()),
    };
    let title = title.to_string();
    // Prefer what the window calls itself; fall back to guessing from the title, which is all
    // there is for our own windows until Slint gives them an app_id.
    let app_id = if declared_id.is_empty() {
        derive_app_id(&title)
    } else {
        declared_id.to_lowercase()
    };
    (title, app_id)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_with_no_app_id_is_named_without_the_separator() {
        // What labwc actually reports for our Slint windows, which set a title and no app_id.
        assert_eq!(split_toplevel_line(": Terminal").0, "Terminal");
        assert_eq!(split_toplevel_line(": Yantrik OS").0, "Yantrik OS");
        assert_eq!(split_toplevel_line(": Snippet Manager").0, "Snippet Manager");
    }

    #[test]
    fn the_shell_is_not_one_of_its_own_open_windows() {
        // wlrctl reports this process too. Offering to switch to the desktop, from the desktop,
        // is the kind of thing that makes a shell feel like it is not paying attention.
        assert_eq!(split_toplevel_line(": Yantrik OS").0, SHELL_WINDOW_TITLE);
    }

    #[test]
    fn a_foreign_window_keeps_the_id_it_declares() {
        let (title, app_id) = split_toplevel_line("firefox: Mozilla Firefox");
        assert_eq!(title, "Mozilla Firefox");
        assert_eq!(app_id, "firefox");
    }

    #[test]
    fn a_colon_in_the_title_itself_survives() {
        // Only the first separator divides the two fields; the rest belongs to the name.
        assert_eq!(split_toplevel_line(": Notes: Handover").0, "Notes: Handover");
        assert_eq!(split_toplevel_line("notes: Notes: Handover").0, "Notes: Handover");
    }

    #[test]
    fn a_line_with_no_separator_is_all_title() {
        assert_eq!(split_toplevel_line("Some Foreign Window").0, "Some Foreign Window");
    }

    #[test]
    fn a_taskbar_entry_is_named_exactly_what_its_window_is_called() {
        // Right-hand side copied from `title:` in apps/<app>/ui/app.slint. The taskbar sends this
        // string to `wlrctl toplevel focus title:…`, so a label that is merely *close* to the
        // window title is a click that does nothing — which is what five of these were.
        for (app_id, window_title) in [
            ("terminal", "Terminal"),
            ("notes", "Notes"),
            ("email", "Email"),
            ("calendar", "Calendar"),
            ("weather", "Weather"),
            ("spreadsheet", "Spreadsheet"),
            ("presentation", "Presentation"),
            ("sysmonitor", "System Monitor"),
            ("downloads", "Download Manager"),
            ("music", "Music Player"),
            ("snippets", "Snippet Manager"),
            ("containers", "Container Manager"),
            ("documents", "Document Editor"),
            ("network", "Network Manager"),
        ] {
            assert_eq!(
                display_name(app_id),
                window_title,
                "the taskbar would ask the compositor to focus a window by the wrong name"
            );
        }
    }
}
