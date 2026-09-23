//! App grid — populate grid apps from installed apps, handle launch, search and categories.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::app_context::AppContext;
use crate::apps::DesktopEntry;
use crate::icons;
use crate::{App, AppGridItem, CategoryItem};

pub fn wire(ui: &App, ctx: &AppContext) {
    let catalogue = ctx.installed_apps.clone();
    let installed = catalogue.get();
    // The visible search field is recreated on open. Reset both backing filters
    // with it so category clicks cannot resurrect a previous search.
    let query = Rc::new(RefCell::new(String::new()));
    let category = Rc::new(RefCell::new(String::from("all")));

    // Rescan every time the launcher opens.
    //
    // The catalogue used to be scanned once at startup, so an app installed while the shell was
    // running simply did not exist: not in the launcher, not in the Lens, not to `open_app`.
    // The launcher opening is exactly the moment someone who has just installed something goes
    // looking for it, and a scan is a few directories of small ini files -- cheap enough to do
    // on a keystroke and far cheaper than being wrong.
    {
        let catalogue = catalogue.clone();
        let query = query.clone();
        let category = category.clone();
        let weak = ui.as_weak();
        ui.on_app_grid_opened(move || {
            let count = catalogue.refresh();
            tracing::debug!(apps = count, "rescanned installed apps for the launcher");
            if let Some(ui) = weak.upgrade() {
                query.borrow_mut().clear();
                *category.borrow_mut() = "all".to_string();
                ui.set_grid_active_category("all".into());
                let apps = catalogue.get();
                ui.set_grid_categories(ModelRc::new(VecModel::from(categories_for(&apps))));
                populate_grid(&ui, &apps, "", "all");
            }
        });
    }

    // The two filters compose: whichever one changes, the other is re-applied from here.
    ui.set_grid_categories(ModelRc::new(VecModel::from(categories_for(&installed))));
    populate_grid(ui, &installed, "", "all");

    {
        let catalogue = catalogue.clone();
        let query = query.clone();
        let category = category.clone();
        let ui_weak = ui.as_weak();
        ui.on_grid_search_apps(move |q| {
            *query.borrow_mut() = q.to_string();
            if let Some(ui) = ui_weak.upgrade() {
                populate_grid(&ui, &catalogue.get(), &query.borrow(), &category.borrow());
            }
        });
    }
    {
        let catalogue = catalogue.clone();
        let query = query.clone();
        let category = category.clone();
        let ui_weak = ui.as_weak();
        ui.on_grid_category_selected(move |id| {
            *category.borrow_mut() = id.to_string();
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_grid_active_category(id.clone());
                populate_grid(&ui, &catalogue.get(), &query.borrow(), &category.borrow());
            }
        });
    }

    // Pin or unpin, from the launcher — the one place every app is listed, and so the one place
    // a pin can always be made or undone.
    {
        let catalogue = catalogue.clone();
        let query = query.clone();
        let category = category.clone();
        let ui_weak = ui.as_weak();
        ui.on_grid_toggle_pin(move |app_id| {
            super::pins::toggle(&app_id);
            let Some(ui) = ui_weak.upgrade() else { return };
            let apps = catalogue.get();
            // Unpinning the last app while looking at "Pinned" would leave the filter selected
            // with the category gone from the list beside it. Fall back to All, which is where a
            // person would have to click next anyway.
            if category.borrow().as_str() == "pinned"
                && !apps.iter().any(|e| super::pins::is_pinned(&e.app_id))
            {
                *category.borrow_mut() = "all".to_string();
                ui.set_grid_active_category("all".into());
            }
            ui.set_grid_categories(ModelRc::new(VecModel::from(categories_for(&apps))));
            populate_grid(&ui, &apps, &query.borrow(), &category.borrow());
            // START updates now, not on the next three-second poll — a pin that takes three
            // seconds to appear reads as a click that did not work.
            super::pins::publish(&ui, &apps);
        });
    }

    // Handle grid-launch-app — routes built-in apps to screens, external apps to processes
    let ui_weak = ui.as_weak();
    ui.on_grid_launch_app(move |app_id| {
        let app_id_str = app_id.as_str();

        // Close grid first
        if let Some(ui) = ui_weak.upgrade() {
            ui.set_app_grid_open(false);
        }

        // Built-in Yantrik apps → navigate to screen via launch-app callback
        let installed = catalogue.get();
        if let Some(entry) = installed.iter().find(|e| e.app_id == app_id_str) {
            if entry.exec == "__builtin__" {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.invoke_launch_app(app_id);
                }
                return;
            }
            tracing::info!(app = %entry.name, exec = %entry.exec, "Launching app from grid");
            let exec_clean = entry
                .exec
                .split_whitespace()
                .filter(|w| !w.starts_with('%'))
                .collect::<Vec<_>>();
            if let Some(cmd) = exec_clean.first() {
                // Through the shell's one launcher, not a bare Command.
                //
                // This spawned directly, so it inherited the shell's whole environment --
                // including SLINT_FULLSCREEN=1, which the session sets because the shell IS the
                // OS and must not be a window. An app that inherits it opens fullscreen with no
                // titlebar, no taskbar and no way out: "I opened notes app and now its showing
                // no option to close".
                //
                // spawn_app_in already removed that variable, registered the launch and reaped
                // the child. Its own doc comment says the paths were unified "so the registry,
                // the environment scrubbing and the reaper cannot end up applying to one launch
                // path and not the other". This was a third path, and it was never brought in.
                // Registered under the canonical id, not the .desktop basename.
                //
                // The catalogue keys entries by filename, so this OS's own apps come through as
                // "yantrik-notes" while APP_NAMES — the single naming source the taskbar, the
                // dock and the window list all read — says "notes". Passing the filename made
                // the taskbar fall through to its title-casing fallback and label the window
                // "Yantrik notes", beside a titlebar saying "Notes".
                let canonical = app_id_str.strip_prefix("yantrik-").unwrap_or(&app_id_str);
                super::dock::spawn_app_with_args(canonical, cmd, &exec_clean[1..]);
            }
        }
    });
}

