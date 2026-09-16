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

    // Rescan every time the launcher opens.
    //
    // The catalogue used to be scanned once at startup, so an app installed while the shell was
    // running simply did not exist: not in the launcher, not in the Lens, not to `open_app`.
    // The launcher opening is exactly the moment someone who has just installed something goes
    // looking for it, and a scan is a few directories of small ini files -- cheap enough to do
    // on a keystroke and far cheaper than being wrong.
    {
        let catalogue = catalogue.clone();
        let weak = ui.as_weak();
        ui.on_app_grid_opened(move || {
            let count = catalogue.refresh();
            tracing::debug!(apps = count, "rescanned installed apps for the launcher");
            if let Some(ui) = weak.upgrade() {
                let apps = catalogue.get();
                ui.set_grid_categories(ModelRc::new(VecModel::from(categories_for(&apps))));
                populate_grid(&ui, &apps, "", "all");
            }
        });
    }

    // The two filters compose: whichever one changes, the other is re-applied from here.
    let query = Rc::new(RefCell::new(String::new()));
    let category = Rc::new(RefCell::new(String::from("all")));

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
fn icon_id_for(app_id: &str) -> String {
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
            "skills", "snippets", "sysmonitor", "image",
        ];
        const SHIPPED: &[&str] = &[
            "yantrik-calendar", "yantrik-container-manager", "yantrik-document-editor",
            "yantrik-download-manager", "yantrik-email", "yantrik-image-viewer",
            "yantrik-music-player", "yantrik-network-manager", "yantrik-notes",
            "yantrik-presentation", "yantrik-snippet-manager", "yantrik-spreadsheet",
            "yantrik-system-monitor", "yantrik-terminal", "yantrik-text-editor",
            "yantrik-weather",
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

fn populate_grid(ui: &App, installed: &Arc<Vec<DesktopEntry>>, query: &str, category: &str) {
    let query_lower = query.to_lowercase();
    let apps: Vec<AppGridItem> = installed
        .iter()
        .filter(|entry| category == "all" || icons::category_id(&entry.categories) == category)
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
            }
        })
        .collect();
    ui.set_grid_apps(ModelRc::new(VecModel::from(apps)));
}
