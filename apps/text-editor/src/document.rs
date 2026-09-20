//! Bounded UTF-8 documents, conflict-aware atomic saves and private recovery.
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
pub const MAX_BYTES: usize = 1024 * 1024;
pub const MAX_TABS: usize = 8;
static SERIAL: AtomicU64 = AtomicU64::new(0);
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Document {
    pub path: Option<PathBuf>,
    pub text: String,
    pub baseline: String,
    pub recovered: bool,
    #[serde(skip)]
    pub undo: Vec<String>,
    #[serde(skip)]
    pub redo: Vec<String>,
}
impl Document {
    pub fn blank() -> Self {
        Self {
            path: None,
            text: String::new(),
            baseline: String::new(),
            recovered: false,
            undo: vec![],
            redo: vec![],
        }
    }
    pub fn snapshot(&self) -> Self {
        Self {
            path: self.path.clone(),
            text: self.text.clone(),
            baseline: self.baseline.clone(),
            recovered: self.recovered,
            undo: vec![],
            redo: vec![],
        }
    }
    pub fn edit(&mut self, text: String) {
        if text == self.text {
            return;
        }
        self.undo.push(std::mem::replace(&mut self.text, text));
        self.redo.clear();
        while self.undo.len() > 64
            || self.undo.iter().map(String::len).sum::<usize>() > 2 * MAX_BYTES
        {
            self.undo.remove(0);
        }
    }
    pub fn undo(&mut self, redo: bool) {
        let (source, target) = if redo {
            (&mut self.redo, &mut self.undo)
        } else {
            (&mut self.undo, &mut self.redo)
        };
        if let Some(text) = source.pop() {
            target.push(std::mem::replace(&mut self.text, text));
        }
    }
    pub fn dirty(&self) -> bool {
        self.recovered || self.text != self.baseline
    }
    pub fn title(&self) -> String {
        self.path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled".into())
    }
    pub fn open(path: &Path) -> Result<Self, String> {
        let path = fs::canonicalize(path).map_err(|e| format!("Cannot open: {e}"))?;
        let text = read(&path)?;
        Ok(Self {
            path: Some(path),
            baseline: text.clone(),
            text,
            recovered: false,
            undo: vec![],
            redo: vec![],
        })
    }
    pub fn save(&self, path: &Path) -> Result<Self, String> {
        validate(&self.text)?;
        if !path.is_absolute() {
            return Err("Use an absolute file path.".into());
        }
        let parent = fs::canonicalize(path.parent().ok_or("Choose a file name.")?)
            .map_err(|e| format!("Cannot open folder: {e}"))?;
        let path = parent.join(path.file_name().ok_or("Choose a file name.")?);
        let own = self.path.as_ref() == Some(&path);
        let mut permissions = None;
        if own {
            let meta = fs::symlink_metadata(&path)
                .map_err(|e| format!("Original file unavailable: {e}. Use Save As."))?;
            if !meta.is_file() || meta.nlink() > 1 {
                return Err("This path is linked or is not a regular file. Use Save As.".into());
            }
            if meta.permissions().readonly() {
                return Err("This file is read-only. Use Save As.".into());
            }
            if read(&path)? != self.baseline {
                return Err("File changed on disk. Your draft is intact; use Save As to keep both versions.".into());
            }
            permissions = Some(meta.permissions());
        } else if fs::symlink_metadata(&path).is_ok() {
            return Err(
                "That file already exists. Choose another name; nothing was overwritten.".into(),
            );
        }
        atomic_write(&path, self.text.as_bytes(), own, permissions)?;
        Ok(Self {
            path: Some(path),
            text: self.text.clone(),
            baseline: self.text.clone(),
            recovered: false,
            undo: vec![],
            redo: vec![],
        })
    }
}
pub fn validate(text: &str) -> Result<(), String> {
    if text.len() > MAX_BYTES {
        return Err("Editing is limited to 1 MiB per document.".into());
    }
    if text.bytes().filter(|b| *b == b'\n').count() > 20_000 {
        return Err("Editing is limited to 20,000 lines per document.".into());
    }
    if text
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err("This appears to be a binary file. Choose a UTF-8 text file.".into());
    }
    Ok(())
}
pub fn read(path: &Path) -> Result<String, String> {
    let f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| format!("Cannot read: {e}"))?;
    let meta = f.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("Only regular text files can be opened.".into());
    }
    if meta.len() > MAX_BYTES as u64 {
        return Err("File exceeds the 1 MiB editing limit. It was not loaded.".into());
    }
    let mut bytes = Vec::new();
    f.take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8(bytes)
        .map_err(|_| "File is not valid UTF-8; no lossy conversion was performed.")?;
    validate(&text)?;
    Ok(text)
}
fn atomic_write(
    path: &Path,
    bytes: &[u8],
    replace: bool,
    permissions: Option<fs::Permissions>,
) -> Result<(), String> {
    let parent = path.parent().ok_or("No parent folder")?;
    let temp = parent.join(format!(
        ".yantrik-editor-{}-{}.tmp",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(bytes)?;
        if let Some(p) = permissions {
            file.set_permissions(p)?;
        }
        file.sync_all()?;
        if replace {
            fs::rename(&temp, path)?;
        } else {
            // Atomic no-clobber publication, including racing creators.
            fs::hard_link(&temp, path)?;
            fs::remove_file(&temp)?;
        }
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(|e| format!("Save failed: {e}. Your draft is still open."))
}
pub fn recovery_path() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state")
        })
        .join("yantrik/editor/drafts.json")
}
pub fn recover(path: &Path) -> Result<Vec<Document>, String> {
    if !path.exists() {
        return Ok(vec![]);
    }
    let mut bytes = Vec::new();
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| e.to_string())?
        .take((MAX_BYTES * MAX_TABS * 4 + 65536) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let mut docs: Vec<Document> = serde_json::from_slice(&bytes)
        .map_err(|e| format!("Recovery file could not be read: {e}"))?;
    if docs.len() > MAX_TABS {
        return Err("Recovery contains too many documents.".into());
    }
    for d in &mut docs {
        validate(&d.text)?;
        validate(&d.baseline)?;
        d.recovered = true;
    }
    Ok(docs)
}
pub fn checkpoint(path: &Path, docs: &[Document]) -> Result<(), String> {
    let docs: Vec<_> = docs.iter().filter(|d| d.dirty()).collect();
    let parent = path.parent().ok_or("Invalid recovery path")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    atomic_write(
        path,
        &serde_json::to_vec(&docs).map_err(|e| e.to_string())?,
        true,
        None,
    )
}
/// Regex performs Unicode case folding without using offsets into a lowercased copy.
pub fn matches(text: &str, query: &str, case_sensitive: bool) -> Vec<(usize, usize)> {
    if query.is_empty() {
        return vec![];
    }
    regex::RegexBuilder::new(&regex::escape(query))
        .case_insensitive(!case_sensitive)
        .build()
        .map(|r| r.find_iter(text).map(|m| (m.start(), m.end())).collect())
        .unwrap_or_default()
}
pub fn replace(text: &str, ranges: &[(usize, usize)], replacement: &str) -> Result<String, String> {
    let removed: usize = ranges.iter().map(|(a, z)| z - a).sum();
    let added = replacement
        .len()
        .checked_mul(ranges.len())
        .ok_or("Replacement is too large")?;
    let size = text
        .len()
        .checked_sub(removed)
        .and_then(|n| n.checked_add(added))
        .ok_or("Replacement is too large")?;
    if size > MAX_BYTES {
        return Err("Replacement would exceed the 1 MiB document limit.".into());
    }
    let mut result = String::with_capacity(size);
    let mut end = 0;
    for &(a, z) in ranges {
        result.push_str(&text[end..a]);
        result.push_str(replacement);
        end = z;
    }
    result.push_str(&text[end..]);
    validate(&result)?;
    Ok(result)
}
pub fn language(path: Option<&Path>) -> &'static str {
    match path
        .and_then(|p| p.extension())
        .and_then(|s| s.to_str())
        .unwrap_or("")
    {
        "rs" => "Rust",
        "py" => "Python",
        "js" | "ts" | "tsx" | "jsx" => "JavaScript / TypeScript",
        "json" => "JSON",
        "md" => "Markdown",
        "toml" => "TOML",
        "sh" => "Shell",
        "c" | "h" | "cpp" => "C / C++",
        _ => "Plain text",
    }
}