/// "All" plus every category that has at least one app, in CATEGORY_TABLE order.
fn categories_for(installed: &Arc<Vec<DesktopEntry>>) -> Vec<CategoryItem> {
    let mut out = vec![CategoryItem {
        id: "all".into(),
        name: "All".into(),
        count: installed.len() as i32,
    }];
    // Second, because it is the one a person curates. Only when it has something in it: an
    // empty filter would open onto "Nothing matches — try another word", which is advice for a
    // search, not for a list you have not started yet.
    let pinned = installed.iter().filter(|e| super::pins::is_pinned(&e.app_id)).count();
    if pinned > 0 {
        out.push(CategoryItem {
            id: "pinned".into(),
            name: "Pinned".into(),
            count: pinned as i32,
        });
    }
    for (_, id) in icons::CATEGORY_TABLE {
        let count = installed
            .iter()
            .filter(|e| icons::category_id(&e.categories) == *id)
            .count();
        if count > 0 {
            out.push(CategoryItem {
                id: SharedString::from(*id),
                name: SharedString::from(icons::category_label(id)),
                count: count as i32,
            });
        }
    }
    out
}

/// The id the icon set is keyed by, for one of the apps this OS ships.
///
/// Two naming schemes meet here and neither is wrong. A freedesktop entry needs a name unique
/// across everything installed on the machine, so ours are `yantrik-download-manager`. The
/// icon set is keyed by what the rest of the shell calls the same app, which is `downloads`.
/// Stripping the prefix gets six of the sixteen; the other ten need saying out loud.
///
/// Anything not listed keeps its own id, so a third-party app is unaffected and a new app that
/// happens to match an icon name works without an entry.
pub(crate) fn icon_id_for(app_id: &str) -> String {
    let bare = app_id.strip_prefix("yantrik-").unwrap_or(app_id);
    let mapped = match bare {
        "container-manager" => "containers",
        "document-editor" => "documents",
        "download-manager" => "downloads",
        "image-viewer" => "image",
        "music-player" => "music",
        "network-manager" => "network",
        "snippet-manager" => "snippets",
        "system-monitor" => "sysmonitor",
        "text-editor" => "editor",
        other => other,
    };
    mapped.to_string()
}

#[cfg(test)]
mod icon_id_tests {
    use super::icon_id_for;

