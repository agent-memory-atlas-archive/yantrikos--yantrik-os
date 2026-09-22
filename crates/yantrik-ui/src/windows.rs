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

// ── Asking the compositor to move a window ──────────────────────────
//
// Everything below builds a `wlrctl toplevel …` command line and runs it. The shell owns none of
// this: labwc decides what is in front, what is minimized and what closes, and the only thing we
// can do is ask. So every function here reports whether the asking worked rather than assuming it.

/// How long a caller waits for `wlrctl` before answering without it.
///
/// Action handlers run on the UI thread — that is the only thread allowed to touch a Slint window
/// — and `wlrctl` is a process, so a wait here is a frozen desktop for as long as it lasts. On the
/// test machine `wlrctl toplevel list` answers in about two milliseconds; 1.2 seconds is far
/// outside that and still well inside the three seconds the control surface gives an action.
const COMPOSITOR_REPLY_LIMIT: Duration = Duration::from_millis(1200);

/// The `wlrctl toplevel <verb>` command line for one window, named by title.
///
/// Two things about wlrctl's matchspec, both learned the hard way and both load-bearing:
///
/// The key is spelled out, because a bare word "is assumed to be an app_id". Our Slint windows
/// declare a title and no wayland app_id at all, and a match on nothing exits as SUCCESS — which
/// is how every click on a taskbar entry used to do nothing at all, silently.
///
/// The title has to be the window's title EXACTLY. `title:` is an exact, case-sensitive
/// comparison in wlrctl 0.2.2: `wlrctl toplevel find title:editor` misses a window called
/// `Editor`, and `title:Yantrik` misses `Yantrik OS`. So every caller resolves what a person
/// typed against the open windows first (see [`window_named`]) and passes the title it found,
/// never the one it was given.
fn toplevel_args(verb: &str, title: &str) -> Vec<String> {
    vec!["toplevel".to_string(), verb.to_string(), format!("title:{title}")]
}

/// The command line that brings a MINIMIZED window back onto the screen.
///
/// A minimized window cannot take focus while it is still minimized, and wlrctl has no
/// "unminimize" verb — `maximize` is what brings it back. `state:minimized` narrows it to windows
/// that are actually minimized, so presenting a visible window does not resize it.
fn restore_args(title: &str) -> Vec<String> {
    let mut args = toplevel_args("maximize", title);
    args.push("state:minimized".to_string());
    args
}

/// Run one `wlrctl` command and say, in words a caller can act on, what happened.
///
/// A non-zero exit means the compositor matched no window, which is the interesting failure: it
/// says the shell and the compositor disagree about what is open. A missing binary is a different
/// answer and gets a different sentence, because nothing on the machine will fix itself.
fn run_wlrctl(args: &[String]) -> Result<(), String> {
    match std::process::Command::new("wlrctl").args(args).status() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!(
            "the compositor matched no window: `wlrctl {}` exited {}",
            args.join(" "),
            status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "on a signal".to_string())
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(
            "wlrctl is not installed on this machine, so nothing here can move a window"
                .to_string(),
        ),
        Err(e) => Err(format!("could not run wlrctl: {e}")),
    }
}

/// Ask the compositor for something from the UI thread, and wait a moment for the answer.
///
/// The work goes to a worker because `wlrctl` is a process; the wait is bounded because the caller
/// is the thread that paints the desktop. A timeout comes back as a timeout and not as success —
/// a caller told "done" by a shell that does not know is exactly the complaint this fixes.
fn ask_compositor(args: Vec<String>) -> Result<(), String> {
    let spelled = format!("wlrctl {}", args.join(" "));
    let (answer, wait) = std::sync::mpsc::channel();
    if std::thread::Builder::new()
        .name("yos-wlrctl".to_string())
        .spawn(move || {
            let _ = answer.send(run_wlrctl(&args));
        })
        .is_err()
    {
        return Err("could not start a thread to talk to the compositor".to_string());
    }
    match wait.recv_timeout(COMPOSITOR_REPLY_LIMIT) {
        Ok(result) => result,
        Err(_) => Err(format!(
            "`{spelled}` had not answered after {} ms, so what the compositor did with it is \
             not known here",
            COMPOSITOR_REPLY_LIMIT.as_millis()
        )),
    }
}

