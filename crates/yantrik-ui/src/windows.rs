//! Windows discovered by the compositor, merged with the shell launch registry.
//! Compositor discovery is cached so surviving windows remain available after a
//! shell restart without spawning a helper on every taskbar refresh.

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
    merge_windows(shell_windows(), wlrctl_windows())
}

/// Cache compositor discovery for nine seconds. Always merge it with our launch
/// registry: windows that survived a shell restart must stay in the taskbar when
/// a newly launched Editor adds the first entry to the fresh registry.
pub fn list_windows_throttled() -> Vec<WindowEntry> {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    static CACHE: Mutex<Option<(Instant, Vec<WindowEntry>)>> = Mutex::new(None);
    let Ok(mut cache) = CACHE.lock() else { return list_windows(); };
    if cache.as_ref().is_none_or(|(at, _)| at.elapsed() >= Duration::from_secs(9)) {
        *cache = Some((Instant::now(), wlrctl_windows()));
    }
    merge_windows(shell_windows(), cache.as_ref().unwrap().1.clone())
}

fn merge_windows(mut launched: Vec<WindowEntry>, discovered: Vec<WindowEntry>) -> Vec<WindowEntry> {
    for window in discovered {
        if !launched.iter().any(|known| known.title == window.title && known.app_id == window.app_id) {
            launched.push(window);
        }
    }
    launched
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
/// What each app this OS ships is CALLED. One list, because there were four.
///
/// The same application answered to a different name depending on which surface you were
/// looking at: the dock said "Editor", the launcher said "Text Editor", the window title said
/// "Text Editor" and the header said whatever the screen author wrote. Downloads was
/// "Download Manager" in three places and "Downloads" in a fourth. No single one of those was
/// wrong, which is exactly why it survived — it only reads as sloppy when you see two at once,
/// and you always do: the taskbar entry sits directly beneath the window it names.
///
/// Short names, because that is the family the dock already used and the dock is the surface a
/// person reads most. The suffixes went rather than being invented away: Container Manager to
/// Containers, Music Player to Music, Image Viewer to Images. The office three keep the brand
/// the dock gave them.
///
/// Keys are the shell's own app ids on the left, matching `Icons.app`, and the .desktop file
/// stems for the rest. `app_names_agree_everywhere` in the tests below reads the .desktop files
/// and each app's Window title and fails if any of them drifts from this.
pub const APP_NAMES: &[(&str, &str)] = &[
    ("browser", "Browser"),
    ("calendar", "Calendar"),
    ("containers", "Containers"),
    ("documents", "yDoc"),
    ("downloads", "Downloads"),
    ("editor", "Editor"),
    ("email", "Email"),
    ("image", "Images"),
    ("music", "Music"),
    ("network", "Network"),
    ("notes", "Notes"),
    ("presentation", "yPresent"),
    ("snippets", "Snippets"),
    ("spreadsheet", "ySheets"),
    ("sysmonitor", "System Monitor"),
    ("terminal", "Terminal"),
    ("weather", "Weather"),
];

fn display_name(app_id: &str) -> String {
    APP_NAMES
        .iter()
        .find(|(id, _)| *id == app_id)
        .map(|(_, name)| (*name).to_string())
        .unwrap_or_else(|| {
            // Unknown id (a .desktop app the shell launched): title-case its first segment.
            let mut c = app_id.replace(['-', '_'], " ");
            if let Some(first) = c.get_mut(0..1) {
                first.make_ascii_uppercase();
            }
            c
        })
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
    fn surviving_windows_remain_when_editor_is_launched() {
        let win=|id:&str,title:&str| WindowEntry {app_id:id.into(),title:title.into(),icon_char:String::new(),subtitle:String::new()};
        let merged=merge_windows(vec![win("editor","Editor")],vec![win("terminal","Terminal"),win("notes","Notes"),win("editor","Editor")]);
        assert_eq!(merged.iter().map(|w|w.app_id.as_str()).collect::<Vec<_>>(),["editor","terminal","notes"]);
    }


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

    // What used to be here: a hand-written list of every app id and the window title it was
    // expected to produce, asserting display_name() matched. It did the right job and carried
    // the wrong kind of list — a second copy of the names, maintained by hand, which went stale
    // the moment the names were settled in one place.
    //
    // `app_name_tests::app_names_agree_everywhere` is the replacement. It reads the .desktop
    // entries and each app's Window title off disk and compares them to APP_NAMES, so it checks
    // the same invariant — that the taskbar label is exactly the window title, because the
    // taskbar sends that string to `wlrctl toplevel focus title:…` and a label merely CLOSE to
    // the title is a click that does nothing — without anyone having to remember to update it.
}

#[cfg(test)]
mod app_name_tests {
    use super::{display_name, APP_NAMES};
    use std::path::{Path, PathBuf};

    /// desktop-file stem -> the shell's own app id for the same application.
    ///
    /// Two naming schemes, both correct. A freedesktop entry needs a name unique across the
    /// whole machine, so ours are `yantrik-download-manager`; the shell calls the same thing
    /// `downloads`, which is what the icon set and the dock are keyed by.
    const STEM_TO_ID: &[(&str, &str)] = &[
        ("calendar", "calendar"),
        ("container-manager", "containers"),
        ("document-editor", "documents"),
        ("download-manager", "downloads"),
        ("email", "email"),
        ("image-viewer", "image"),
        ("music-player", "music"),
        ("network-manager", "network"),
        ("notes", "notes"),
        ("presentation", "presentation"),
        ("snippet-manager", "snippets"),
        ("spreadsheet", "spreadsheet"),
        ("system-monitor", "sysmonitor"),
        ("terminal", "terminal"),
        ("text-editor", "editor"),
        ("weather", "weather"),
    ];

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("the crate sits two levels under the repository root")
    }

    fn field(text: &str, key: &str) -> Option<String> {
        text.lines()
            .find_map(|l| l.strip_prefix(key))
            .map(|v| v.trim().to_string())
    }

    /// An application answers to ONE name, on every surface that shows it.
    ///
    /// It answered to four. The dock said "Editor", the launcher said "Text Editor", the
    /// window title said "Text Editor", and the taskbar entry beneath that window said
    /// something else again. Downloads was "Download Manager" in three places and "Downloads"
    /// in a fourth. Each was defensible alone, which is why it lasted — it only reads as
    /// sloppy when two are on screen together, and the taskbar entry sits directly under the
    /// window it names, so they always are.
    #[test]
    fn app_names_agree_everywhere() {
        let root = repo_root();
        let mut wrong = Vec::new();

        for (stem, id) in STEM_TO_ID {
            let want = APP_NAMES
                .iter()
                .find(|(k, _)| k == id)
                .map(|(_, n)| *n)
                .unwrap_or_else(|| panic!("{id} is not in APP_NAMES"));

            let entry = root.join(format!("apps/desktop-files/yantrik-{stem}.desktop"));
            let text = std::fs::read_to_string(&entry)
                .unwrap_or_else(|e| panic!("cannot read {}: {e}", entry.display()));
            if let Some(name) = field(&text, "Name=") {
                if name != want {
                    wrong.push(format!("{stem}: .desktop says {name:?}, APP_NAMES says {want:?}"));
                }
            }

            let win = root.join(format!("apps/{stem}/ui/app.slint"));
            let text = std::fs::read_to_string(&win)
                .unwrap_or_else(|e| panic!("cannot read {}: {e}", win.display()));
            let title = text
                .lines()
                .find_map(|l| l.trim().strip_prefix("title: \""))
                .and_then(|r| r.split('"').next())
                .map(|s| s.to_string());
            if let Some(title) = title {
                if title != want {
                    wrong.push(format!("{stem}: window title {title:?}, APP_NAMES says {want:?}"));
                }
            }

            if display_name(id) != want {
                wrong.push(format!(
                    "{stem}: taskbar calls it {:?}, APP_NAMES says {want:?}",
                    display_name(id)
                ));
            }
        }

        assert!(
            wrong.is_empty(),
            "one application, more than one name:\n  {}\n\n\
             APP_NAMES in this file is the list. Change it there and change the .desktop entry \
             and the app's Window title to match.",
            wrong.join("\n  ")
        );
    }

    /// An app the shell did not launch still gets a readable name rather than an id.
    #[test]
    fn a_foreign_app_is_title_cased_not_left_raw() {
        assert_eq!(display_name("libreoffice-writer"), "Libreoffice writer");
        assert_eq!(display_name("chromium"), "Chromium");
    }
}