    /// Every app this OS ships resolves to an id the icon set actually knows.
    ///
    /// The list on the right is `Icons.app` in crates/yantrik-ui-kit/slint/icon.slint. Without
    /// this mapping ten of the sixteen fell through to their category glyph, so Mail,
    /// Downloads and Network Manager all drew the same picture — which is what the launcher
    /// was photographed doing.
    #[test]
    fn every_shipped_app_maps_to_an_icon_the_set_knows() {
        const KNOWN: &[&str] = &[
            "terminal", "browser", "files", "editor", "email", "notes", "system", "network",
            "packages", "memory", "media", "music", "weather", "settings", "calendar", "bond",
            "notifications", "spreadsheet", "documents", "presentation", "launchpad", "yantrik",
            "about", "containers", "devices", "downloads", "permissions", "personality",
            "skills", "snippets", "sysmonitor", "image", "studio",
        ];
        const SHIPPED: &[&str] = &[
            "yantrik-calendar", "yantrik-container-manager", "yantrik-document-editor",
            "yantrik-download-manager", "yantrik-email", "yantrik-image-viewer",
            "yantrik-music-player", "yantrik-network-manager", "yantrik-notes",
            "yantrik-presentation", "yantrik-snippet-manager", "yantrik-spreadsheet",
            "yantrik-studio", "yantrik-system-monitor", "yantrik-terminal",
            "yantrik-text-editor", "yantrik-weather",
        ];

        let missing: Vec<String> = SHIPPED
            .iter()
            .map(|app| (app, icon_id_for(app)))
            .filter(|(_, id)| !KNOWN.contains(&id.as_str()))
            .map(|(app, id)| format!("{app} -> {id}"))
            .collect();

        assert!(
            missing.is_empty(),
            "these apps resolve to an icon id the set does not have, so they will draw their \
             category glyph instead of their own icon:\n  {}",
            missing.join("\n  ")
        );
    }

    /// A foreign app keeps its own id; this table is only for ours.
    #[test]
    fn a_third_party_app_is_left_alone() {
        assert_eq!(icon_id_for("chromium"), "chromium");
        assert_eq!(icon_id_for("org.gnome.Nautilus"), "org.gnome.Nautilus");
    }
}

#[cfg(test)]
mod app_colour_tests {
    use super::icon_id_for;
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    /// The one table that gives an app its colour, in the UI kit.
    fn app_color_slint() -> String {
        let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../yantrik-ui-kit/slint/app_color.slint");
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
    }

