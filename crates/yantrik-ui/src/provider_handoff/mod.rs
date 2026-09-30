//! Giving a harness one of the person's saved providers, through that harness's own config.
//!
//! The rule this keeps (Pranab, 2026-09-15): each harness keeps its own settings — the Mind, Hermes,
//! Pi, OpenClaw, DeepSeek — and the OS provides the interface to reach them, not a copy of their
//! configuration. The harness protocol still carries no endpoint, model or key. What this adds is
//! one explicit action a person takes on one harness at a time: "use this provider", which writes
//! into that harness's own file the way the person would by hand, after a card that names the
//! provider, the address its key goes to, every file it will touch and whether the harness restarts.
//!
//! What it guarantees:
//! - **Nothing without a click.** No write at boot, on attach, or when a provider changes.
//! - **Keys go only where the person pointed them**, in a file at mode 600 that the harness
//!   already reads, written through `crate::private_file` (a fresh 600 temp file, no link
//!   followed, published in one step). A provider the catalogue knows sends its key only to its
//!   own address (`pinned_base`). The card, the marker and every log line carry the provider's
//!   name, the address and the file's path, never the key.
//! - **The person's own file is kept.** The first time a file is written it is copied aside, and
//!   that copy is never replaced — not by a second assignment, not after an attempt that failed
//!   halfway. Revert puts it back, or removes the written file when there was none.
//! - **Revert touches only the adapter's own files**, whatever the marker says, and restarts only
//!   the adapter's own unit.
//!
//! One module per harness under this one; each knows only its own file format.

mod deepseek;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::private_file::{self, Publish};
use crate::wire::settings::ProviderStoreEntry;

/// One file a plan writes.
pub(crate) struct Write {
    pub path: PathBuf,
    /// The whole new file. It can hold a key, so it is never printed: see the Debug below.
    pub content: String,
    /// What this file is, for the card: "its address, model and key".
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
    /// The address the harness will send requests — and the key — to.
    pub destination: String,
    pub writes: Vec<Write>,
    /// The user unit to restart so the harness reads its new settings, when it is running.
    pub restart: Option<String>,
}

impl Plan {
    /// The card's text: provider, address and model, every file by path and what it is, and the
    /// restart. Never the key.
    pub fn card(&self, home: &Path) -> String {
        let mut lines = vec![format!(
            "{} will use {} at {} with {}.",
            self.harness_name,
            self.provider_name,
            host(&self.destination),
            self.model
        )];
        if self.destination.starts_with("http://") && !is_local(&self.destination) {
            lines.push("This address is plain http: the key would cross the network unencrypted.".into());
        }
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

    /// One sentence for an approval card: who gets which provider, where its key goes, which files.
    pub fn sentence(&self, home: &Path) -> String {
        let files: Vec<String> = self.writes.iter().map(|w| display(&w.path, home)).collect();
        format!(
            "{} will use {} at {} with {}: writes {} with the provider's key, and keeps your own copy for Revert.",
            self.harness_name,
            self.provider_name,
            host(&self.destination),
            self.model,
            files.join(", ")
        )
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
    /// Every file this adapter ever writes. Revert touches these and nothing else, whatever the
    /// marker says: the marker is a file too, and something else running as the person could
    /// have written it.
    fn files(&self, home: &Path) -> Vec<PathBuf>;
    /// The unit that runs it, restarted after a change. From here, never from the marker.
    fn unit(&self) -> Option<&'static str>;
    fn plan(&self, home: &Path, provider: &ProviderStoreEntry) -> Result<Plan, String>;
}

/// The harnesses that know how to take a provider, by id.
pub(crate) fn adapter_for(harness: &str) -> Option<&'static dyn Handoff> {
    match harness {
        "deepseek" => Some(&deepseek::DeepSeek),
        _ => None,
    }
}

/// Where a saved provider's key may be sent: its own address, for a provider the catalogue
/// knows. A provider saved as NVIDIA NIM whose address was later edited to another host would
/// otherwise carry an NVIDIA key there — the rule the Mind keeps too (a known provider never
/// takes a caller's base URL). Local runtimes (Ollama, llama.cpp, …) and Custom providers run
/// wherever the person put them, and the card names that address.
pub(crate) fn pinned_base(provider: &ProviderStoreEntry) -> Result<String, String> {
    let base = crate::wire::provider_models::openai_base(&provider.base_url);
    let Some(known) = yantrik_ml::ProviderDescriptor::by_id(&provider.provider_type) else {
        return Ok(base);
    };
    if known.kind == yantrik_ml::ProviderKind::Local || known.default_base_url.is_empty() {
        return Ok(base);
    }
    let own = host(known.openai_base_url());
    if host(&base) != own && host(&provider.base_url) != host(known.default_base_url) {
        return Err(format!(
            "{} is saved with the address {}, not {}'s own ({own}). Its key is only sent to its own \
             address — save it as Custom to use another one.",
            provider.name,
            host(&provider.base_url),
            known.display_name
        ));
    }
    Ok(base)
}

/// The host (and port) of a URL, without scheme, userinfo or path.
pub(crate) fn host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split(['/', '?', '#']).next().unwrap_or("").rsplit('@').next().unwrap_or("").to_string()
}

