//! The library: characters and games as plain files under one directory.
//!
//! `characters/<slug>.json` holds a validated CharacterSpec. `games/<slug>/` holds
//! `spec.json`, the compiled `index.html`, and — once the verifier has run —
//! `verify.json` and `screenshot.png`. Plain files in the ordinary data directory
//! because a built game is a deliverable a person can copy anywhere and open by
//! double-clicking; nothing here needs a database, and everything survives a
//! reinstall of the app.
//!
//! Deleting a game moves it to the freedesktop Trash, where the desktop's Files
//! app can bring it back. That is why `delete` is graded recoverable-standard
//! rather than dangerous on the control surface.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::Local;
use serde_json::Value;

use crate::compile;
use crate::spec::{parse_character, parse_game, CharacterSpec, GameSpec, ResolvedGame};

/// Where the library lives by default: beside every other Yantrik app's data.
fn default_root() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".local/share/yantrik/arcade")
}

/// A character as the list shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct CharacterEntry {
    pub slug: String,
    pub name: String,
    pub archetype: String,
}

/// A game as the list shows it: what exists on disk, not what the spec promises.
#[derive(Debug, Clone, PartialEq)]
pub struct GameEntry {
    pub slug: String,
    pub title: String,
    pub built: bool,
    pub last_build: Option<String>,
    /// One line out of verify.json, e.g. "passed all 7 gates" or the first failure.
    pub verification: Option<String>,
}

pub struct Library {
    root: PathBuf,
    /// Tests point this at a pretend Trash; in the app it is None and the real
    /// freedesktop directories are resolved at delete time. An injected field
    /// rather than an env var because unit tests share one process and mutating
    /// XDG_DATA_HOME would race.
    trash_override: Option<PathBuf>,
}

impl Library {
    /// The real library, honouring `YANTRIK_ARCADE_DIR` so tests (and the CLI)
    /// can point the app somewhere disposable.
    pub fn open() -> Library {
        let root = match std::env::var("YANTRIK_ARCADE_DIR") {
            Ok(dir) if !dir.is_empty() => PathBuf::from(dir),
            _ => default_root(),
        };
        Library { root, trash_override: None }
    }

    /// A library rooted at an explicit directory — what every test here uses.
    /// Test-only on purpose: the shipping paths all go through `open()` so there
    /// is exactly one way to find the real library.
    #[cfg(test)]
    pub fn at(root: impl Into<PathBuf>) -> Library {
        Library { root: root.into(), trash_override: None }
    }

    /// Send deletes to a pretend Trash data directory instead of the real one.
    #[cfg(test)]
    pub fn with_trash_at(mut self, data_dir: impl Into<PathBuf>) -> Library {
        self.trash_override = Some(data_dir.into());
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn characters_dir(&self) -> PathBuf {
        self.root.join("characters")
    }

    pub fn game_dir(&self, slug: &str) -> PathBuf {
        self.root.join("games").join(slug)
    }

    pub fn game_html(&self, slug: &str) -> PathBuf {
        self.game_dir(slug).join("index.html")
    }

    pub fn game_spec_path(&self, slug: &str) -> PathBuf {
        self.game_dir(slug).join("spec.json")
    }

    pub fn verify_path(&self, slug: &str) -> PathBuf {
        self.game_dir(slug).join("verify.json")
    }

    pub fn screenshot_path(&self, slug: &str) -> PathBuf {
        self.game_dir(slug).join("screenshot.png")
    }

    // ── Characters ────────────────────────────────────────────────

    /// Validate and save a character spec. Refusals are the validator's sentences.
    pub fn save_character(&self, text: &str) -> Result<CharacterEntry, String> {
        let spec = parse_character(text)?;
        let slug = slugify(&spec.name);
        if slug.is_empty() {
            return Err(format!(
                "`name` ({:?}) leaves nothing usable as a filename; use letters, digits or spaces.",
                spec.name
            ));
        }
        fs::create_dir_all(self.characters_dir()).map_err(|e| format!("cannot create the characters directory: {e}"))?;
        let path = self.characters_dir().join(format!("{slug}.json"));
        if path.exists() {
            return Err(format!(
                "A character called {:?} is already saved; delete it first or pick another name.",
                spec.name
            ));
        }
        let json = serde_json::to_string_pretty(&spec).expect("a parsed spec re-serializes");
        fs::write(&path, json).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        Ok(CharacterEntry {
            slug,
            name: spec.name.clone(),
            archetype: spec.archetype.as_str().to_string(),
        })
    }

    /// Find a saved character by its exact name or by slug, case-insensitively.
    pub fn load_character(&self, name_or_slug: &str) -> Result<CharacterSpec, String> {
        let want = name_or_slug.trim().to_lowercase();
        let dir = self.characters_dir();
        let entries = fs::read_dir(&dir).map_err(|_| {
            format!("No character called {:?} is saved: the character library is empty.", name_or_slug)
        })?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let slug = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let Ok(text) = fs::read_to_string(&path) else { continue };
            let Ok(spec) = serde_json::from_str::<CharacterSpec>(&text) else { continue };
            if slug.eq_ignore_ascii_case(&want) || spec.name.to_lowercase() == want {
                return Ok(spec);
            }
        }
        Err(format!(
            "No character called {:?} is saved; `describe` lists the ones that are.",
            name_or_slug
        ))
    }

