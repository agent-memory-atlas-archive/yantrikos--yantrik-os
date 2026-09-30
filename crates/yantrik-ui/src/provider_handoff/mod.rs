//! Giving a harness one of the person's saved providers, through that harness's own config.
//!
//! The rule this keeps (Pranab, 2026-09-15): each harness keeps its own settings — the Mind, Hermes,
//! Pi, OpenClaw, DeepSeek — and the OS provides the interface to reach them, not a copy of their
//! configuration. The harness protocol still carries no endpoint, model or key. What this adds is
//! one explicit action a person takes on one harness at a time: "use this provider", which writes
//! into that harness's own file the way the person would by hand, after a card that names every
//! file it will touch and whether the harness restarts.
//!
//! What it guarantees:
//! - **Nothing without a click.** No write at boot, on attach, or when a provider changes.
//! - **Keys go only where the person pointed them**, in a file at mode 600 that the harness
//!   already reads, written through a temporary file and a rename so a half-written file never
//!   exists. The card, the marker and every log line carry the provider's name and the file's
//!   path, never the key.
//! - **The person's own file is kept.** The first time a file is written it is copied aside, and
//!   Revert puts that copy back (or removes the file, if there was none). Assigning again later
//!   does not overwrite the original copy.
//! - **The row can say what it did**, from a marker in ~/.config/yantrik/handoff that names the
//!   provider, the model and the files — and no key.
//!
//! One module per harness under this one; each knows only its own file format.

mod deepseek;

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::wire::settings::ProviderStoreEntry;

/// One file a plan writes.
pub(crate) struct Write {
    pub path: PathBuf,
    /// The whole new file. It can hold a key, so it is never printed: see the Debug below.
    pub content: String,
    /// What this file is, for the card: "DeepSeek's settings, with the key".
    pub what: String,
}

impl std::fmt::Debug for Write {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Write").field("path", &self.path).field("what", &self.what).finish_non_exhaustive()
    }
}

/// What assigning a provider to a harness will do, before anything is done.
#[derive(Debug)]
pub(crate) struct Plan {
    pub harness: String,
    pub harness_name: String,
    pub provider_id: String,
    pub provider_name: String,
    pub model: String,
    pub writes: Vec<Write>,
    /// The user unit to restart so the harness reads its new settings, when it is running.
    pub restart: Option<String>,
}

impl Plan {
    /// The card's text: which provider and model, every file by path and what it is, and the
    /// restart. Never the key.
    pub fn card(&self, home: &Path) -> String {
        let mut lines = vec![format!("{} will use {} with {}.", self.harness_name, self.provider_name, self.model)];
        lines.push(String::new());
        lines.push("This writes:".into());
        for w in &self.writes {
            lines.push(format!("  • {} — {}", display(&w.path, home), w.what));
        }
        lines.push(String::new());
        lines.push("Your current file is kept, and Revert puts it back.".into());
        if let Some(unit) = &self.restart {
            lines.push(format!("{} restarts if it is running.", unit.trim_end_matches(".service")));
        }
        lines.join("\n")
    }
}

/// What the row reads: which provider a harness was given, and what to undo. No key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Marker {
    pub provider_id: String,
    pub provider_name: String,
    pub model: String,
    pub at: String,
    pub files: Vec<Touched>,
    #[serde(default)]
    pub restart: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Touched {
    pub path: PathBuf,
    /// The person's own file, kept aside the first time; `None` when there was none.
    pub backup: Option<PathBuf>,
}

/// A harness that can be given a provider.
pub(crate) trait Handoff: Send + Sync {
    fn harness(&self) -> &'static str;
    fn name(&self) -> &'static str;
    fn plan(&self, home: &Path, provider: &ProviderStoreEntry) -> Result<Plan, String>;
}

/// The harnesses that know how to take a provider, by id.
pub(crate) fn adapter_for(harness: &str) -> Option<&'static dyn Handoff> {
    match harness {
        "deepseek" => Some(&deepseek::DeepSeek),
        _ => None,
    }
}

pub(crate) fn marker_path(home: &Path, harness: &str) -> PathBuf {
    home.join(".config/yantrik/handoff").join(format!("{harness}.json"))
}

