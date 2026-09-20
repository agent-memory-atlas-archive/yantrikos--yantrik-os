//! Dock wiring — on_launch_app callback, and the one answer to "can this app open here?".

use std::path::{Path, PathBuf};

use slint::ComponentHandle;

use crate::app_context::AppContext;
use crate::apps::DesktopEntry;
use crate::App;

/// What opening one of the shell's own apps does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Launch {
    /// One of the shell's own screens. Compiled into the shell, so always there.
    Screen(i32),
    /// Settings, opened at one of its sections.
    SettingsSection(i32),
    /// The editor screen, on a blank file.
    Editor,
    /// The Apps launcher.
    Launchpad,
    /// A program shipped beside the shell, registered as `id` while it runs.
    Program { id: &'static str, bin: &'static str },
    /// Whichever web browser this machine has.
    Browser,
}

/// Every app the shell opens by name, and what opening it does.
///
/// The first name in each row is the one the app is listed by; the rest are the spellings the
/// same app arrives under — the name its binary carries, the id it publishes on its control
/// surface — after `canonical_id` has folded their punctuation.
///
/// This table IS the dispatch. There used to be a match in `wire()` and, beside it, a list of the
/// ids that match accepted, with a comment asking the two to agree. They did not: ten arms were
/// missing from the list, and the launcher's About and Skills tiles had no arm at all, so
/// clicking them logged "Unknown app" while `open_app` answered "launching". One table cannot
/// disagree with itself.
///
/// Three rows carry history worth keeping:
///
/// - `text_editor` is the name the binary carries and the name a person would try; `editor` is
///   what the arm was always called. Both open the native Text Editor, which forwards repeated file opens to its existing window.
/// - The image viewer was routed to screen 11 because the standalone binary, although shipped,
///   could not open a picture: no argument handling, no control surface, and navigation
///   callbacks that only logged. Routing around it kept the shell screen working and left
///   24 MB in /opt/yantrik/bin that nothing could reach. The app opens files now, so the route
///   is the app, and the file browser hands it the path.
/// - `network_manager` was accepted by the guard — a .desktop entry matched — and then reached
///   no arm, so open_app answered "launching" and nothing happened.
const ROUTES: &[(&[&str], Launch)] = &[
    (&["terminal"], Launch::Program { id: "terminal", bin: "yantrik-terminal" }),
    (&["browser"], Launch::Browser),
    (&["files"], Launch::Screen(8)),
    (&["settings"], Launch::Screen(7)),
    (&["notes"], Launch::Program { id: "notes", bin: "yantrik-notes" }),
    (&["editor", "text_editor"], Launch::Program { id: "editor", bin: "yantrik-text-editor" }),
    (&["image_viewer", "images"], Launch::Program { id: "images", bin: "yantrik-image-viewer" }),
    (&["bond"], Launch::Screen(4)),
    (&["personality"], Launch::Screen(5)),
    (&["memory"], Launch::Screen(6)),
    (&["notifications"], Launch::Screen(9)),
    (&["system"], Launch::Screen(10)),
    (&["media"], Launch::Screen(13)),
    (&["about"], Launch::Screen(16)),
    // "Install companion skills" is a section of Settings, not a screen of its own.
    (&["skills"], Launch::SettingsSection(7)),
    (&["email"], Launch::Program { id: "email", bin: "yantrik-email" }),
    (&["calendar"], Launch::Program { id: "calendar", bin: "yantrik-calendar" }),
    (&["packages"], Launch::Screen(21)),
    (&["network", "network_manager"], Launch::Program { id: "network", bin: "yantrik-network-manager" }),
    (&["sysmonitor", "system_monitor"], Launch::Program { id: "sysmonitor", bin: "yantrik-system-monitor" }),
    (&["weather"], Launch::Program { id: "weather", bin: "yantrik-weather" }),
    (&["downloads", "download_manager"], Launch::Program { id: "downloads", bin: "yantrik-download-manager" }),
    (&["snippets", "snippet_manager"], Launch::Program { id: "snippets", bin: "yantrik-snippet-manager" }),
    (&["containers", "container_manager"], Launch::Program { id: "containers", bin: "yantrik-container-manager" }),
    (&["devices", "device_dashboard"], Launch::Screen(27)),
    (&["permissions", "permission_dashboard"], Launch::Screen(28)),
    (&["documents", "document_editor"], Launch::Program { id: "documents", bin: "yantrik-document-editor" }),
    (&["presentation", "slides"], Launch::Program { id: "presentation", bin: "yantrik-presentation" }),
    (&["launchpad"], Launch::Launchpad),
];

