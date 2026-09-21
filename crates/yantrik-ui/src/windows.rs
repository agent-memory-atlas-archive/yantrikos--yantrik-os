//! Windows discovered by the compositor, merged with the shell launch registry.
//! Compositor discovery is cached so surviving windows remain available after a
//! shell restart without spawning a helper on every taskbar refresh.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The title the shell's own window carries, from `title:` in yantrik-ui-slint/ui/app.slint.
///
/// Kept here so the one place that has to exclude it says why, rather than a bare string buried
/// in a filter. `pub(crate)` because `control_approvals` needs the same string for the opposite
/// reason — it asks the compositor to bring THIS window forward when an approval card goes up,
/// and has to recognise it to avoid recording the shell as the window to hand the screen back to.
pub(crate) const SHELL_WINDOW_TITLE: &str = "Yantrik OS";

/// A running window on the desktop.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowEntry {
    pub title: String,
    pub app_id: String,
    pub icon_char: String,
    pub subtitle: String,
}

/// How long one reading of the compositor's window list is trusted.
///
/// Nine seconds, which is three turns of the system poll. Long enough that the taskbar refresh
/// never spawns a process on its own cadence, short enough that a window closed by hand leaves
/// the strip while the person is still looking at it.
const COMPOSITOR_TTL: Duration = Duration::from_secs(9);

/// The last reading of `wlrctl toplevel list`, and when it was taken.
///
/// Process-wide, because every surface that asks "what is open" has to get the same answer, and
/// because the one caller that must never spawn a subprocess — `shell_windows`, which runs inside
/// the `describe` closure on the UI thread — reads it without refreshing it.
static COMPOSITOR: Mutex<Option<(Instant, Vec<WindowEntry>)>> = Mutex::new(None);

/// Read the compositor's window list now and keep it. Returns how many windows it found.
///
/// Called once at startup (see `main`) and then by the taskbar refresh. The startup call is the
/// point of this whole mechanism: see `shell_windows` below.
pub fn refresh_compositor_windows() -> usize {
    let found = wlrctl_windows();
    let count = found.len();
    if let Ok(mut cache) = COMPOSITOR.lock() {
        *cache = Some((Instant::now(), found));
    }
    count
}

/// The last reading, without taking a new one. Empty until something has refreshed it.
fn compositor_snapshot() -> Vec<WindowEntry> {
    COMPOSITOR.lock().ok().and_then(|c| c.as_ref().map(|(_, w)| w.clone())).unwrap_or_default()
}

/// Take a new reading if the one we have has aged out.
fn refresh_compositor_if_stale() {
    let stale = match COMPOSITOR.lock() {
        Ok(cache) => cache.as_ref().is_none_or(|(at, _)| at.elapsed() >= COMPOSITOR_TTL),
        Err(_) => return,
    };
    if stale {
        refresh_compositor_windows();
    }
}

/// The windows the shell has open, after taking a fresh reading of the compositor.
///
/// For the callers that are about to put the list in front of somebody — the window switcher
/// opening on a hotkey — where a list up to nine seconds stale is a list with a window in it that
/// has just been closed.
pub fn list_windows() -> Vec<WindowEntry> {
    refresh_compositor_windows();
    shell_windows()
}

/// The same list, for callers on a timer: the compositor is only asked again once the last answer
/// has aged out. The taskbar refresh runs every three seconds and must not spawn a process each
/// time it does.
pub fn list_windows_throttled() -> Vec<WindowEntry> {
    refresh_compositor_if_stale();
    shell_windows()
}

/// The launch registry, plus everything the compositor saw that the registry does not know about.
///
/// A window is the same window if the id matches, not only if the title does. The registry names
/// a window by its app id and titles it `display_name(id)`; the compositor gives back whatever
/// the window is actually called at this moment. Those agree today — `app_names_agree_everywhere`
/// holds every app's `title:` to its APP_NAMES entry — but the day one of them puts a filename in
/// its title bar, matching on the title as well would have listed the same window twice: once as
/// the shell remembers launching it and once as the compositor sees it.
///
/// Our own apps are single-instance (see `running::mark_launched`), so one id is one window.
fn merge_windows(mut launched: Vec<WindowEntry>, discovered: Vec<WindowEntry>) -> Vec<WindowEntry> {
    for window in discovered {
        if !launched.iter().any(|known| known.app_id == window.app_id) {
            launched.push(window);
        }
    }
    launched
}

