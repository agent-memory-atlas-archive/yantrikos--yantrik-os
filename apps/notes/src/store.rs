//! The existing Markdown vault, with bounded reads and recoverable writes.
use serde::{Deserialize, Serialize};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
pub const LIMIT: usize = 256 * 1024;
pub const VAULT_LIMIT: usize = 32 * 1024 * 1024;
#[derive(Clone, Default, Serialize, Deserialize, Debug)]
pub struct Note {
    pub id: String,
    pub text: String,
    pub meta: String,
    pub baseline: Option<String>,
    pub baseline_meta: Option<String>,
    pub modified: u64,
    pub trash: bool,
}
impl Note {
    pub fn title(&self) -> String {
        self.text
            .lines()
            .find_map(|l| l.strip_prefix("# "))
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("Untitled")
            .trim()
            .chars()
            .take(160)
            .collect()
    }
    pub fn field(&self, key: &str) -> String {
        self.meta
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{key}:")))
            .unwrap_or("")
            .trim()
            .into()
    }
    pub fn set_field(&mut self, key: &str, value: &str) {
        let mut lines: Vec<_> = self
            .meta
            .lines()
            .filter(|l| !l.starts_with(&format!("{key}:")))
            .map(String::from)
            .collect();
        lines.push(format!("{key}:{}", value.replace(['\r', '\n'], " ")));
        self.meta = lines.join("\n") + "\n";
    }
    pub fn dirty(&self) -> bool {
        self.baseline.as_deref() != Some(&self.text)
            || self.baseline_meta.as_deref().unwrap_or("") != self.meta
    }
    pub fn blank(title: &str) -> Self {
        Self {
            id: format!("note-{}.md", uuid7::uuid7()),
            text: format!("# {}\n\n", title.replace(['\r', '\n'], " ")),
            ..Self::default()
        }
    }
}
pub fn validate(s: &str) -> Result<(), String> {
    if s.len() > LIMIT || s.bytes().filter(|b| *b == b'\n').count() > 10000 {
        return Err("A note can contain up to 256 KiB and 10,000 lines.".into());
    }
    if s.chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err("Notes require UTF-8 text without control characters.".into());
    }
    Ok(())
}
pub fn read(path: &Path, limit: usize) -> Result<Option<String>, String> {
    let mut file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let m = file.metadata().map_err(|e| e.to_string())?;
    if !m.is_file() || m.len() > limit as u64 {
        return Err("Not a regular text file, or file exceeds the size limit.".into());
    }
    let mut s = String::new();
    Read::by_ref(&mut file)
        .take(limit as u64 + 1)
        .read_to_string(&mut s)
        .map_err(|e| e.to_string())?;
    if s.len() > limit {
        return Err("File grew beyond its size limit.".into());
    }
    Ok(Some(s))
}
pub fn atomic(path: &Path, text: &str, replace: bool) -> Result<(), String> {
    let parent = path.parent().ok_or("No parent directory")?;
    let temp = parent.join(format!(".notes-{}.tmp", uuid7::uuid7()));
    let result = (|| {
        let mut f = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temp)?;
        if replace {
            if let Ok(m) = fs::symlink_metadata(path) {
                if !m.is_file() || m.nlink() != 1 || m.mode() & 0o222 == 0 {
                    return Err(std::io::Error::other("Destination is linked or read-only"));
                }
                f.set_permissions(fs::Permissions::from_mode(m.mode() & 0o777))?;
            }
        }
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        if replace {
            fs::rename(&temp, path)?;
        } else {
            fs::hard_link(&temp, path)?;
            fs::remove_file(&temp)?;
        }
        fs::File::open(parent)?.sync_all()
    })();
    let _ = fs::remove_file(temp);
    result.map_err(|e| e.to_string())
}
fn valid_id(id: &str) -> Result<(), String> {
    if id.ends_with(".md")
        && Path::new(id).components().count() == 1
        && !id.starts_with('.')
        && !id.contains(['/', '\\'])
    {
        Ok(())
    } else {
        Err("Invalid note filename".into())
    }
}
fn paths(dir: &Path, n: &Note) -> Result<(PathBuf, PathBuf), String> {
    valid_id(&n.id)?;
    let p = dir.join(&n.id);
    Ok((p.clone(), p.with_extension("meta")))
}
pub fn initialize(dir: &Path) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    for name in [".recovery-v2", ".trash-v2"] {
        let p = dir.join(name);
        fs::create_dir_all(&p).map_err(|e| e.to_string())?;
        if !fs::symlink_metadata(&p)
            .map_err(|e| e.to_string())?
            .is_dir()
        {
            return Err("Recovery and Trash must be real directories.".into());
        }
        fs::set_permissions(p, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    }
    Ok(())
}
pub fn load(dir: &Path) -> Result<(Vec<Note>, String), String> {
    initialize(dir)?;
    let mut notes = vec![];
    let mut bytes = 0;
    let mut skipped = 0;
    let mut entries: Vec<_> = fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .take(10001)
        .filter_map(Result::ok)
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let Some(id) = e.file_name().to_str().map(String::from) else {
            continue;
        };
        if !id.ends_with(".md") {
            continue;
        }
        let result = (|| {
            valid_id(&id)?;
            let text = read(&e.path(), LIMIT)?.ok_or("Missing note")?;
            validate(&text)?;
            let meta = read(&e.path().with_extension("meta"), 8192)?.unwrap_or_default();
            Ok::<_, String>((text, meta))
        })();
        let Ok((text, meta)) = result else {
            skipped += 1;
            continue;
        };
        if notes.len() >= 2000 || bytes + text.len() > VAULT_LIMIT {
            skipped += 1;
            continue;
        }
        bytes += text.len();
        let modified = e
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        notes.push(Note {
            id,
            text: text.clone(),
            meta: meta.clone(),
            baseline: Some(text),
            baseline_meta: Some(meta),
            modified,
            trash: false,
        });
    }
    for e in fs::read_dir(dir.join(".trash-v2"))
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .take(2000)
    {
        if e.path().extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        if let Ok(Some(s)) = read(&e.path(), LIMIT * 12 + 32768) {
            if let Ok(mut n) = serde_json::from_str::<Note>(&s) {
                if valid_id(&n.id).is_ok()
                    && validate(&n.text).is_ok()
                    && bytes + n.text.len() <= VAULT_LIMIT
                {
                    n.trash = true;
                    bytes += n.text.len();
                    notes.push(n)
                } else {
                    skipped += 1
                }
            } else {
                skipped += 1
            }
        }
    }
    let journal = dir.join(".recovery-v2/pending.json");
    if let Some(s) = read(&journal, LIMIT * 12 + 32768)? {
        let mut n: Note = serde_json::from_str(&s)
            .map_err(|_| "Recovery file is unreadable; it has been preserved.")?;
        valid_id(&n.id)?;
        validate(&n.text)?;
        // Always recover as a separate note: an external edit must survive too.
        let identical = notes
            .iter()
            .any(|a| !a.trash && a.id == n.id && a.text == n.text && a.meta == n.meta);
        if !identical {
            n.id = format!("recovered-{}.md", uuid7::uuid7());
            n.baseline = None;
            n.baseline_meta = None;
            n.trash = false;
            save_files(dir, &n)?;
            notes.push(saved(n));
        }
        fs::remove_file(journal).map_err(|e| e.to_string())?;
        return Ok((
            notes,
            "Recovered the interrupted save. Your original notes were preserved.".into(),
        ));
    }
    Ok((
        notes,
        if skipped > 0 {
            format!("{skipped} files could not be loaded (format or library limits). Originals are untouched.")
        } else {
            String::new()
        },
    ))
}
fn check(dir: &Path, n: &Note) -> Result<(), String> {
    let (p, m) = paths(dir, n)?;
    if read(&p, LIMIT)? != n.baseline
        || read(&m, 8192)?.unwrap_or_default() != n.baseline_meta.clone().unwrap_or_default()
    {
        return Err(
            "This note changed on disk. Your draft is protected; use Save a copy, then Refresh."
                .into(),
        );
    }
    Ok(())
}
fn save_files(dir: &Path, n: &Note) -> Result<(), String> {
    let (p, m) = paths(dir, n)?;
    check(dir, n)?;
    atomic(&p, &n.text, n.baseline.is_some())?;
    if !n.meta.is_empty() || m.exists() {
        atomic(&m, &n.meta, m.exists())?;
    }
    Ok(())
}
fn saved(mut n: Note) -> Note {
    n.baseline = Some(n.text.clone());
    n.baseline_meta = Some(n.meta.clone());
    n.modified = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    n
}
pub fn save(dir: &Path, n: &Note) -> Result<Note, String> {
    validate(&n.text)?;
    if n.meta.len() > 8192 {
        return Err("Metadata exceeds 8 KiB.".into());
    }
    valid_id(&n.id)?;
    // Write-ahead recovery survives a crash between content and metadata commits.
    atomic(
        &dir.join(".recovery-v2/pending.json"),
        &serde_json::to_string(n).map_err(|e| e.to_string())?,
        true,
    )?;
    save_files(dir, n)?;
    fs::remove_file(dir.join(".recovery-v2/pending.json")).map_err(|e| e.to_string())?;
    Ok(saved(n.clone()))
}
pub fn trash(dir: &Path, n: &Note) -> Result<(), String> {
    check(dir, n)?;
    let (p, m) = paths(dir, n)?;
    let record = dir.join(".trash-v2").join(format!("{}.json", n.id));
    atomic(
        &record,
        &serde_json::to_string(n).map_err(|e| e.to_string())?,
        false,
    )?;
    fs::remove_file(p).map_err(|e| e.to_string())?;
    if m.exists() {
        fs::remove_file(m).map_err(|e| e.to_string())?;
    }
    fs::File::open(dir)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())
}
pub fn restore(dir: &Path, n: &Note) -> Result<Note, String> {
    let mut n = n.clone();
    n.baseline = None;
    n.baseline_meta = None;
    n.trash = false;
    let (p, m) = paths(dir, &n)?;
    if p.exists() || m.exists() {
        return Err("A note already uses this filename. Nothing was overwritten.".into());
    }
    let n = save(dir, &n)?;
    fs::remove_file(dir.join(".trash-v2").join(format!("{}.json", n.id)))
        .map_err(|e| e.to_string())?;
    Ok(n)
}