/// An app that is in this tree and not in this build.
///
/// `reason` is a sentence a person reads; `returns_when` is what somebody would have to build.
/// Both are quoted verbatim to whoever asked to open the app, so they are written to be read by
/// a person or by a mind that has just been refused and needs to know whether to try something
/// else or to go and write the missing half.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shelved {
    /// Every canonical spelling the app arrives under, `canonical_id`-folded.
    pub ids: &'static [&'static str],
    /// What the app is called on screen, from `windows::APP_NAMES`.
    pub name: &'static str,
    /// The binary it would have run. A .desktop entry naming this is dropped from the catalogue.
    pub binary: &'static str,
    /// Why it is not in this build.
    pub reason: &'static str,
    /// The minimum real thing that would put it back.
    pub returns_when: &'static str,
}

/// The apps that are not shipped, and what each is waiting on.
///
/// The rule this table enforces: an app with nothing under its screen is not shipped. Music and
/// ySheets were both complete drawings over nothing — Music has no playback engine, no scanner
/// and no library, so only `YANTRIK_MUSIC_DEMO=1` could ever put a song on screen; ySheets never
/// sets `cell-grid`, `row-count` or `col-count`, so its 50x26 grid has no cells and every guard
/// in cell-click, cell-edit and the formula bar fails. The decision and its reasoning are in
/// design/apps-plan-2026-09-20.md, Wave 3: build it or take it off the shelf. Shipping the
/// drawing is the option that plan rules out.
///
/// Shelved is not deleted. Both crates stay workspace members so they keep compiling and cannot
/// rot in silence, both keep their entry in the lints' debt, and the `.desktop` files stay in the
/// tree — they are simply not packaged, not routed, and not offered to anybody.
///
/// This table is the whole of the shelf. Routes, the catalogue filter, the Lens and the launch
/// refusal all consult it rather than each carrying their own list of names to omit, because a
/// shelf spread over seven files is one a future contributor undoes one line at a time without
/// ever deciding to. Un-shelving an app is deleting one entry here and putting its row back in
/// ROUTES.
const SHELVED: &[Shelved] = &[
    Shelved {
        ids: &["music", "music_player"],
        name: "Music",
        binary: "yantrik-music-player",
        reason: "nothing plays audio yet — there is no playback engine, no scanner and no \
                 library behind the screen",
        returns_when: "mpv is driven over its JSON IPC socket, a folder scan fills a small \
                       library store, and play/pause/next/queue and `open <file>` work",
    },
    Shelved {
        ids: &["spreadsheet", "ysheets"],
        name: "ySheets",
        binary: "yantrik-spreadsheet",
        reason: "there is no cell model behind the grid, so nothing can be typed into it, by \
                 mouse or by mind",
        returns_when: "a cell model, CSV load and save, and arithmetic with references plus \
                       SUM/AVG/MIN/MAX/COUNT are there",
    },
];

/// The shelved app a name refers to, matched the way the dispatch matches a route.
///
/// Every spelling has to reach it, because a refusal that only fires for one of them is not a
/// refusal: a caller reads `music-player` off the binary, `Music Player` off the window title and
/// `music` off the dock, and the launcher used to treat those as three different questions.
pub fn shelved(app: &str) -> Option<&'static Shelved> {
    let id = canonical_id(app);
    // The catalogue names this OS's own apps by their .desktop filename, so `yantrik_music_player`
    // arrives here the same way `music` does.
    let id = id.strip_prefix("yantrik_").unwrap_or(&id);
    SHELVED.iter().find(|s| s.ids.contains(&id))
}

/// The shelved app an Exec line would run, if any.
///
/// This is what keeps the shelf honest on a machine that already has the binary and its .desktop
/// file on disk from an earlier release. Nothing removes those on update, so the catalogue will
/// keep finding the entry; matching on the program it would run means the tile never comes back.
pub fn shelved_exec(exec: &str) -> Option<&'static Shelved> {
    let bin = exec.split_whitespace().next()?;
    let name = bin.rsplit('/').next()?;
    SHELVED.iter().find(|s| s.binary == name)
}

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

/// What opening `app` does, if it is one of the shell's own.
pub fn route(app: &str) -> Option<Launch> {
    let id = canonical_id(app);
    ROUTES.iter().find(|(names, _)| names.contains(&id.as_str())).map(|(_, launch)| *launch)
}

/// Every name the shell's own apps answer to.
pub fn builtin_app_ids() -> impl Iterator<Item = &'static str> {
    ROUTES.iter().flat_map(|(names, _)| names.iter().copied())
}

/// The .desktop entry a name refers to, matched the way the dispatch matches it.
fn catalogue_entry<'a>(app: &str, installed: &'a [DesktopEntry]) -> Option<&'a DesktopEntry> {
    let lower = app.trim().to_lowercase();
    installed.iter().find(|e| e.app_id == app || e.name.to_lowercase() == lower)
}