    pub fn list_characters(&self) -> Vec<CharacterEntry> {
        let mut out = Vec::new();
        if let Ok(entries) = fs::read_dir(self.characters_dir()) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                let slug = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
                if let Ok(text) = fs::read_to_string(&path) {
                    if let Ok(spec) = serde_json::from_str::<CharacterSpec>(&text) {
                        out.push(CharacterEntry {
                            slug,
                            name: spec.name,
                            archetype: spec.archetype.as_str().to_string(),
                        });
                    }
                }
            }
        }
        out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        out
    }

    // ── Games ─────────────────────────────────────────────────────

    /// Validate a game spec and file it. Building is a separate step so a spec can
    /// be saved, looked at, and edited before anything is compiled.
    pub fn save_game(&self, text: &str) -> Result<GameEntry, String> {
        let spec = parse_game(text)?;
        let slug = slugify(&spec.title);
        if slug.is_empty() {
            return Err(format!(
                "`title` ({:?}) leaves nothing usable as a filename; use letters, digits or spaces.",
                spec.title
            ));
        }
        let dir = self.game_dir(&slug);
        if dir.exists() {
            return Err(format!(
                "A game called {:?} already exists; delete it first or pick another title.",
                spec.title
            ));
        }
        fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let json = serde_json::to_string_pretty(&spec).expect("a parsed spec re-serializes");
        fs::write(self.game_spec_path(&slug), json)
            .map_err(|e| format!("cannot write the spec: {e}"))?;
        Ok(self.describe_game(&slug, &spec))
    }

    /// Load a filed spec by title or slug.
    pub fn load_game_spec(&self, title_or_slug: &str) -> Result<(String, GameSpec), String> {
        let want = title_or_slug.trim().to_lowercase();
        let want_slug = slugify(title_or_slug);
        let games_dir = self.root.join("games");
        let entries = fs::read_dir(&games_dir).map_err(|_| {
            format!("No game called {:?} exists: the library is empty.", title_or_slug)
        })?;
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let slug = path.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string();
            let spec_path = path.join("spec.json");
            let Ok(text) = fs::read_to_string(&spec_path) else { continue };
            let Ok(spec) = serde_json::from_str::<GameSpec>(&text) else { continue };
            if slug.eq_ignore_ascii_case(&want)
                || slug == want_slug
                || spec.title.to_lowercase() == want
            {
                return Ok((slug, spec));
            }
        }
        Err(format!(
            "No game called {:?} exists; `describe` lists the ones that do.",
            title_or_slug
        ))
    }

    /// Compile a filed game into its index.html. Resolves a by-name character
    /// against the character library at build time.
    pub fn build_game(&self, title_or_slug: &str) -> Result<(String, PathBuf), String> {
        let (slug, spec) = self.load_game_spec(title_or_slug)?;
        let lib = self;
        let resolved = ResolvedGame::resolve(spec, &|name| lib.load_character(name))?;
        let html = compile::compile(&resolved);
        let out = self.game_html(&slug);
        fs::write(&out, html).map_err(|e| format!("cannot write {}: {e}", out.display()))?;
        // A fresh build invalidates the old verdict: it was about the previous file.
        let _ = fs::remove_file(self.verify_path(&slug));
        Ok((slug, out))
    }

    pub fn list_games(&self) -> Vec<GameEntry> {
        let mut out = Vec::new();
        if let Ok(entries) = fs::read_dir(self.root.join("games")) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let Some(slug) = path.file_name().and_then(|s| s.to_str()).map(String::from) else {
                    continue;
                };
                let Ok(text) = fs::read_to_string(path.join("spec.json")) else { continue };
                let Ok(spec) = serde_json::from_str::<GameSpec>(&text) else { continue };
                out.push(self.describe_game(&slug, &spec));
            }
        }
        out.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
        out
    }

    /// One game's row: existence facts straight off the disk.
    fn describe_game(&self, slug: &str, spec: &GameSpec) -> GameEntry {
        let html = self.game_html(slug);
        let built = html.exists();
        let last_build = html
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .map(|t| {
                let dt: chrono::DateTime<Local> = t.into();
                dt.format("%Y-%m-%d %H:%M").to_string()
            });
        let verification = fs::read_to_string(self.verify_path(slug))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .map(|v| verify_summary_line(&v));
        GameEntry {
            slug: slug.to_string(),
            title: spec.title.clone(),
            built,
            last_build,
            verification,
        }
    }

    /// Record a verification result beside the game.
    pub fn write_verify(&self, slug: &str, report: &Value) -> Result<(), String> {
        let path = self.verify_path(slug);
        let json = serde_json::to_string_pretty(report).map_err(|e| e.to_string())?;
        fs::write(&path, json).map_err(|e| format!("cannot write {}: {e}", path.display()))
    }

    /// Move a game to the freedesktop Trash. Recoverable from Files, which is
    /// the whole reason this is not `fs::remove_dir_all`.
    pub fn delete_game(&self, title_or_slug: &str) -> Result<String, String> {
        let (slug, spec) = self.load_game_spec(title_or_slug)?;
        let dir = self.game_dir(&slug);
        let trash = self.trash_dirs();
        fs::create_dir_all(&trash.files).map_err(|e| format!("cannot reach the Trash: {e}"))?;
        fs::create_dir_all(&trash.info).map_err(|e| format!("cannot reach the Trash: {e}"))?;

        let target = unique_path(&trash.files.join(format!("yantrik-arcade-{slug}")));
        // rename is atomic on one filesystem; the copy fallback covers a library
        // that lives somewhere else (an override on /mnt/c, say).
        if fs::rename(&dir, &target).is_err() {
            copy_dir(&dir, &target)?;
            fs::remove_dir_all(&dir).map_err(|e| format!("cannot remove the original: {e}"))?;
        }
        let info = format!(
            "[Trash Info]\nPath={}\nDeletionDate={}\n",
            percent_encode(&dir.display().to_string()),
            Local::now().format("%Y-%m-%dT%H:%M:%S")
        );
        let info_path = trash
            .info
            .join(target.file_name().and_then(|n| n.to_str()).unwrap_or("game"));
        let _ = fs::write(info_path.with_extension("trashinfo"), info);
        Ok(format!("{:?} moved to the Trash", spec.title))
    }
}

