//! Bounded, conflict-aware publication of private YAML preferences.
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::{
    collections::HashMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, OnceLock,
    },
};
const LIMIT: usize = 256 * 1024;
static FILES: OnceLock<Mutex<HashMap<PathBuf, PreferenceFile>>> = OnceLock::new();
static SERIAL: AtomicU64 = AtomicU64::new(0);
pub struct PreferenceFile {
    path: PathBuf,
    baseline: Option<String>,
}
fn read(path: &Path) -> Result<Option<String>, String> {
    let mut f = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("Cannot read preferences: {e}")),
    };
    let m = f.metadata().map_err(|e| e.to_string())?;
    if !m.is_file() || m.len() > LIMIT as u64 {
        return Err("Preferences must be a regular UTF-8 file of at most 256 KiB.".into());
    }
    let mut text = String::new();
    Read::by_ref(&mut f)
        .take(LIMIT as u64 + 1)
        .read_to_string(&mut text)
        .map_err(|_| "Preferences are not valid UTF-8.")?;
    if text.len() > LIMIT {
        return Err("Preferences exceed 256 KiB.".into());
    }
    Ok(Some(text))
}
impl PreferenceFile {
    pub fn open(path: PathBuf) -> Result<Self, String> {
        let baseline = read(&path)?;
        Ok(Self { path, baseline })
    }
    pub fn content(&self) -> Option<&str> {
        self.baseline.as_deref()
    }
    pub fn save(&mut self, yaml: &str) -> Result<(), String> {
        if yaml.len() > LIMIT {
            return Err("Preferences exceed 256 KiB.".into());
        }
        let current = read(&self.path)?;
        if current != self.baseline {
            return Err("Preferences changed on disk. Changes in this session were not saved; restart the shell to reload the file.".into());
        }
        let mut existing = match current.as_deref() {
            Some(s) => serde_yaml::from_str::<serde_yaml::Mapping>(s).map_err(|_| {
                "Existing preferences contain invalid YAML and have been preserved."
            })?,
            None => serde_yaml::Mapping::new(),
        };
        let changes: serde_yaml::Mapping =
            serde_yaml::from_str(yaml).map_err(|_| "Cannot serialize preferences.")?;
        existing.extend(changes);
        let text = serde_yaml::to_string(&existing).map_err(|_| "Cannot serialize preferences.")?;
        if text.len() > LIMIT {
            return Err("Preferences exceed 256 KiB.".into());
        }
        if let Ok(m) = fs::symlink_metadata(&self.path) {
            if !m.is_file() || m.nlink() != 1 || m.mode() & 0o222 == 0 {
                return Err(
                    "Preferences are linked or read-only; the original file was preserved.".into(),
                );
            }
        }
        let parent = self
            .path
            .parent()
            .ok_or("Preferences have no parent directory")?;
        fs::create_dir_all(parent)
            .map_err(|e| format!("Cannot create preferences directory: {e}"))?;
        let temp = parent.join(format!(
            ".preferences-{}-{}.tmp",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        let mut temp_created = false;
        let result = (|| {
            let mut f = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(&temp)
                .map_err(|e| e.to_string())?;
            temp_created = true;
            f.write_all(text.as_bytes())
                .and_then(|_| f.sync_all())
                .map_err(|e| e.to_string())?;
            // Check again immediately before publication; this is normal conflict detection,
            // not a locking protocol with uncooperative external writers.
            if read(&self.path)? != self.baseline {
                return Err(
                    "Preferences changed while saving; the external version was preserved.".into(),
                );
            }
            if self.baseline.is_some() {
                fs::rename(&temp, &self.path).map_err(|e| e.to_string())?;
            } else {
                fs::hard_link(&temp, &self.path).map_err(|e| e.to_string())?;
                fs::remove_file(&temp).map_err(|e| e.to_string())?;
            }
            self.baseline = Some(text);
            fs::File::open(parent)
                .and_then(|f| f.sync_all())
                .map_err(|e| format!("File saved, but directory synchronization failed: {e}"))
        })();
        if temp_created {
            let _ = fs::remove_file(temp);
        }
        result
    }
}
pub fn load(path: impl AsRef<Path>) -> Result<Option<String>, String> {
    let mut files = FILES
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| "Preference store unavailable")?;
    let path = path.as_ref();
    if !files.contains_key(path) {
        files.insert(path.to_owned(), PreferenceFile::open(path.to_owned())?);
    }
    Ok(files[path].baseline.clone())
}
pub fn save(path: impl AsRef<Path>, yaml: &str) -> Result<(), String> {
    let path = path.as_ref();
    let _ = load(path)?;
    FILES
        .get()
        .unwrap()
        .lock()
        .map_err(|_| "Preference store unavailable")?
        .get_mut(path)
        .unwrap()
        .save(yaml)
}

/// Search the controls a person is looking for, not only category names.
pub fn search_categories(query: &str) -> Vec<i32> {
    const INDEX: [&str; 9] = [
        "appearance theme dark light accent color colour wallpaper background display",
        "ai intelligence provider model api endpoint routing budget usage privacy local cloud",
        "desktop workspace overview agent shortcuts keyboard windows",
        "network wifi wi-fi wireless ethernet wired ip address connection",
        "accounts integrations google spotify facebook instagram connect sync",
        "privacy security incognito memory retention lock idle timeout notifications disturb dnd",
        "system about version devices packages updates monitor notifications",
        "skills extensions capabilities store installed",
        "harnesses minds assistant companion agent hermes built in",
    ];
    let query = query.to_lowercase();
    let words: Vec<_> = query.split_whitespace().collect();
    INDEX
        .iter()
        .enumerate()
        .filter(|(_, s)| words.iter().all(|word| s.contains(word)))
        .map(|(i, _)| i as i32)
        .collect()
}