/// Whether an app can be opened on this machine, as it is right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    /// Opening it opens it.
    Ready,
    /// The shell knows the app, but what it runs is not on this machine. Carries what is missing.
    Missing(String),
    /// The app is in the tree and not in this build. Carries why, and what would bring it back.
    Shelved(&'static Shelved),
    /// Nothing answers to that name.
    Unknown,
}

/// Whether opening `app` will actually open something.
///
/// "Known" was the only question anybody asked, and it is the wrong one. The Browser pin was
/// known — it had an arm — and the arm ran `chromium`, which the installer does not put on the
/// disk. So START showed Browser, `open_app browser` answered "launching", and a click did
/// nothing but log ENOENT. Anything that lists an app or promises to open one asks this instead,
/// which checks the same two sources the dispatch uses, in the same order, down to whether the
/// program they would run exists.
pub fn availability(app: &str, installed: &[DesktopEntry]) -> Availability {
    // Asked first, and before the catalogue, so a stale .desktop file and a stale binary left on
    // disk by an earlier release cannot answer Ready for something this build does not ship.
    if let Some(shelf) = shelved(app) {
        return Availability::Shelved(shelf);
    }
    if let Some(entry) = catalogue_entry(app, installed) {
        if let Some(shelf) = shelved_exec(&entry.exec) {
            return Availability::Shelved(shelf);
        }
        if entry.exec != "__builtin__" {
            return program_availability(&entry.exec);
        }
        // A built-in catalogue entry is launched by its route, like the dispatch does.
    }
    match route(app) {
        None => Availability::Unknown,
        Some(Launch::Program { bin, .. }) => match find_program(bin) {
            Some(_) => Availability::Ready,
            None => Availability::Missing(bin.to_string()),
        },
        Some(Launch::Browser) => match find_browser() {
            Some(_) => Availability::Ready,
            None => Availability::Missing(format!(
                "a web browser (looked for {})",
                BROWSERS.iter().map(|(name, _)| *name).collect::<Vec<_>>().join(", ")
            )),
        },
        Some(Launch::Screen(_) | Launch::SettingsSection(_) | Launch::Editor | Launch::Launchpad) => {
            Availability::Ready
        }
    }
}

/// Whether the program an Exec line runs is on this machine.
fn program_availability(exec: &str) -> Availability {
    let Some(bin) = exec.split_whitespace().next() else {
        return Availability::Missing("a program to run".to_string());
    };
    match find_program(bin) {
        Some(_) => Availability::Ready,
        None => Availability::Missing(bin.to_string()),
    }
}

/// Whether the launch_app dispatch will do anything with this id.
pub fn is_known_app(app: &str, installed: &[DesktopEntry]) -> bool {
    availability(app, installed) != Availability::Unknown
}

/// Whether opening this app will open it.
pub fn is_launchable(app: &str, installed: &[DesktopEntry]) -> bool {
    availability(app, installed) == Availability::Ready
}

/// Whether a catalogue entry belongs in the launcher at all.
///
/// A tile is a promise that clicking it opens something. A .desktop file can outlive its package,
/// and a built-in tile can name a screen nothing routes to — both were in the launcher, and both
/// did nothing when clicked.
///
/// A shelved app is the third case, and the only one where the program on the disk works
/// perfectly well: an installed machine keeps `/opt/yantrik/bin/yantrik-music-player` and its
/// .desktop entry from whatever release put them there, because the updater installs binaries
/// over binaries and never removes one the new bundle does not carry. Matching the shelf by the
/// program the Exec line runs is what keeps that stale pair out of the launcher.
pub fn entry_is_launchable(entry: &DesktopEntry) -> bool {
    if shelved(&entry.app_id).is_some() || shelved_exec(&entry.exec).is_some() {
        return false;
    }
    if entry.exec == "__builtin__" {
        return route(&entry.app_id).is_some();
    }
    program_availability(&entry.exec) == Availability::Ready
}

/// The names of the apps that will open on this machine, for telling a caller what it can ask for.
pub fn launchable_app_ids(installed: &[DesktopEntry]) -> Vec<&'static str> {
    ROUTES
        .iter()
        .map(|(names, _)| names[0])
        .filter(|id| is_launchable(id, installed))
        .collect()
}

/// The web browsers the Browser pin will open, in order of preference, with what each needs.
///
/// Chromium first: it is what this OS installs, and the browser tools drive it. The flags keep
/// what the old hardcoded launch had — native Wayland, no first-run wizard, no default-browser
/// nag, and no GPU, which a VM does not have. The rest open as they are.
///
/// The profile is the browser's own. The visible browser used to run from
/// `--user-data-dir=/tmp/chromium-visible`, because a headless instance might hold the default
/// profile's lock; the browser tools now keep every profile under ~/.local/share/yantrik/browsers,
/// so that reason is gone, and a /tmp profile forgot every login and bookmark at each reboot.
const BROWSERS: &[(&str, &[&str])] = &[
    ("chromium", CHROMIUM_FLAGS),
    ("chromium-browser", CHROMIUM_FLAGS),
    ("google-chrome-stable", CHROMIUM_FLAGS),
    ("google-chrome", CHROMIUM_FLAGS),
    ("firefox", &[]),
    ("firefox-esr", &[]),
    ("epiphany-browser", &[]),
    ("epiphany", &[]),
];