/// Bring the window called `title` to the front, restoring it first if it was minimized. Says
/// whether the compositor had such a window.
pub fn present(title: &str) -> bool {
    if let Err(why) = run_wlrctl(&restore_args(title)) {
        // Not a warning: the usual reason is that the window was never minimized, and the
        // matchspec simply matched nothing.
        tracing::debug!(window = %title, reason = %why, "nothing to un-minimize before focusing");
    }
    match run_wlrctl(&toplevel_args("focus", title)) {
        Ok(()) => true,
        Err(why) => {
            tracing::warn!(window = %title, reason = %why, "could not bring a window forward");
            false
        }
    }
}

/// Bring the window of one of our apps to the front, by the id the launcher knows it by.
pub fn present_app(app_id: &str) -> bool {
    present(&display_name(app_id))
}

/// Bring the shell's own window to the front. `Err` says why it is still behind something.
///
/// The shell is an ordinary toplevel to labwc — see the `<margin>` note in config/labwc/rc.xml,
/// which exists because the status bar and dock are a fullscreen window rather than a layer-shell
/// panel — so it gets in front the same way any other window does, and it cannot raise itself
/// under Wayland. Every surface that puts something in front of a person needs this:
/// `control_approvals` already asks for it when a card goes up, `open_lens` when the ask bar
/// opens, and `show_screen` because a screen nobody can see has not been shown.
pub fn raise_shell() -> Result<(), String> {
    ask_compositor(toplevel_args("focus", SHELL_WINDOW_TITLE))
}

/// Ask the window called `title` to close, the way pressing its × does.
///
/// The compositor's close request, deliberately, and not a signal to a process: an app holding
/// unsaved work is entitled to put up its own dialog and stay open, and the shell has no standing
/// to overrule it. So `Ok` here means the request was delivered, never that the window went.
pub fn close(title: &str) -> Result<(), String> {
    ask_compositor(toplevel_args("close", title))
}

/// Put the window called `title` out of the way, leaving it running.
///
/// wlrctl spells the verb the American way; this desktop's own surface does not, which is why the
/// two spellings meet here rather than anywhere a caller can see.
pub fn minimise(title: &str) -> Result<(), String> {
    ask_compositor(toplevel_args("minimize", title))
}

// ── Which window a person meant ─────────────────────────────────────

/// Every window a caller may name, the shell's own included.
///
/// [`shell_windows`] leaves the shell out on purpose: the taskbar must not offer to switch you to
/// the desktop you are already looking at. A control surface is the opposite case — the shell's
/// window is the one thing on this machine that a mind cannot reach any other way, and
/// `focus_window title=Yantrik` answering "no open window matches" while the desktop was plainly
/// running is how that omission was found.
pub fn addressable_titles() -> Vec<String> {
    let mut titles: Vec<String> = shell_windows().into_iter().map(|w| w.title).collect();
    if !titles.iter().any(|t| t == SHELL_WINDOW_TITLE) {
        titles.push(SHELL_WINDOW_TITLE.to_string());
    }
    titles
}

/// The open windows that answer to `want`, best first.
///
/// An exact title wins outright and alone, because "Notes" is Notes even while "Notes: Handover"
/// is open. Failing that it is a substring, which is what a person typing part of a title means.
///
/// The shell comes last among the loose matches, and only among them. It is the one window that is
/// always addressable and never in the window list, so it must not shadow something the person can
/// actually see — but with nothing else matching, `Yantrik` has to reach the desktop, which is the
/// whole reason it is in the candidate list.
fn matching_titles(want: &str, open: &[String]) -> Vec<String> {
    let want = want.trim().to_lowercase();
    if want.is_empty() {
        return Vec::new();
    }
    if let Some(exact) = open.iter().find(|title| title.to_lowercase() == want) {
        return vec![exact.clone()];
    }
    let shell_matches = SHELL_WINDOW_TITLE.to_lowercase().contains(&want)
        && open.iter().any(|title| title == SHELL_WINDOW_TITLE);
    let mut found: Vec<String> = open
        .iter()
        .filter(|title| *title != SHELL_WINDOW_TITLE && title.to_lowercase().contains(&want))
        .cloned()
        .collect();
    if shell_matches {
        found.push(SHELL_WINDOW_TITLE.to_string());
    }
    found
}

/// The refusal when nothing open answers to that name.
///
/// One sentence for all three verbs: a caller being told what is open should not have to learn it
/// twice in two different wordings.
fn nothing_matches(want: &str, open: &[String]) -> String {
    format!("no open window matches `{}`; there is: {}", want.trim(), open.join(", "))
}