/// One line out of a verify.json report, for the games list and `describe`.
pub fn verify_summary_line(report: &Value) -> String {
    let when = report
        .get("when")
        .and_then(|v| v.as_str())
        .unwrap_or("at an unknown time");
    let gates = report.get("gates").and_then(|v| v.as_array());
    match (report.get("passed").and_then(|v| v.as_bool()), gates) {
        (Some(true), Some(g)) => format!("verified {when}: passed all {} gates", g.len()),
        (Some(false), Some(g)) => {
            let first = g
                .iter()
                .find(|gate| gate.get("passed").and_then(|v| v.as_bool()) == Some(false));
            match first {
                Some(gate) => format!(
                    "failed {when} at {}: {}",
                    gate.get("gate").and_then(|v| v.as_str()).unwrap_or("?"),
                    gate.get("detail").and_then(|v| v.as_str()).unwrap_or("no detail")
                ),
                None => format!("failed {when}"),
            }
        }
        _ => format!("verified {when}"),
    }
}

/// A filename-safe slug from a title or name: lowercase, runs of anything else
/// collapsed to single dashes.
pub fn slugify(text: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for ch in text.trim().to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

struct TrashDirs {
    files: PathBuf,
    info: PathBuf,
}

impl Library {
    fn trash_dirs(&self) -> TrashDirs {
        // $XDG_DATA_HOME respected, per the freedesktop spec, with its default.
        let data = match &self.trash_override {
            Some(dir) => dir.clone(),
            None => std::env::var("XDG_DATA_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
                    PathBuf::from(home).join(".local/share")
                }),
        };
        TrashDirs {
            files: data.join("Trash/files"),
            info: data.join("Trash/info"),
        }
    }
}