const CHROMIUM_FLAGS: &[&str] = &[
    "--ozone-platform=wayland",
    "--no-first-run",
    "--no-default-browser-check",
    "--disable-gpu",
];

/// The first browser from `BROWSERS` this machine has.
pub fn find_browser() -> Option<(&'static str, &'static [&'static str])> {
    BROWSERS.iter().copied().find(|(name, _)| find_program(name).is_some())
}

/// Where a program is, if it is anywhere it could be run from.
///
/// The shell is started from `/opt/yantrik/bin` (or a cargo target dir in development), and the
/// apps are deployed beside it — but nothing puts that directory on PATH, so a bare
/// `Command::new("yantrik-notes")` fails with ENOENT on a clean install. Prefer the shell's own
/// directory, then the deploy path, then PATH.
pub fn find_program(bin: &str) -> Option<PathBuf> {
    if bin.is_empty() {
        return None;
    }
    if bin.contains('/') {
        let path = PathBuf::from(bin);
        return is_executable(&path).then_some(path);
    }
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(dir) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)) {
        dirs.push(dir);
    }
    dirs.push(PathBuf::from("/opt/yantrik/bin"));
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    dirs.into_iter().map(|dir| dir.join(bin)).find(|p| is_executable(p))
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata().map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Wire on_launch_app callback.
pub fn wire(ui: &App, ctx: &AppContext) {
    let catalogue = ctx.installed_apps.clone();
    let ui_weak = ui.as_weak();

    ui.on_launch_app(move |app_id| {
        let app = app_id.to_string();
        tracing::info!(app = %app, "Launching app");

        // The shelf, before anything that could run a program. `check_launchable` already
        // refuses `open_app`, but this callback is also reached by a tile, a pin and by
        // `invoke_launch_app` from anywhere in the shell, and the binary is still on the disk of
        // every machine that installed an earlier release — so the last gate before spawn says no
        // as well, and says why.
        if let Some(shelf) = shelved(&app) {
            tracing::warn!(
                app = %app,
                "{} is not part of this build: {}. It comes back when {}.",
                shelf.name, shelf.reason, shelf.returns_when
            );
            return;
        }

        // Installed .desktop apps first; a built-in entry falls through to its route.
        //
        // A pin or the Lens can name an app ("notes") that ALSO has a .desktop entry (Name=Notes);
        // that entry matches first, so it must launch exactly like the routes below do — same
        // resolution, same environment scrubbing.
        let installed = catalogue.get();
        if let Some(entry) = catalogue_entry(&app, &installed) {
            if let Some(shelf) = shelved_exec(&entry.exec) {
                tracing::warn!(
                    app = %app, exec = %entry.exec,
                    "{} is not part of this build: {}", shelf.name, shelf.reason
                );
                return;
            }
            if entry.exec != "__builtin__" {
                let parts: Vec<&str> = entry.exec.split_whitespace().collect();
                if let Some((bin, args)) = parts.split_first() {
                    let id = super::app_grid::icon_id_for(&entry.app_id);
                    spawn_app_with_args(&id, bin, args);
                }
                return;
            }
        }

        // Matched on the canonical spelling, so an id taken from an app's control surface reaches
        // the same route as the dock's own. The .desktop lookup above deliberately still uses the
        // raw string: those entries carry real ids and names, and folding their punctuation would
        // be guessing at somebody else's vocabulary rather than settling our own.
        let Some(launch) = route(&app) else {
            tracing::warn!(app = %app, "Unknown app");
            return;
        };
        let show = |screen: i32| {
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_current_screen(screen);
                ui.invoke_navigate(screen);
            }
        };
        match launch {
            Launch::Screen(screen) => show(screen),
            Launch::SettingsSection(section) => {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_settings_category(section);
                }
                show(7);
            }
            Launch::Editor => spawn_app("editor", "yantrik-text-editor"),
            Launch::Launchpad => {
                show(1);
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_app_grid_open(true);
                }
            }
            Launch::Program { id, bin } => spawn_app(id, bin),
            // A browser is a window like any other, so it goes through the one launcher: the
            // registry learns it is open, the reaper notices if it dies at once, and it gets the
            // session's display environment. The old arm set WAYLAND_DISPLAY and XDG_RUNTIME_DIR
            // by hand, for uid 1000 only.
            Launch::Browser => match find_browser() {
                Some((bin, args)) => spawn_app_with_args("browser", bin, args),
                None => tracing::error!(
                    "Cannot open the browser: none is installed (looked for {})",
                    BROWSERS.iter().map(|(name, _)| *name).collect::<Vec<_>>().join(", ")
                ),
            },
        }
    });
}

