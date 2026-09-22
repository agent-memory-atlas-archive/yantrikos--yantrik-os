// Yantrik UI Kit — reusable Slint components.
// Consuming crates access .slint files via DEP_YANTRIK_UI_KIT_SLINT_PATH env var.
//
// Components: AppHeader, YButton, YIconButton, YInput, YDialog, YTabs,
// YSidebar, YListItem, YContextMenu, YEmptyState, LineChart, RadarChart,
// ToastBanner, MessageBubble, Icon.
//
// AppHeader is the one every app is required to use; see docs/app-sdk.md. YToolbar and
// AppShell used to sit here and were instantiated by nothing, because each spent Slint's
// single `@children` on a region no app needed. They are gone.

#[cfg(test)]
mod app_header_is_mandatory {
    use std::path::{Path, PathBuf};

    /// Every app we ship, and the screen component that draws it.
    ///
    /// An app binary is a thin `Window` around one of these, so this is where an app's top edge
    /// is decided. Adding an app means adding a line here; that is the point.
    const APP_SCREENS: &[(&str, &str)] = &[
        ("calendar", "calendar.slint"),
        ("container-manager", "container_manager.slint"),
        ("document-editor", "document_editor.slint"),
        ("download-manager", "download_manager.slint"),
        ("email", "email.slint"),
        ("image-viewer", "image_viewer.slint"),
        ("music-player", "music_player.slint"),
        ("network-manager", "network_manager.slint"),
        ("notes", "notes_editor.slint"),
        ("presentation", "presentation.slint"),
        ("snippet-manager", "snippet_manager.slint"),
        ("spreadsheet", "spreadsheet.slint"),
        ("system-monitor", "system_monitor.slint"),
        ("terminal", "terminal.slint"),
        ("text-editor", "text_editor.slint"),
        ("weather", "weather.slint"),
    ];

    fn ui_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../yantrik-ui-slint/ui")
            .canonicalize()
            .expect("the shell's ui directory sits beside the kit")
    }

    /// The check that makes the header consistent by construction rather than by agreement.
    ///
    /// Sixteen apps once drew their own top bar and the heights came out 28, 32, 36, 44, 48, 56
    /// and 64 — nobody chose that, it is just what happens when sixteen files each decide. A
    /// screen that instantiates AppHeader cannot choose: the height, the padding, the glyph tile
    /// and the button size all belong to the component, and the app supplies only data.
    ///
    /// So this asserts the one thing that cannot be re-derived from the markup: that the app
    /// went through the frame at all.
    #[test]
    fn every_app_we_ship_draws_its_header_with_the_shared_component() {
        let dir = ui_dir();
        let mut missing = Vec::new();

        for (app, screen) in APP_SCREENS {
            let path = dir.join(screen);
            let src = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{app}: cannot read {}: {e}", path.display()));

            if !src.contains("AppHeader {") {
                missing.push(format!("{app} ({screen}) draws its own header"));
            }
        }

        assert!(
            missing.is_empty(),
            "these apps do not use the shared header:\n  {}\n\n\
             Use AppHeader from the UI kit. Header content is data — `title`, `icon`, \
             `subtitle` and an `actions` model of AppAction — so that every app's top edge is \
             identical without anyone having to remember a number. See docs/app-sdk.md.",
            missing.join("\n  ")
        );
    }

    /// The header's height is a token, not a literal an app can retype.
    ///
    /// `h-app-header` exists so there is exactly one answer to "how tall is an app's top edge".
    /// If AppHeader ever hardcodes a number instead, the token stops meaning anything and the
    /// drift can start again from one file.
    #[test]
    fn the_header_takes_its_height_from_the_token() {
        let src = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("slint/app_header.slint"),
        )
        .expect("app_header.slint is part of this crate");

        assert!(
            src.contains("height: Theme.h-app-header;"),
            "AppHeader must take its height from Theme.h-app-header, so the one number that \
             decides how tall an app's top edge is lives in the design tokens"
        );
    }

    /// The components that got us here are not allowed to come back.
    ///
    /// YToolbar and AppShell were each instantiated by nothing for the same reason: a Slint
    /// component has one `@children`, both spent it on a region no app needed, and so every app
    /// went and drew its own header instead. A half-usable shared component is worse than none —
    /// it looks like the problem is solved.
    #[test]
    fn the_shells_that_nobody_could_use_are_still_gone() {
        let kit = Path::new(env!("CARGO_MANIFEST_DIR")).join("slint");
        for dead in ["y_toolbar.slint", "app_shell.slint"] {
            assert!(
                !kit.join(dead).exists(),
                "{dead} is back. If a shared component cannot express what every app needs \
                 (for a header: trailing actions), apps will bypass it and hand-roll their own. \
                 Pass the content as data instead of as children — see AppHeader."
            );
        }
    }
}