pub(crate) fn marker(home: &Path, harness: &str) -> Option<Marker> {
    let text = std::fs::read_to_string(marker_path(home, harness)).ok()?;
    serde_json::from_str(&text).ok()
}

/// The row's line: "Provider: NVIDIA NIM · nvidia/nemotron-…" once assigned, else its own.
pub(crate) fn row_line(home: &Path, harness: &str) -> String {
    match (adapter_for(harness), marker(home, harness)) {
        (_, Some(m)) => format!("Provider: {} · {} (from your saved providers)", m.provider_name, m.model),
        (Some(_), None) => "Provider: its own settings".into(),
        (None, None) => String::new(),
    }
}

/// Do what the plan says: keep each file the person had, write the new one at 600, leave the
/// marker, restart the harness if it is running.
pub(crate) fn apply(home: &Path, plan: &Plan) -> Result<Marker, String> {
    let earlier = marker(home, &plan.harness);
    let mut files = Vec::new();
    for w in &plan.writes {
        // The original is kept once. A second assignment must not take the person's own file
        // and replace it with the first assignment's.
        let kept = earlier.as_ref().and_then(|m| m.files.iter().find(|t| t.path == w.path)).map(|t| t.backup.clone());
        let backup = match kept {
            Some(b) => b,
            None if w.path.exists() => {
                let b = backup_path(&w.path);
                std::fs::copy(&w.path, &b).map_err(|e| format!("could not keep a copy of {}: {e}", w.path.display()))?;
                set_private(&b)?;
                Some(b)
            }
            None => None,
        };
        write_private(&w.path, &w.content)?;
        files.push(Touched { path: w.path.clone(), backup });
    }
    let m = Marker {
        provider_id: plan.provider_id.clone(),
        provider_name: plan.provider_name.clone(),
        model: plan.model.clone(),
        at: chrono::Utc::now().to_rfc3339(),
        files,
        restart: plan.restart.clone(),
    };
    let text = serde_json::to_string_pretty(&m).map_err(|e| e.to_string())?;
    write_private(&marker_path(home, &plan.harness), &text)?;
    if let Some(unit) = &plan.restart {
        try_restart(unit);
    }
    tracing::info!(harness = %plan.harness, provider = %plan.provider_name, model = %plan.model, "gave a harness a provider");
    Ok(m)
}

/// Put back what the person had: each kept file over the one written, or the written file
/// removed when there was none before. Then restart the harness if it is running.
pub(crate) fn revert(home: &Path, harness: &str) -> Result<(), String> {
    let m = marker(home, harness).ok_or_else(|| format!("{harness} was not given a provider here"))?;
    for t in &m.files {
        match &t.backup {
            Some(b) => std::fs::rename(b, &t.path).map_err(|e| format!("could not put back {}: {e}", t.path.display()))?,
            None => {
                let _ = std::fs::remove_file(&t.path);
            }
        }
    }
    std::fs::remove_file(marker_path(home, harness)).map_err(|e| e.to_string())?;
    if let Some(unit) = &m.restart {
        try_restart(unit);
    }
    tracing::info!(harness = %harness, "put a harness's own settings back");
    Ok(())
}

fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".before-yantrik");
    path.with_file_name(name)
}

/// Write `content` to `path` at mode 600, through a temporary file in the same directory and a
/// rename: the file is never world-readable and never half-written.
fn write_private(path: &Path, content: &str) -> Result<(), String> {
    let dir = path.parent().ok_or("no directory")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("could not make {}: {e}", dir.display()))?;
    let tmp = dir.join(format!(".{}.yantrik-tmp", path.file_name().and_then(|n| n.to_str()).unwrap_or("file")));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut f = options.open(&tmp).map_err(|e| format!("could not write {}: {e}", path.display()))?;
    f.write_all(content.as_bytes()).and_then(|_| f.sync_all()).map_err(|e| format!("could not write {}: {e}", path.display()))?;
    drop(f);
    set_private(&tmp)?;
    std::fs::rename(&tmp, path).map_err(|e| format!("could not write {}: {e}", path.display()))
}

fn set_private(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    }
    let _ = path;
    Ok(())
}

fn try_restart(unit: &str) {
    let _ = std::process::Command::new("systemctl").args(["--user", "try-restart", unit]).status();
}

fn display(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests;