    /// The body of one `public pure function <name>` in that file.
    fn function_body<'a>(src: &'a str, name: &str) -> &'a str {
        let start = src
            .find(&format!("public pure function {name}("))
            .unwrap_or_else(|| panic!("app_color.slint has no function {name}"));
        let rest = &src[start..];
        let end = rest.find("\n    }").expect("a function body ends at its closing brace");
        &rest[..end]
    }

    /// `(key, value)` for every `<var> == "key" ? "value"` or `<var> == "key" ? root.x` arm.
    fn arms(body: &str, var: &str) -> Vec<(String, String)> {
        let needle = format!("{var} == \"");
        body.lines()
            .filter_map(|line| {
                let after = &line[line.find(&needle)? + needle.len()..];
                let (key, rest) = after.split_once('"')?;
                let value = rest.split_once('?')?.1.trim();
                let value = value.split("//").next()?.trim().trim_matches('"').to_string();
                Some((key.to_string(), value))
            })
            .collect()
    }

    /// Every app id the launcher, the desktop's workspace row and the taskbar can draw a tile
    /// for: the shell's built-in apps, every app we ship a .desktop entry for (under the id the
    /// icon set and the tiles are keyed by), and every app the taskbar can name a window after.
    fn ids_the_launcher_knows() -> BTreeSet<String> {
        let mut ids: BTreeSet<String> = yantrik_shell_core::apps::builtin_apps()
            .into_iter()
            .map(|entry| icon_id_for(&entry.app_id))
            .collect();
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/desktop-files");
        for entry in std::fs::read_dir(&dir).expect("apps/desktop-files is in the tree") {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            if let Some(stem) = name.strip_suffix(".desktop") {
                ids.insert(icon_id_for(stem));
            }
        }
        ids.extend(crate::windows::APP_NAMES.iter().map(|(id, _)| id.to_string()));
        ids
    }

    /// Every app a tile can be drawn for has a colour of its own.
    ///
    /// An app missing from `AppColor.hue-for-app` does not fail to draw: it falls back to a
    /// quiet grey tile, which is right for a third-party app and wrong for one of ours — it
    /// is how Agents, Arcade and Weather came to wear the house accent in the launcher while
    /// every app beside them had a colour. So the table is held to the launcher's own list.
    #[test]
    fn every_app_the_launcher_knows_has_a_colour() {
        let src = app_color_slint();
        let table: BTreeSet<String> = arms(function_body(&src, "hue-for-app"), "id")
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        let missing: Vec<String> = ids_the_launcher_knows()
            .into_iter()
            .filter(|id| !table.contains(id))
            .collect();
        assert!(
            missing.is_empty(),
            "these apps have no colour in AppColor.hue-for-app \
             (crates/yantrik-ui-kit/slint/app_color.slint), so their tile falls back to grey:\n  {}",
            missing.join("\n  ")
        );
    }

    /// A hue the table names is one the palette has, as a glyph tone AND as a tile.
    ///
    /// A misspelt hue ("amer") would not fail to compile — `hue()` falls through to the accent
    /// and `tile-hue()` to a grey tile — so this is the only thing that catches it.
    #[test]
    fn every_hue_in_the_table_is_in_the_palette() {
        let src = app_color_slint();
        let glyph: BTreeSet<String> =
            arms(function_body(&src, "hue"), "name").into_iter().map(|(h, _)| h).collect();
        let tile: BTreeSet<String> =
            arms(function_body(&src, "tile-hue"), "name").into_iter().map(|(h, _)| h).collect();
        for (id, hue) in arms(function_body(&src, "hue-for-app"), "id") {
            assert!(glyph.contains(&hue), "{id} is {hue:?}, which hue() does not know");
            assert!(tile.contains(&hue), "{id} is {hue:?}, which tile-hue() does not know");
        }
        assert_eq!(glyph, tile, "the glyph tones and the tile fills name the same hues");
    }

    /// The colours the design names are the ones the table gives (desk-and-mind, "Colour per
    /// app"): Files blue, Calendar red, Notes amber, Terminal green, Mail blue, Browser teal,
    /// Studio violet. Files is the folder blue ("sky") so it and Mail, alphabetical
    /// neighbours in the launcher as Email and Files, are two blues and not one.
    #[test]
    fn the_design_colours_hold() {
        let src = app_color_slint();
        let table = arms(function_body(&src, "hue-for-app"), "id");
        let hue = |id: &str| {
            table
                .iter()
                .find(|(k, _)| k == id)
                .map(|(_, h)| h.as_str())
                .unwrap_or_else(|| panic!("{id} has no colour"))
        };
        assert_eq!(hue("files"), "sky");
        assert_eq!(hue("email"), "blue");
        assert_eq!(hue("calendar"), "red");
        assert_eq!(hue("notes"), "amber");
        assert_eq!(hue("terminal"), "green");
        assert_eq!(hue("browser"), "teal");
        assert_eq!(hue("studio"), "violet");
    }
}

fn populate_grid(ui: &App, installed: &Arc<Vec<DesktopEntry>>, query: &str, category: &str) {
    let query_lower = query.to_lowercase();
    let apps: Vec<AppGridItem> = installed
        .iter()
        .filter(|entry| match category {
            "all" => true,
            "pinned" => super::pins::is_pinned(&entry.app_id),
            other => icons::category_id(&entry.categories) == other,
        })
        .filter(|entry| {
            if query_lower.is_empty() {
                return true;
            }
            entry.name.to_lowercase().contains(&query_lower)
                || entry.app_id.to_lowercase().contains(&query_lower)
                || entry.categories.to_lowercase().contains(&query_lower)
                || entry.comment.to_lowercase().contains(&query_lower)
        })
        .map(|entry| {
            let icon = icons::resolve(&entry.icon);
            AppGridItem {
                app_id: entry.app_id.clone().into(),
                name: entry.name.clone().into(),
                icon_char: entry.icon_char.clone().into(),
                icon_id: icon_id_for(&entry.app_id).into(),
                has_icon: icon.is_some(),
                icon: icon.unwrap_or_default(),
                category: SharedString::from(icons::category_id(&entry.categories)),
                pinned: super::pins::is_pinned(&entry.app_id),
                pinnable: super::pins::is_pinnable(&entry.app_id),
            }
        })
        .collect();
    ui.set_grid_apps(ModelRc::new(VecModel::from(apps)));
}