// ── What an app costs while nobody is using it ──────────────────────────────
//
// An open Terminal showing nothing but a prompt burned about one and a half cores, for hours,
// on a desktop where nobody had typed anything (#29, and most of #53's idle total). Nothing was
// running in the shell; the screen held `$ ` and a cursor. The cause was four lines of markup —
// the cursor rectangle carried
//
//     animate blink-opacity { duration: 600ms; iteration-count: -1; }
//
// and an animation that never finishes is a window that never stops redrawing. Put back and
// measured from /proc/<pid>/stat deltas, that one property costs 110% of a core at an empty
// prompt; the same binary without it spends zero jiffies in thirty seconds. The compositor pays
// again on top of that, which is why labwc sat at 27% beside it.
//
// No functional test sees this. Every screenshot is right, every key works, every action
// answers — the app is simply expensive. So the check has to read the markup, and it has to
// cover every app rather than the one that was caught, because the mistake is a single property
// that anyone can type again. The two ways a Slint window asks to be redrawn for ever are an
// animation with a negative iteration count and a Timer that is always running; both are
// refused here for every screen an app window actually compiles.
#[cfg(test)]
mod an_idle_app_stops_drawing {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    fn repo() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("the kit sits two levels under the checkout")
    }

    /// The directories every app's `build.rs` hands to the Slint compiler, in its search order.
    ///
    /// Two of these hold files of the same name (`app_header.slint` lives in the kit, and
    /// `components/app_header.slint` in the shell's ui directory), so the order is what decides
    /// which file an app is really compiling. It is the order the `with_include_paths` call in
    /// every `apps/*/build.rs` uses.
    fn include_paths(repo: &Path) -> Vec<PathBuf> {
        vec![
            repo.join("crates/yantrik-design-tokens/slint"),
            repo.join("crates/yantrik-ui-kit/slint"),
            repo.join("crates/yantrik-ui-slint/ui"),
        ]
    }

    /// Every `.slint` file one app window compiles: its own `ui/app.slint` and, through the
    /// import graph, the screens and components that file reaches.
    ///
    /// Following the imports is the point. The animation that cost #29 its cores was not in the
    /// terminal's own `app.slint` — it was in a shared screen the terminal imported, and a check
    /// that only read the file named after the app would have walked straight past it.
    fn markup_reachable_from(entry: &Path, includes: &[PathBuf]) -> BTreeSet<PathBuf> {
        let mut seen = BTreeSet::new();
        let mut queue = vec![entry.to_path_buf()];
        while let Some(file) = queue.pop() {
            if !seen.insert(file.clone()) {
                continue;
            }
            let source = std::fs::read_to_string(&file)
                .unwrap_or_else(|e| panic!("cannot read {}: {e}", file.display()));
            for spec in imported_paths(&source) {
                // Slint's own widget library, which has no file in this checkout.
                if spec == "std-widgets.slint" {
                    continue;
                }
                let own = file.parent().expect("a file has a directory").to_path_buf();
                let resolved = std::iter::once(own)
                    .chain(includes.iter().cloned())
                    .map(|base| base.join(&spec))
                    .find(|candidate| candidate.is_file())
                    .unwrap_or_else(|| {
                        panic!(
                            "{} imports {spec}, which is in none of the include paths",
                            file.display()
                        )
                    });
                queue.push(resolved);
            }
        }
        seen
    }

    /// The `.slint` paths one file imports: everything inside the quotes of `from "…"`.
    fn imported_paths(source: &str) -> Vec<String> {
        source
            .split("from \"")
            .skip(1)
            .filter_map(|rest| rest.split_once('"'))
            .map(|(spec, _)| spec.to_string())
            .filter(|spec| spec.ends_with(".slint"))
            .collect()
    }

    /// The source with `//` comments removed, so a commented-out animation is not a finding.
    ///
    /// Line comments only, and applied to the copy this check reads rather than to the one the
    /// imports are resolved from. A `//` inside a string literal would take the rest of that
    /// line with it; no shipped screen has one, and the cost would be a missed finding on one
    /// line rather than a false one.
    fn without_comments(source: &str) -> String {
        source
            .lines()
            .map(|line| line.split_once("//").map_or(line, |(code, _)| code))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// What a `Timer` body gives its `running` property, if it sets one.
    ///
    /// Read by hand rather than with a search for `"running"`, because a Timer's `triggered =>`
    /// handler is part of the same body and may well mention a name that ends in `running`.
    fn running_gate(body: &str) -> Option<String> {
        let mut at = 0;
        while let Some(offset) = body[at..].find("running").map(|i| at + i) {
            at = offset + "running".len();
            let before = body[..offset].chars().next_back();
            let own_word = !before.is_some_and(|c| c.is_alphanumeric() || c == '-' || c == '_');
            if own_word {
                if let Some(value) = body[at..].trim_start().strip_prefix(':') {
                    return Some(value.split(';').next().unwrap_or("").trim().to_string());
                }
            }
        }
        None
    }

    /// The body of every `Timer { … }` in one file, with the line it starts on.
    fn timer_bodies(source: &str) -> Vec<(usize, String)> {
        let bytes = source.as_bytes();
        let mut found = Vec::new();
        let mut at = 0;
        while let Some(offset) = source[at..].find("Timer").map(|i| at + i) {
            at = offset + "Timer".len();
            let open = source[at..]
                .find(|c: char| !c.is_whitespace())
                .map(|i| at + i)
                .filter(|&i| bytes[i] == b'{');
            let Some(open) = open else { continue };
            let mut depth = 1usize;
            let mut end = open + 1;
            while end < bytes.len() && depth > 0 {
                match bytes[end] {
                    b'{' => depth += 1,
                    b'}' => depth -= 1,
                    _ => {}
                }
                end += 1;
            }
            let close = end.saturating_sub(1).max(open + 1);
            found.push((
                source[..offset].matches('\n').count() + 1,
                source[open + 1..close].to_string(),
            ));
            at = end;
        }
        found
    }

    #[test]
    fn no_app_window_asks_to_be_redrawn_for_ever() {
        let repo = repo();
        let includes = include_paths(&repo);
        let mut apps: Vec<PathBuf> = std::fs::read_dir(repo.join("apps"))
            .expect("the apps we ship live in apps/")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|dir| dir.join("ui/app.slint").is_file())
            .collect();
        apps.sort();
        // A path mistake here would make the whole check pass by reading nothing, so it says out
        // loud that it found the apps — and the one this came from by name.
        assert!(
            apps.iter().any(|app| app.ends_with("terminal")),
            "no app windows found under {}/apps; this check reads every app's ui/app.slint and \
             found {} of them",
            repo.display(),
            apps.len()
        );

        let mut never_still = Vec::new();
        for app in &apps {
            let name = app.file_name().expect("an app has a directory name").to_string_lossy();
            for file in markup_reachable_from(&app.join("ui/app.slint"), &includes) {
                let source = without_comments(
                    &std::fs::read_to_string(&file)
                        .unwrap_or_else(|e| panic!("cannot read {}: {e}", file.display())),
                );
                let shown = file.strip_prefix(&repo).unwrap_or(&file).display().to_string();
                let where_ = |line: usize| format!("{name}: {shown}:{line}");
                for (number, line) in source.lines().enumerate() {
                    let Some((_, count)) = line.split_once("iteration-count") else {
                        continue;
                    };
                    let count = count
                        .trim_start()
                        .trim_start_matches(':')
                        .split(';')
                        .next()
                        .unwrap_or("")
                        .trim();
                    if count.starts_with('-') {
                        never_still.push(format!(
                            "{} — animate … {{ iteration-count: {count} }}",
                            where_(number + 1)
                        ));
                    }
                }
                for (line, body) in timer_bodies(&source) {
                    match running_gate(&body) {
                        // Gated on state: it ticks only while something is happening.
                        Some(gate) if gate != "true" => {}
                        Some(_) => never_still
                            .push(format!("{} — Timer {{ running: true }}", where_(line))),
                        // Slint's Timer runs by default, so saying nothing says true.
                        None => never_still.push(format!(
                            "{} — Timer with no `running`, which defaults to true",
                            where_(line)
                        )),
                    }
                }
            }
        }
        never_still.sort();
        never_still.dedup();

        assert!(
            never_still.is_empty(),
            "these app windows never stop redrawing, so they cost CPU while nobody is using \
             them:\n  {}\n\n\
             An animation with a negative iteration count, and a Timer that is always running, \
             both hold the window at the display's frame rate for as long as it is open — the \
             app pays, and the compositor pays again to composite each frame. Measured on the \
             terminal, one such animation was 110% of a core at an empty prompt.\n\n\
             Drive the effect from state instead: gate the Timer on the thing that is happening \
             (`running: root.is-loading`), and let an animation run to its end rather than \
             repeat for ever. An app that is doing nothing has to ask for no frames at all — \
             apps/terminal's `real_window_shell_tabs_search_clipboard_resize_and_idle` asserts \
             exactly that for one app, by counting the frames the renderer requests.",
            never_still.join("\n  ")
        );
    }
}