/// Where an app binary lives, or the bare name for `Command` to look up if it is nowhere.
pub fn resolve_app_binary(bin: &str) -> PathBuf {
    find_program(bin).unwrap_or_else(|| PathBuf::from(bin))
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
    spawn_app_in(app_id, bin, args, None)
}

/// The same launcher, started in a particular directory.
///
/// For "open a terminal here", where the directory IS the request. Goes through one body with
/// `spawn_app_with_args` so the registry, the environment scrubbing and the reaper cannot end up
/// applying to one launch path and not the other — which is how the dock grew two of them before.
/// The display environment a launched app needs, which the shell itself does not have.
///
/// This is the whole of why third-party software did not run.
///
/// labwc starts Xwayland and the socket is there — /tmp/.X11-unix/X0 exists — but nothing in
/// the session exports DISPLAY, and a child inherits only what the shell has. So Chromium,
/// whose .desktop file says `Exec=/usr/bin/chromium %U`, started, failed to find an X server,
/// printed "Missing X server or $DISPLAY", and exited. `open_app` had already answered
/// `accepted: true`. Setting DISPLAY is the entire fix: with it, the same command opens a
/// window.
///
/// The toolkit hints are the other half of the same thought. GTK and Qt both take a
/// preference LIST, so a Wayland-native app uses Wayland and one that cannot falls back to
/// Xwayland on its own. Neither is forced, and an app that already sets them keeps its choice.
fn session_env() -> Vec<(&'static str, String)> {
    let mut env = Vec::new();

    if std::env::var_os("DISPLAY").is_none() {
        if let Some(display) = x_display() {
            env.push(("DISPLAY", display));
        }
    }
    if std::env::var_os("GDK_BACKEND").is_none() {
        env.push(("GDK_BACKEND", "wayland,x11".to_string()));
    }
    if std::env::var_os("QT_QPA_PLATFORM").is_none() {
        env.push(("QT_QPA_PLATFORM", "wayland;xcb".to_string()));
    }
    env
}

/// Which X display Xwayland is serving, read from its socket rather than assumed.
///
/// `:0` is the usual answer and hardcoding it would work today, but the number is chosen by
/// whoever started Xwayland, and a session that already had one running gets `:1`.
fn x_display() -> Option<String> {
    let dir = std::fs::read_dir("/tmp/.X11-unix").ok()?;
    let mut numbers: Vec<u32> = dir
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            name.strip_prefix('X')?.parse::<u32>().ok()
        })
        .collect();
    numbers.sort_unstable();
    numbers.first().map(|n| format!(":{n}"))
}