fn is_local(url: &str) -> bool {
    let h = host(url);
    let name = h.rsplit_once(':').map_or(h.as_str(), |(n, _)| n).trim_matches(['[', ']']);
    name == "localhost"
        || name.starts_with("127.")
        || name == "::1"
        || name.starts_with("192.168.")
        || name.starts_with("10.")
        || name.ends_with(".local")
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

/// One apply or revert at a time, whichever path started it.
static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Do what the plan says: keep each file the person had, write the new one at 600, leave the
/// marker, restart the harness if it is running.
pub(crate) fn apply(home: &Path, plan: &Plan) -> Result<Marker, String> {
    let _one_at_a_time = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let earlier = marker(home, &plan.harness);
    let mut files = Vec::new();
    for w in &plan.writes {
        let backup = keep_original(&w.path, earlier.as_ref())?;
        private_file::write(&w.path, w.content.as_bytes(), Publish::Replace, || Ok(()))?;
        files.push(Touched { path: w.path.clone(), backup });
    }
    let m = Marker {
        provider_id: plan.provider_id.clone(),
        provider_name: plan.provider_name.clone(),
        model: plan.model.clone(),
        at: chrono::Utc::now().to_rfc3339(),
        files,
    };
    let text = serde_json::to_string_pretty(&m).map_err(|e| e.to_string())?;
    private_file::write(&marker_path(home, &plan.harness), text.as_bytes(), Publish::Replace, || Ok(()))?;
    if let Some(unit) = &plan.restart {
        try_restart(unit);
    }
    tracing::info!(harness = %plan.harness, provider = %plan.provider_name, model = %plan.model, "gave a harness a provider");
    Ok(m)
}

/// Put back what the person had: each kept file over the one written, or the written file
/// removed when there was none before. Then restart the harness if it is running.
pub(crate) fn revert(home: &Path, harness: &str) -> Result<(), String> {
    let _one_at_a_time = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let adapter = adapter_for(harness).ok_or_else(|| format!("{harness} cannot be given a provider here"))?;
    let m = marker(home, harness).ok_or_else(|| format!("{harness} was not given a provider here"))?;
    let own = adapter.files(home);
    // Checked before anything moves: a marker naming any other file, or a backup anywhere but
    // beside the file it keeps, was not written by this code.
    for t in &m.files {
        if !own.contains(&t.path) || t.backup.as_ref().is_some_and(|b| *b != backup_path(&t.path)) {
            return Err(format!(
                "the record of what was changed names {}, which {} never writes; nothing was undone",
                t.path.display(),
                adapter.name()
            ));
        }
    }
    for t in &m.files {
        private_file::check_target(&t.path)?;
        match &t.backup {
            Some(b) => {
                private_file::check_target(b)?;
                std::fs::rename(b, &t.path).map_err(|e| format!("could not put back {}: {e}", t.path.display()))?;
            }
            None => {
                let _ = std::fs::remove_file(&t.path);
            }
        }
    }
    std::fs::remove_file(marker_path(home, harness)).map_err(|e| e.to_string())?;
    if let Some(unit) = adapter.unit() {
        try_restart(unit);
    }
    tracing::info!(harness = %harness, "put a harness's own settings back");
    Ok(())
}

/// The person's own file, kept once beside it. A copy already there is theirs — from an earlier
/// assignment, or one that failed after writing — and is never replaced, so Revert can always
/// return what they had before this code first touched it.
fn keep_original(path: &Path, earlier: Option<&Marker>) -> Result<Option<PathBuf>, String> {
    if let Some(t) = earlier.and_then(|m| m.files.iter().find(|t| t.path == path)) {
        return Ok(t.backup.clone());
    }
    let b = backup_path(path);
    if std::fs::symlink_metadata(&b).is_ok() {
        private_file::check_target(&b)?;
        return Ok(Some(b));
    }
    if std::fs::symlink_metadata(path).is_err() {
        return Ok(None);
    }
    private_file::check_target(path)?;
    let original = std::fs::read(path).map_err(|e| format!("could not keep a copy of {}: {e}", path.display()))?;
    private_file::write(&b, &original, Publish::CreateOnly, || Ok(()))?;
    Ok(Some(b))
}

pub(crate) fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".before-yantrik");
    path.with_file_name(name)
}

fn try_restart(unit: &str) {
    let _ = std::process::Command::new("systemctl").args(["--user", "try-restart", "--", unit]).status();
}

fn display(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests;
