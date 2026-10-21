//! Every app id, on the framework or still to be ported, against every other component's file.

use std::path::{Path, PathBuf};

use super::*;
use crate::paths::{Paths, APPS_DIR, DEFAULT_PROFILE, SETTINGS_FILE};

/// The repository, from this crate's manifest (`frameworks/yantrik-app`).
fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The ids the apps in `apps/` use or will use: each folder's name, and every id its sources
/// declare (`const ID`, `APP_ID`, an instance `claim`, `App::new`), plus the ports to come.
fn app_ids() -> Vec<String> {
    let mut ids: Vec<String> = [
        // The framework apps today.
        "weather", "notes", "email", "containers", "image-viewer", "network", "system-monitor",
        // The ports to come, by folder and by the ids they use now.
        "calendar", "music-player", "music", "terminal", "browser", "spreadsheet", "document-editor",
        "documents", "text-editor", "editor", "snippet-manager", "snippets", "download-manager",
        "downloads", "desktop-files", "presentation", "studio", "arcade", "blender",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let apps = repo().join("apps");
    let folders = std::fs::read_dir(&apps).unwrap_or_else(|e| panic!("{}: {e}", apps.display()));
    for folder in folders.flatten().filter(|f| f.path().is_dir()) {
        ids.push(folder.file_name().to_string_lossy().to_string());
        for source in ["src/lib.rs", "src/main.rs"] {
            let Ok(text) = std::fs::read_to_string(folder.path().join(source)) else { continue };
            for marker in ["const ID: &'static str = \"", "APP_ID: &str = \"", "claim(\"", "App::new(\""] {
                for (at, _) in text.match_indices(marker) {
                    let rest = &text[at + marker.len()..];
                    if let Some(end) = rest.find('"') {
                        ids.push(rest[..end].to_string());
                    }
                }
            }
        }
    }
    ids.sort();
    ids.dedup();
    ids.retain(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    ids
}

fn root_dir(root: Root) -> &'static str {
    match root {
        Root::Config => "config",
        Root::Data => "data",
        Root::State => "state",
        Root::Cache => "cache",
    }
}

fn entry_path(base: &Path, e: &Entry) -> PathBuf {
    base.join(root_dir(e.root)).join("yantrik").join(e.name)
}

#[test]
fn the_scan_finds_the_framework_apps() {
    let ids = app_ids();
    for id in ["weather", "notes", "email", "containers", "image-viewer", "network", "system-monitor"] {
        assert!(ids.iter().any(|i| i == id), "{id} in {ids:?}");
    }
}

/// The fault this guards: under `yantrik/<id>.json`, Email's settings were email-service's
/// account file. No app's settings may be, or sit inside, any other component's entry.
#[test]
fn no_apps_settings_file_is_another_components() {
    let base = Path::new("/r");
    assert!(is_others(Root::Config, "email.json", "email"), "the collision this rule removed");
    for id in app_ids() {
        let p = Paths::under(base, &id, DEFAULT_PROFILE);
        let expected = base.join("config/yantrik").join(APPS_DIR).join(&id).join(SETTINGS_FILE);
        assert_eq!(p.config_file(), expected);
        for e in ENTRIES {
            let other = entry_path(base, e);
            assert!(
                !p.config_file().starts_with(&other) && !p.imported_record().starts_with(&other),
                "{id}'s settings {} land on {} ({})",
                p.config_file().display(),
                other.display(),
                e.owner
            );
        }
    }
}

/// An app's data, state and cache folders are `<root>/yantrik/<id>/`: one only meets another
/// component's entry where that entry names the app as sharing it (Notes and its vault).
#[test]
fn no_apps_folders_are_another_components_unless_shared_on_purpose() {
    let base = Path::new("/r");
    for id in app_ids() {
        let p = Paths::under(base, &id, DEFAULT_PROFILE);
        for (dir, root) in [(p.data_dir(), Root::Data), (p.state_dir(), Root::State), (p.cache_dir(), Root::Cache)] {
            for e in ENTRIES.iter().filter(|e| e.root == root && e.app != Some(id.as_str())) {
                let other = entry_path(base, e);
                assert!(
                    !dir.starts_with(&other) && !other.starts_with(dir),
                    "{id}'s {} meets {} ({})",
                    dir.display(),
                    other.display(),
                    e.owner
                );
            }
        }
    }
}

#[test]
fn the_apps_directory_is_the_frameworks_alone_and_the_table_is_one_entry_a_name() {
    for e in ENTRIES {
        assert_ne!(e.name, APPS_DIR, "{} keeps `{APPS_DIR}`: the framework's own", e.owner);
        assert!(!e.name.contains('/') && !e.name.is_empty(), "{}: first component only", e.name);
        let twins = ENTRIES.iter().filter(|o| o.root == e.root && o.name == e.name).count();
        assert_eq!(twins, 1, "{:?} {} twice", e.root, e.name);
    }
}

/// Nothing but this crate writes under `yantrik/apps/`: a component that started to would put a
/// file where an app's settings go.
#[test]
fn no_other_component_names_the_apps_directory() {
    let repo = repo();
    let mut found = Vec::new();
    for top in ["services", "crates", "deploy", "harnesses", "packaging", "scripts", "apps"] {
        scan(&repo.join(top), &mut found);
    }
    assert!(found.is_empty(), "these name `yantrik/apps/`, the framework's own directory: {found:?}");
}

fn scan(dir: &Path, found: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if !matches!(name.as_str(), "target" | "node_modules" | ".git") {
                scan(&path, found);
            }
            continue;
        }
        let text_like = ["rs", "sh", "py", "yaml", "yml", "toml", "json", "service", "desktop"]
            .iter()
            .any(|ext| path.extension().is_some_and(|e| e == *ext))
            || path.extension().is_none();
        if !text_like {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        // Code, not prose: a comment may say where the settings are.
        let code: String = text
            .lines()
            .filter(|l| !["//", "#", "*", "/*"].iter().any(|c| l.trim_start().starts_with(c)))
            .collect::<Vec<_>>()
            .join("
");
        let text = code;
        let squeezed: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        if text.contains("yantrik/apps/") || text.contains("yantrik/apps\"") || squeezed.contains(r#"join("yantrik").join("apps")"#) {
            found.push(path.display().to_string());
        }
    }
}