/// The window a caller means, or a refusal that names what is open instead.
///
/// For the verbs a person can undo by hand — focus, minimise. The first match is good enough for
/// those: guessing wrong shows itself immediately and costs one more call to put right.
pub fn window_named(want: &str, open: &[String]) -> Result<String, String> {
    matching_titles(want, open)
        .into_iter()
        .next()
        .ok_or_else(|| nothing_matches(want, open))
}

/// The window to close, or a refusal saying why nothing was closed.
///
/// Stricter than [`window_named`] on two counts, because closing is the one verb here that a
/// person cannot undo by clicking something.
///
/// An ambiguous title is refused rather than resolved to whichever window came back first: with
/// two Chromium windows open, `close_window title=chromium` picking one of them is a coin toss
/// with somebody's tab in it.
///
/// And the desktop itself is refused. `Yantrik OS` is the shell — the status bar, the dock, the
/// Lens and every screen — so closing it ends the session, which is not what anyone asking to
/// close a window means. Stepping away has `lock`.
pub fn window_to_close(want: &str, open: &[String]) -> Result<String, String> {
    let found = matching_titles(want, open);
    let title = match found.len() {
        0 => return Err(nothing_matches(want, open)),
        1 => found[0].clone(),
        _ => {
            return Err(format!(
                "`{}` matches {} open windows and closing the wrong one cannot be undone; \
                 say which: {}",
                want.trim(),
                found.len(),
                found.join(", ")
            ))
        }
    };
    if title == SHELL_WINDOW_TITLE {
        return Err(format!(
            "`{SHELL_WINDOW_TITLE}` is the desktop itself — the status bar, the dock and every \
             screen — so closing it ends the session rather than a window. Use `lock` to step \
             away, or name one of the app windows"
        ));
    }
    Ok(title)
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

    /// The exact command lines the shell hands wlrctl, because every one of them has been wrong
    /// at some point and each was wrong in silence.
    ///
    /// `title:` is not decoration. Without the key, wlrctl reads the word as an app_id; our Slint
    /// windows declare no app_id; a match on nothing exits zero. That is how the taskbar came to
    /// do nothing when clicked, for every window, without a line in the log.
    #[test]
    fn the_command_lines_name_the_window_by_title() {
        assert_eq!(
            toplevel_args("focus", "Editor"),
            ["toplevel", "focus", "title:Editor"]
        );
        assert_eq!(
            toplevel_args("close", "Notes: Handover"),
            ["toplevel", "close", "title:Notes: Handover"],
            "a colon in the title belongs to the title; wlrctl splits the matchspec on the first"
        );
        assert_eq!(
            restore_args("System Monitor"),
            ["toplevel", "maximize", "title:System Monitor", "state:minimized"],
            "only windows that ARE minimized, or presenting a visible window resizes it"
        );
        // One argument each, so a title with a space travels as a title with a space. There is no
        // shell between us and wlrctl and there must not be a quoting scheme pretending there is.
        assert_eq!(toplevel_args("focus", "Yantrik OS").len(), 3);
    }

    /// wlrctl spells it `minimize`. This desktop's action is `minimise_window`, and the two
    /// spellings are allowed to meet in exactly one place — here.
    #[test]
    fn minimise_asks_the_compositor_to_minimize() {
        assert_eq!(
            toplevel_args("minimize", "Calendar"),
            ["toplevel", "minimize", "title:Calendar"]
        );
    }

    /// The shell asks for itself by the title its own window carries, and by nothing else:
    /// `title:` is an exact, case-sensitive comparison in wlrctl, so `title:Yantrik` matches
    /// no window on a machine whose shell is called `Yantrik OS`.
    #[test]
    fn the_shell_asks_for_itself_by_its_whole_title() {
        assert_eq!(
            toplevel_args("focus", SHELL_WINDOW_TITLE),
            ["toplevel", "focus", "title:Yantrik OS"]
        );
    }

    fn open(titles: &[&str]) -> Vec<String> {
        titles.iter().map(|t| (*t).to_string()).collect()
    }

    /// The desktop was reachable by no name at all.
    ///
    /// `focus_window title=Yantrik` answered "no open window matches `yantrik`" on a machine with
    /// the shell plainly running, because the window list leaves the shell out — deliberately, so
    /// the taskbar does not offer to switch you to the desktop you are on — and the control
    /// surface read that same list. The shell is a window; a caller has to be able to name it.
    #[test]
    fn the_shell_answers_to_its_own_name() {
        let desktop = open(&["Editor", "Terminal", "Notes", SHELL_WINDOW_TITLE]);
        assert_eq!(window_named("Yantrik", &desktop).unwrap(), SHELL_WINDOW_TITLE);
        assert_eq!(window_named("yantrik", &desktop).unwrap(), SHELL_WINDOW_TITLE);
        assert_eq!(window_named("Yantrik OS", &desktop).unwrap(), SHELL_WINDOW_TITLE);
        assert_eq!(window_named("  yantrik os  ", &desktop).unwrap(), SHELL_WINDOW_TITLE);
    }

    /// The desktop is always one of the windows a caller can name.
    ///
    /// `shell_windows` — what the taskbar and `describe shell` read — leaves the shell out, and
    /// that is right for both of them. The control surface read the same list, which is how the
    /// one window that is always open became the one window nothing could ask for.
    #[test]
    fn the_desktop_is_always_addressable_even_with_nothing_else_open() {
        let open = addressable_titles();
        assert!(
            open.iter().any(|t| t == SHELL_WINDOW_TITLE),
            "the shell's own window has to be nameable: {open:?}"
        );
        assert_eq!(window_named("Yantrik", &open).unwrap(), SHELL_WINDOW_TITLE);
    }

    /// An exact title wins outright, even while a longer title contains it.
    #[test]
    fn an_exact_title_beats_a_window_that_merely_contains_it() {
        let desktop = open(&["Notes: Handover", "Notes", SHELL_WINDOW_TITLE]);
        assert_eq!(window_named("notes", &desktop).unwrap(), "Notes");
        assert_eq!(window_named("handover", &desktop).unwrap(), "Notes: Handover");
    }

    /// The desktop does not shadow a window the person can see.
    ///
    /// A terminal showing `yantrik@home: ~` contains "yantrik"; so does the shell. The one on
    /// screen is the one meant, and the shell is still there when nothing else matches.
    #[test]
    fn the_shell_comes_last_among_the_loose_matches() {
        let desktop = open(&["yantrik@home: ~", SHELL_WINDOW_TITLE]);
        assert_eq!(window_named("yantrik", &desktop).unwrap(), "yantrik@home: ~");
        assert_eq!(window_named("OS", &desktop).unwrap(), SHELL_WINDOW_TITLE);
    }

    /// A refusal says what IS open, so the next call can be right.
    #[test]
    fn naming_nothing_open_says_what_is() {
        let desktop = open(&["Editor", SHELL_WINDOW_TITLE]);
        let err = window_named("gimp", &desktop).unwrap_err();
        assert!(err.contains("no open window matches `gimp`"), "{err}");
        assert!(err.contains("Editor"), "{err}");
        assert!(err.contains(SHELL_WINDOW_TITLE), "the desktop is one of the windows: {err}");
        assert!(window_named("   ", &desktop).is_err(), "an empty title matches nothing");
    }

    /// Closing is the one verb here nobody can undo with a click, so it refuses to guess.
    #[test]
    fn closing_an_ambiguous_title_is_refused_rather_than_guessed() {
        let desktop = open(&[
            "Northwind Cloud - Pricing - Chromium",
            "Ask | Hacker News - Chromium",
            SHELL_WINDOW_TITLE,
        ]);
        let err = window_to_close("chromium", &desktop).unwrap_err();
        assert!(err.contains("matches 2 open windows"), "{err}");
        assert!(err.contains("Hacker News"), "the refusal names them: {err}");
        // Naming one of them exactly still works.
        assert_eq!(
            window_to_close("Ask | Hacker News - Chromium", &desktop).unwrap(),
            "Ask | Hacker News - Chromium"
        );
        // And the same ambiguity is fine for a verb that can be undone.
        assert!(window_named("chromium", &desktop).is_ok());
    }

    /// Closing the desktop is not closing a window.
    #[test]
    fn the_desktop_itself_is_not_closable() {
        let desktop = open(&["Editor", SHELL_WINDOW_TITLE]);
        for asked in ["Yantrik", "Yantrik OS", "yantrik os"] {
            let err = window_to_close(asked, &desktop)
                .expect_err("closing the shell must be refused");
            assert!(err.contains("is the desktop itself"), "{err}");
            assert!(err.contains("lock"), "the refusal offers what was probably meant: {err}");
        }
        assert_eq!(window_to_close("Editor", &desktop).unwrap(), "Editor");
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