fn unique_path(base: &Path) -> PathBuf {
    let mut candidate = base.to_path_buf();
    let mut n = 2;
    while candidate.exists() {
        candidate = base.with_file_name(format!(
            "{}.{}",
            base.file_name().and_then(|f| f.to_str()).unwrap_or("game"),
            n
        ));
        n += 1;
    }
    candidate
}

/// The `Path=` value in a .trashinfo is percent-encoded, per the spec.
fn percent_encode(path: &str) -> String {
    let mut out = String::new();
    for b in path.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    fs::create_dir_all(to).map_err(|e| format!("cannot create {}: {e}", to.display()))?;
    for entry in fs::read_dir(from).map_err(|e| format!("cannot read {}: {e}", from.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if src.is_dir() {
            copy_dir(&src, &dst)?;
        } else {
            fs::copy(&src, &dst)
                .map_err(|e| format!("cannot copy {} to {}: {e}", src.display(), dst.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static N: AtomicU32 = AtomicU32::new(0);

    fn temp_library() -> (Library, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "arcade-lib-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&dir);
        (Library::at(&dir), dir)
    }

    const PIP: &str = r##"{
        "name": "Pip",
        "archetype": "critter",
        "ears": "pointy",
        "tail": "curl",
        "palette": { "base": "#44cc88", "belly": "#f2ead8", "accent": "#ff9f43", "nose": "#2f3640", "eye": "#2f3640" }
    }"##;

    const GRUMBLE: &str = r##"{
        "name": "Grumble",
        "archetype": "brute",
        "ears": "none",
        "stance": "crouched",
        "palette": { "base": "#7a6ce0", "belly": "#d9d2f5", "accent": "#ffd23f", "nose": "#3a2f66", "eye": "#ffd23f" }
    }"##;

    fn meadow_run(character: &str) -> String {
        format!(
            r##"{{
                "title": "Meadow Run",
                "player": {{ "character": {character} }},
                "collectible": {{ "kind": "berry", "count": 5 }},
                "hazards": [{{ "kind": "wanderer", "speed": 2.0, "count": 2 }}]
            }}"##
        )
    }

    #[test]
    fn slugify_collapses_and_trims() {
        assert_eq!(slugify("Pip's Meadow Run!"), "pip-s-meadow-run");
        assert_eq!(slugify("  Spaced   Out  "), "spaced-out");
        assert_eq!(slugify("!!!"), "");
    }

    #[test]
    fn characters_round_trip() {
        let (lib, dir) = temp_library();
        let entry = lib.save_character(PIP).unwrap();
        assert_eq!(entry.slug, "pip");
        assert_eq!(entry.name, "Pip");
        let loaded = lib.load_character("Pip").unwrap();
        assert_eq!(loaded.name, "Pip");
        // Slug works too, case-insensitively.
        assert_eq!(lib.load_character("pip").unwrap().name, "Pip");
        assert_eq!(lib.load_character("PIP").unwrap().name, "Pip");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_duplicate_character_is_refused_with_a_sentence() {
        let (lib, dir) = temp_library();
        lib.save_character(PIP).unwrap();
        let err = lib.save_character(PIP).unwrap_err();
        assert!(err.contains("already saved"), "{err}");
        assert!(err.contains("Pip"), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_invalid_character_is_refused_before_touching_disk() {
        let (lib, dir) = temp_library();
        let err = lib.save_character(r##"{ "name": "X", "palette": { "base": "green", "belly": "#fff", "accent": "#000", "nose": "#111", "eye": "#222" } }"##).unwrap_err();
        assert!(err.contains("palette.base"), "{err}");
        assert!(!lib.characters_dir().exists() || lib.list_characters().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_character_says_the_library_is_empty() {
        let (lib, dir) = temp_library();
        let err = lib.load_character("Nobody").unwrap_err();
        assert!(err.contains("empty"), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn characters_list_sorted_by_name() {
        let (lib, dir) = temp_library();
        lib.save_character(PIP).unwrap();
        lib.save_character(GRUMBLE).unwrap();
        let list = lib.list_characters();
        let names: Vec<_> = list.iter().map(|c| c.name.clone()).collect();
        assert_eq!(names, vec!["Grumble", "Pip"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn games_save_then_build_with_a_named_character() {
        let (lib, dir) = temp_library();
        lib.save_character(PIP).unwrap();
        let entry = lib.save_game(&meadow_run("\"Pip\"")).unwrap();
        assert_eq!(entry.slug, "meadow-run");
        assert!(!entry.built);
        let (slug, html) = lib.build_game("Meadow Run").unwrap();
        assert_eq!(slug, "meadow-run");
        assert!(html.exists());
        let text = fs::read_to_string(&html).unwrap();
        assert!(text.contains("window.ARCADE_GAME"));
        assert!(text.contains("\"Pip\""));
        // The list now shows it built, with a timestamp.
        let games = lib.list_games();
        assert_eq!(games.len(), 1);
        assert!(games[0].built);
        assert!(games[0].last_build.is_some());
        assert!(games[0].verification.is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn building_with_an_inline_character_needs_no_library() {
        let (lib, dir) = temp_library();
        lib.save_game(&meadow_run(PIP)).unwrap();
        let (_, html) = lib.build_game("meadow-run").unwrap();
        assert!(html.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn building_with_a_missing_character_names_it() {
        let (lib, dir) = temp_library();
        lib.save_game(&meadow_run("\"Ghost\"")).unwrap();
        let err = lib.build_game("meadow-run").unwrap_err();
        assert!(err.contains("Ghost"), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_duplicate_game_is_refused_with_a_sentence() {
        let (lib, dir) = temp_library();
        lib.save_game(&meadow_run(PIP)).unwrap();
        let err = lib.save_game(&meadow_run(PIP)).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rebuild_clears_a_stale_verdict() {
        let (lib, dir) = temp_library();
        lib.save_game(&meadow_run(PIP)).unwrap();
        lib.build_game("meadow-run").unwrap();
        lib.write_verify("meadow-run", &serde_json::json!({
            "passed": true, "when": "2026-09-22 10:00",
            "gates": [{ "gate": "boots", "passed": true }]
        })).unwrap();
        assert!(lib.list_games()[0].verification.as_ref().unwrap().contains("passed all 1 gates"));
        lib.build_game("meadow-run").unwrap();
        assert!(lib.list_games()[0].verification.is_none(), "a fresh build has no verdict yet");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_summary_names_the_first_failed_gate() {
        let report = serde_json::json!({
            "passed": false, "when": "2026-09-22 10:00",
            "gates": [
                { "gate": "boots", "passed": true },
                { "gate": "bot_reaches_win", "passed": false, "detail": "bot collected 3 of 10 in 30 s" }
            ]
        });
        let line = verify_summary_line(&report);
        assert!(line.contains("bot_reaches_win"), "{line}");
        assert!(line.contains("3 of 10"), "{line}");
    }

    #[test]
    fn delete_moves_the_game_to_the_trash() {
        let (lib, dir) = temp_library();
        // A private pretend-XDG so the test does not touch the real Trash.
        let fake_data = dir.join("home/data");
        let lib = lib.with_trash_at(&fake_data);
        lib.save_game(&meadow_run(PIP)).unwrap();
        lib.build_game("meadow-run").unwrap();
        let msg = lib.delete_game("Meadow Run").unwrap();
        assert!(msg.contains("Trash"), "{msg}");
        assert!(!lib.game_dir("meadow-run").exists());
        let moved: Vec<_> = fs::read_dir(fake_data.join("Trash/files")).unwrap().flatten().collect();
        assert_eq!(moved.len(), 1);
        assert!(moved[0].path().join("index.html").exists());
        let infos: Vec<_> = fs::read_dir(fake_data.join("Trash/info")).unwrap().flatten().collect();
        assert_eq!(infos.len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn percent_encoding_keeps_the_trash_info_legal() {
        assert_eq!(percent_encode("/home/me/My Games"), "/home/me/My%20Games");
    }

    #[test]
    fn deleting_an_unknown_game_refuses_with_a_sentence() {
        let (lib, dir) = temp_library();
        let err = lib.delete_game("nope").unwrap_err();
        assert!(err.contains("nope"), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }
}