pub fn spawn_app_in(app_id: &str, bin: &str, args: &[&str], dir: Option<&std::path::Path>) {
    let path = resolve_app_binary(bin);
    let mut command = std::process::Command::new(&path);
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    command.args(args);
    for (key, value) in session_env() {
        command.env(key, value);
    }
    match command
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
            let owns_window = crate::running::mark_launched(app_id, pid, bin);
            // Reap it when it exits. Without a wait, every app the shell ever launched lingers
            // as a zombie until the shell itself quits — and a zombie still has a /proc entry,
            // which is enough to confuse anything that checks "is that pid alive". The same wait
            // is where the registry learns the window has closed.
            let id = app_id.to_string();
            let name = bin.to_string();
            // A launch is not a window. Spawning succeeds for anything executable, so the only
            // evidence that an app actually came up is that it is still there a moment later —
            // and the only evidence it did not is the exit this thread is already waiting for.
            // It used to be logged at info and discarded, which is how "accepted: true" and an
            // empty screen could both be true at once.
            crate::running::clear_launch_failure(&id);
            let started = std::time::Instant::now();
            std::thread::spawn(move || {
                match child.wait() {
                    Ok(status) => {
                        let lived = started.elapsed();
                        let lived_ms = lived.as_millis() as u64;
                        // A second copy of a single-instance app exits at once, on purpose: it
                        // has asked the window that is already open to show itself. That is a
                        // handover, and reporting it as a failed launch put every relaunch of
                        // notes or the terminal in `describe shell`'s failed_launches.
                        if lived_ms < crate::running::LAUNCH_GRACE_MS
                            && !owns_window
                            && status.success()
                        {
                            tracing::info!(
                                app = %name, lived_ms,
                                "A second copy handed over to the window already open"
                            );
                        } else if lived_ms < crate::running::LAUNCH_GRACE_MS {
                            tracing::warn!(
                                app = %name, %status, lived_ms,
                                "App exited immediately — it never showed a window"
                            );
                            crate::running::mark_launch_failed(
                                &id, &name, &status.to_string(), lived_ms,
                            );
                        } else {
                            tracing::info!(app = %name, %status, "App exited");
                        }
                    }
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
                route(id).is_some(),
                "`{id}` normalises to `{}`, which no route answers to",
                canonical_id(id)
            );
        }
    }

    /// The apps in apps/, by the name each binary carries.
    ///
    /// `yantrik-network-manager` is launched by typing `network-manager` long before anyone
    /// learns the arm is called `network`, and that is the name `open_app` gets. Three of these
    /// had no arm at all — network-manager, text-editor and image-viewer — and the first two
    /// were ACCEPTED by the guard because a .desktop entry matched, so the call reported success
    /// and did nothing.
    const SHIPPED_APPS: &[&str] = &[
        "calendar", "container-manager", "document-editor", "download-manager", "email",
        "image-viewer", "music-player", "network-manager", "notes", "presentation",
        "snippet-manager", "spreadsheet", "system-monitor", "terminal", "text-editor", "weather",
    ];

    #[test]
    fn every_app_we_ship_opens_by_the_name_of_its_binary() {
        for app in SHIPPED_APPS {
            if shelved(app).is_some() {
                continue;
            }
            assert!(
                route(app).is_some(),
                "apps/{app} ships a binary that `open_app name={app}` cannot launch"
            );
        }
    }

    /// Every app under `apps/` is either shipped or shelved, and never both.
    ///
    /// The two lists are what a reader compares to answer "what is in this build", so a name that
    /// is in neither, or in both, is the drift this whole table exists to prevent.
    #[test]
    fn an_app_is_either_shipped_or_shelved() {
        for app in SHIPPED_APPS {
            let on_shelf = shelved(app).is_some();
            let routed = route(app).is_some();
            assert!(
                on_shelf != routed,
                "apps/{app} is {}",
                if on_shelf { "both shelved and routed" } else { "neither shelved nor routed" }
            );
        }
        // And every shelf entry names an app that is really there. A shelf row for something
        // that has been deleted refuses a name nothing would ever ask for.
        for shelf in SHELVED {
            assert!(
                SHIPPED_APPS.iter().any(|a| shelved(a).map(|s| s.binary) == Some(shelf.binary)),
                "the shelf names {}, which is not one of the apps in apps/",
                shelf.binary
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
            "containers", "downloads", "snippets", "documents", "presentation",
            "sysmonitor", "devices", "permissions", "slides", "text_editor", "image_viewer",
        ] {
            assert!(is_known_app(id, &[]), "the dispatch launches `{id}` but the guard refuses it");
        }
    }

    /// Every tile the launcher shows for the shell's own screens opens something.
    ///
    /// About and Skills were tiles with no route: a click logged "Unknown app", and `open_app`
    /// answered "launching" because the catalogue entry made them look known.
    #[test]
    fn every_builtin_tile_has_somewhere_to_go() {
        for entry in crate::apps::builtin_apps() {
            assert!(
                route(&entry.app_id).is_some(),
                "the launcher shows `{}` ({}), and nothing opens it",
                entry.name,
                entry.app_id
            );
            assert!(entry_is_launchable(&entry));
        }
    }

    /// A shell app whose program is not on the disk is known, and not launchable.
    #[test]
    fn a_missing_program_is_known_but_does_not_open() {
        let there = PathBuf::from("/definitely/not/here/yantrik-notes");
        assert!(find_program(there.to_str().unwrap()).is_none());
        assert_eq!(
            program_availability("/definitely/not/here/yantrik-notes --new"),
            Availability::Missing("/definitely/not/here/yantrik-notes".to_string())
        );
        // Screens are compiled into the shell; they are always there.
        assert_eq!(availability("files", &[]), Availability::Ready);
        assert_eq!(availability("about", &[]), Availability::Ready);
        assert_eq!(availability("skills", &[]), Availability::Ready);
    }

    /// A .desktop file whose program is gone is kept out of the launcher, and one whose program
    /// is present is kept in it.
    #[cfg(unix)]
    #[test]
    fn a_desktop_entry_is_listed_only_if_its_program_exists() {
        let entry = |exec: &str| DesktopEntry {
            name: "Thing".into(),
            exec: exec.into(),
            icon: String::new(),
            categories: String::new(),
            comment: String::new(),
            app_id: "thing".into(),
            icon_char: String::new(),
        };
        assert!(entry_is_launchable(&entry("/bin/sh -c true")));
        assert!(entry_is_launchable(&entry("sh")), "a bare name is looked up on PATH");
        assert!(!entry_is_launchable(&entry("/usr/bin/no-such-browser-anywhere %U")));
        assert!(!entry_is_launchable(&entry("no-such-browser-anywhere")));

        let gone = [entry("no-such-browser-anywhere")];
        assert_eq!(
            availability("thing", &gone),
            Availability::Missing("no-such-browser-anywhere".to_string())
        );
        assert!(!is_launchable("thing", &gone));
        assert!(is_known_app("thing", &gone), "known, so the caller hears 'not installed'");
    }

    /// A file that exists but cannot be executed is not a program.
    #[cfg(unix)]
    #[test]
    fn a_file_that_cannot_run_is_not_a_program() {
        let dir = std::env::temp_dir().join(format!("yantrik-dock-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("not-executable");
        std::fs::write(&file, "#!/bin/sh
").unwrap();
        assert!(find_program(file.to_str().unwrap()).is_none());
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(find_program(file.to_str().unwrap()).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The browser row is the Browser pin, whichever browser answers it.
    #[test]
    fn browser_is_one_route_with_several_programs() {
        assert_eq!(route("browser"), Some(Launch::Browser));
        assert!(BROWSERS.iter().any(|(name, _)| *name == "chromium"));
        assert!(BROWSERS.iter().any(|(name, _)| name.starts_with("firefox")));
        // Only the Chromium family gets Chromium's flags; Firefox would refuse to start on them.
        for (name, flags) in BROWSERS {
            let chromium = name.contains("chrom");
            assert_eq!(!flags.is_empty(), chromium, "{name}");
            assert!(!flags.iter().any(|f| f.contains("/tmp")), "{name}: a /tmp profile forgets everything");
        }
    }

    /// No name is claimed by two routes: the first would always win, silently.
    #[test]
    fn every_name_routes_to_exactly_one_app() {
        let mut seen = std::collections::HashSet::new();
        for id in builtin_app_ids() {
            assert!(seen.insert(id), "`{id}` appears in two routes");
            assert_eq!(canonical_id(id), id, "`{id}` is not canonical, so it can never match");
        }
    }

    #[test]
    fn an_app_that_does_not_exist_is_still_refused() {
        // The guard must not have become a rubber stamp on the way to being more generous.
        assert!(!is_known_app("nonexistent-app", &[]));
        assert!(!is_known_app("", &[]));
    }

    // ── The shelf ──

    /// A shelved .desktop entry, as an installed machine still has one on disk.
    fn stale_entry(app_id: &str, name: &str, exec: &str) -> DesktopEntry {
        DesktopEntry {
            name: name.into(),
            exec: exec.into(),
            icon: String::new(),
            categories: String::new(),
            comment: String::new(),
            app_id: app_id.into(),
            icon_char: String::new(),
        }
    }

    /// Every spelling a caller could arrive with is refused, and refused for the same reason.
    ///
    /// A shelf that only catches one spelling is not a shelf: `music-player` is what the binary
    /// is called, `Music Player` is the window title, `music` is what the dock says, and an agent
    /// reads whichever of those it saw last.
    #[test]
    fn every_spelling_of_a_shelved_app_reaches_the_shelf() {
        for spelling in [
            "music", "music_player", "music-player", "Music Player", "MUSIC",
            "  music  ", "yantrik-music-player",
        ] {
            let shelf = shelved(spelling).unwrap_or_else(|| panic!("`{spelling}` is not shelved"));
            assert_eq!(shelf.binary, "yantrik-music-player", "{spelling}");
        }
        for spelling in ["spreadsheet", "Spreadsheet", "ySheets", "ysheets", "yantrik-spreadsheet"] {
            let shelf = shelved(spelling).unwrap_or_else(|| panic!("`{spelling}` is not shelved"));
            assert_eq!(shelf.binary, "yantrik-spreadsheet", "{spelling}");
        }
    }

    /// Opening a shelved app is refused, and never answered "launching".
    ///
    /// Checked against a catalogue that still holds the app, because that is the state of every
    /// machine updated from a release that had it: the binary and the .desktop file are both
    /// still on the disk, and the updater removes neither.
    #[test]
    fn a_shelved_app_does_not_open_even_with_its_desktop_file_on_disk() {
        let stale = [
            stale_entry("yantrik-music-player", "Music", "/opt/yantrik/bin/yantrik-music-player"),
            stale_entry("yantrik-spreadsheet", "ySheets", "/opt/yantrik/bin/yantrik-spreadsheet"),
        ];
        for name in ["music", "music-player", "Music", "spreadsheet", "ySheets"] {
            assert!(!is_launchable(name, &stale), "`{name}` must not open");
            assert!(
                matches!(availability(name, &stale), Availability::Shelved(_)),
                "`{name}` must be refused as shelved, not as unknown or missing"
            );
        }
    }

    /// The catalogue filter drops a shelved entry, whichever way it is named.
    ///
    /// Matched on the program the Exec line runs as well as on the id, because a .desktop file
    /// left on disk by an earlier release is the case this has to survive and nothing says its
    /// basename will still be one the shelf recognises.
    #[test]
    fn the_catalogue_drops_a_shelved_desktop_entry() {
        assert!(!entry_is_launchable(&stale_entry(
            "yantrik-music-player", "Music", "/opt/yantrik/bin/yantrik-music-player"
        )));
        assert!(!entry_is_launchable(&stale_entry(
            "yantrik-spreadsheet", "ySheets", "/opt/yantrik/bin/yantrik-spreadsheet"
        )));
        // Renamed by hand, or installed somewhere else: the program is what gives it away.
        assert!(!entry_is_launchable(&stale_entry(
            "sheets-old", "Sheets", "/usr/local/bin/yantrik-spreadsheet %f"
        )));
        // And an app that is not shelved is untouched by any of it.
        assert!(entry_is_launchable(&stale_entry("shell", "Shell", "/bin/sh")));
    }

    /// No route points at a shelved binary, and no shelved name is offered as something to open.
    #[test]
    fn nothing_routes_to_a_shelved_app() {
        for (names, launch) in ROUTES {
            for name in *names {
                assert!(shelved(name).is_none(), "`{name}` is both routed and shelved");
            }
            if let Launch::Program { bin, .. } = launch {
                assert!(
                    shelved_exec(bin).is_none(),
                    "a route runs {bin}, which is a shelved binary"
                );
            }
        }
        // The list a refusal hands back must not name something that would itself be refused.
        let offered = launchable_app_ids(&[]);
        for id in &offered {
            assert!(shelved(id).is_none(), "`{id}` is offered as launchable and is shelved");
        }
    }

    /// Every shelf entry says why, and says what would bring it back.
    ///
    /// Both strings are quoted straight into the refusal a person or a mind reads, so an empty
    /// one is a refusal that explains nothing.
    #[test]
    fn a_shelf_entry_argues_for_itself() {
        for shelf in SHELVED {
            assert!(!shelf.reason.trim().is_empty(), "{} has no reason", shelf.binary);
            assert!(
                !shelf.returns_when.trim().is_empty(),
                "{} does not say what would bring it back",
                shelf.binary
            );
            assert!(!shelf.ids.is_empty(), "{} answers to no name", shelf.binary);
            for id in shelf.ids {
                assert_eq!(canonical_id(id), *id, "`{id}` is not canonical, so it can never match");
            }
        }
    }

    /// The apps that ship are untouched by the shelf.
    #[test]
    fn un_shelved_apps_are_unaffected() {
        for id in ["notes", "terminal", "files", "calendar", "email", "documents", "presentation"] {
            assert!(shelved(id).is_none(), "`{id}` is not shelved");
            assert!(route(id).is_some(), "`{id}` still routes");
            assert!(is_known_app(id, &[]), "`{id}` is still known");
        }
    }

    /// The release is packaged from one list of exclusions, and it is this one.
    ///
    /// build-release.sh discovers the binaries to ship by looking at what the build produced, so
    /// a shelved crate that is still a workspace member would be packaged simply because it
    /// compiled. The script therefore carries the same two binary names, and a shelf that grows
    /// an entry the script does not know about would ship the app it just refused to open.
    #[test]
    fn the_release_script_excludes_every_shelved_binary() {
        let script = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../deploy/yantrik-os/build-release.sh");
        let Ok(text) = std::fs::read_to_string(&script) else {
            return; // Packaged source without the deploy tree; nothing to check against.
        };
        // The release script used to carry its own copy of the shelf and this test read the
        // names out of it. Five scripts carried such a copy and four were wrong, so the list
        // is no longer written down anywhere but here: `shelved-bins.sh` reads the table above
        // and every packaging script asks it. What can still go wrong is a script that stops
        // asking, or a reader that stops reading this file — so that is what is checked.
        assert!(
            text.contains("shelved-bins.sh"),
            "{} no longer asks shelved-bins.sh what is shelved, so a release would ship it all",
            script.display()
        );
        let reader = script.with_file_name("shelved-bins.sh");
        let reader_text = std::fs::read_to_string(&reader)
            .unwrap_or_else(|e| panic!("{} is missing: {e}", reader.display()));
        assert!(
            reader_text.contains("crates/yantrik-ui/src/wire/dock.rs"),
            "{} does not read the SHELVED table in dock.rs",
            reader.display()
        );
        // Its sed pattern takes `binary: "…"` lines that start with whitespace. Hold the table
        // to that shape, or an entry reformatted onto one line would silently leave the shelf.
        let this_file = include_str!("dock.rs");
        for shelf in SHELVED {
            let as_the_script_sees_it = this_file.lines().any(|line| {
                line.starts_with(char::is_whitespace)
                    && line.trim_start().starts_with(&format!("binary: \"{}\"", shelf.binary))
            });
            assert!(
                as_the_script_sees_it,
                "{} is shelved but not written the way shelved-bins.sh reads it",
                shelf.binary
            );
        }
    }
}