/// The windows the shell has open: what it launched, plus what the compositor says is on screen.
///
/// This is what `describe shell` reports and what the dock reads its running marks from, so it
/// has to be cheap — it runs on the UI thread inside the describe closure — and correct on every
/// screen, because an app stays open when the shell navigates away from the desktop. Cheap is why
/// it reads the cached compositor snapshot rather than taking one: no subprocess on this thread.
///
/// It used to be the launch registry ALONE, on the reasoning that the shell started its own
/// children and therefore knew them better than any query could. True, and it misses the case
/// that matters: the registry is an in-process `HashMap`, so it is empty every time this process
/// starts. Restart the shell under a compositor that keeps running — which is what a crash, an
/// update or `systemctl restart` does — and four apps are still on screen while `describe shell`
/// says "0 windows open" and the dock shows none of them running. The shell only ever learned of
/// a window by watching itself create it, and it had not watched these.
///
/// The compositor did watch them, and it is the only thing in the session that outlives us. So it
/// is asked, and what it says is merged in. The old objection to asking it — that the title
/// heuristic collapsed every Yantrik window onto one id — is answered by `app_id_for_title`:
/// our windows carry titles that are exactly the `APP_NAMES` entries, an invariant
/// `app_names_agree_everywhere` already enforces against the .desktop files and the app sources,
/// so the id comes from a lookup rather than a guess. `wlrctl` being absent costs us only what it
/// cost before: the registry answer, which is what this returned in the first place.
pub fn shell_windows() -> Vec<WindowEntry> {
    let launched: Vec<WindowEntry> = crate::running::running()
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
        .collect();
    merge_windows(launched, compositor_snapshot())
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

/// Bring the window called `title` to the front, restoring it first if it was minimized. Says
/// whether the compositor had such a window.
///
/// A minimized window cannot take focus while it is still minimized, and wlrctl has no
/// "unminimize" verb — `maximize` is what brings it back onto the screen. Applied only to windows
/// that are actually minimized, so presenting a visible window does not resize it.
///
/// The key is given explicitly: wlrctl's matchspec treats a bare word as an app_id, our Slint
/// windows set a title and no app_id, and a match on nothing exits as success.
pub fn present(title: &str) -> bool {
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
        Ok(status) => status.success(),
        Err(e) => {
            tracing::warn!(error = %e, "could not run wlrctl to focus a window");
            false
        }
    }
}

/// Bring the window of one of our apps to the front, by the id the launcher knows it by.
pub fn present_app(app_id: &str) -> bool {
    present(&display_name(app_id))
}

/// Ask the compositor what is on screen, through `wlrctl toplevel list`.
///
/// The only account of a window this process did not start — an app a person launched from a
/// terminal, and, the case this exists for, an app that was open before the shell restarted.
/// Never called from the UI thread: `refresh_compositor_windows` is what runs it, and everything
/// else reads the cache it fills.
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
    // Prefer what the window calls itself. Our own windows call themselves nothing — Slint gives
    // them no Wayland app_id — so the title is matched against APP_NAMES next, which is a lookup
    // rather than a guess: those strings ARE the window titles our apps declare, and
    // `app_names_agree_everywhere` fails the build if one drifts.
    //
    // Guessing was the whole problem. `derive_app_id` takes the first word, so the System Monitor
    // window came back as `system`, Downloads as `download`, yDoc as `ydoc` and Images as
    // `images` — none of which is the id the dock keys its running mark by, so after a shell
    // restart those four tiles stayed dark with the apps plainly open on screen. Guessing is now
    // the last resort, for windows that are neither ours nor self-identifying.
    let app_id = if !declared_id.is_empty() {
        declared_id.to_lowercase()
    } else if let Some(id) = app_id_for_title(&title) {
        id.to_string()
    } else {
        derive_app_id(&title)
    };
    (title, app_id)
}

/// The app id whose window is titled exactly this, if it is one of ours.
///
/// Exactly, not loosely: "Notes" is Notes and "Notes: Handover" is a note open in it, and a
/// substring match would make the second one a second copy of the first in every window list.
fn app_id_for_title(title: &str) -> Option<&'static str> {
    APP_NAMES.iter().find(|(_, name)| *name == title).map(|(id, _)| *id)
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


    /// The case the shell used to get wrong: the launch registry is empty because this process
    /// has just started, and four apps are on screen because the compositor did not restart.
    ///
    /// `describe shell` said "0 windows open" and the dock showed nothing running. The merge is
    /// what answers it — with an empty registry the compositor's list IS the list.
    #[test]
    fn a_shell_that_has_just_started_still_sees_the_windows_already_open() {
        let restarted_into: Vec<WindowEntry> = ["notes: Notes", ": Terminal", ": Editor", "firefox: Mozilla Firefox"]
            .iter()
            .map(|line| {
                let (title, app_id) = split_toplevel_line(line);
                WindowEntry { title, app_id, icon_char: String::new(), subtitle: String::new() }
            })
            .collect();
        let merged = merge_windows(Vec::new(), restarted_into);
        assert_eq!(merged.len(), 4, "every window the compositor still holds is open");
        assert_eq!(
            merged.iter().map(|w| w.app_id.as_str()).collect::<Vec<_>>(),
            ["notes", "terminal", "editor", "firefox"]
        );
    }

    /// Every name in APP_NAMES is a window title the compositor can hand back, and it has to come
    /// back as the id the dock keys its running mark by — otherwise the app is open and its tile
    /// is dark. Five of these used to land on something else entirely.
    #[test]
    fn our_own_window_titles_resolve_to_the_id_the_dock_uses() {
        for (id, name) in APP_NAMES {
            // What labwc reports for a Slint window: no app_id, then the title.
            let (_, resolved) = split_toplevel_line(&format!(": {name}"));
            assert_eq!(&resolved, id, "the window titled {name:?} must be `{id}`");
        }
    }

    /// The five the first-word guess got wrong, named so the regression is readable.
    #[test]
    fn the_windows_the_first_word_guess_misnamed() {
        for (title, want) in [
            ("System Monitor", "sysmonitor"),
            ("Downloads", "downloads"),
            ("Images", "image"),
            ("yDoc", "documents"),
            ("yPresent", "presentation"),
        ] {
            assert_eq!(split_toplevel_line(&format!(": {title}")).1, want);
        }
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
