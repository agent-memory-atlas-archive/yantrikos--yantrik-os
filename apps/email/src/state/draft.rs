//! The draft that used to be lost: the unsent message, kept as one file.

use std::path::Path;

/// A message that was being written when the composer was closed.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Draft {
    #[serde(default)]
    pub to: String,
    #[serde(default)]
    pub cc: String,
    #[serde(default)]
    pub bcc: String,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub body: String,
}

impl Draft {
    /// Nothing was typed. A draft of nothing is not kept, so reopening the composer after
    /// cancelling an empty one does not restore an empty one.
    pub fn is_empty(&self) -> bool {
        self.to.trim().is_empty()
            && self.cc.trim().is_empty()
            && self.bcc.trim().is_empty()
            && self.subject.trim().is_empty()
            && self.body.trim().is_empty()
    }
}

/// Write the draft whole: beside the file and renamed over it, so a crash in the middle leaves
/// the previous draft rather than half of this one, and readable only by its owner, because an
/// unsent message is private mail.
pub fn save_draft(path: &Path, draft: &Draft) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("could not make {}: {e}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(draft)
        .map_err(|e| format!("could not write the draft: {e}"))?;
    let beside = path.with_extension("json.part");
    let written = write_private(&beside, text.as_bytes()).and_then(|()| std::fs::rename(&beside, path));
    written.map_err(|e| {
        let _ = std::fs::remove_file(&beside);
        format!("could not write {}: {e}", path.display())
    })
}

/// A new file with these bytes, synced, mode 0600 where the platform has modes.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    // A file left by an earlier attempt could carry other permissions; the mode applies only to
    // a file this call creates.
    let _ = std::fs::remove_file(path);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// The kept draft, or `None` when there is not one. A file that is there and unreadable is an
/// error rather than a shrug: a draft silently treated as absent is the loss this exists to stop.
pub fn load_draft(path: &Path) -> Result<Option<Draft>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("could not read {}: {e}", path.display())),
    };
    if text.trim().is_empty() {
        return Ok(None);
    }
    let draft: Draft = serde_json::from_str(&text)
        .map_err(|e| format!("{} is not a draft: {e}", path.display()))?;
    if draft.is_empty() {
        Ok(None)
    } else {
        Ok(Some(draft))
    }
}

pub fn clear_draft(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("could not remove {}: {e}", path.display())),
    }
}
