//! The disk an editor's store writes through, as a seam: the real one, and one that fails a
//! chosen step so every way a save can fail is a test (design/notes-gate-2026-10-21.md §3).
//!
//! An atomic replace is five steps — create a temp file, write it, sync it, rename it over the
//! destination, sync the directory — and each failure means something different for what is on
//! disk. [`Step::durability`] says which.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::Durability;

/// One step of an atomic replace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    CreateTemp,
    Write,
    SyncFile,
    Rename,
    SyncDir,
}

impl Step {
    /// What a failure at this step leaves on disk.
    pub fn durability(self) -> Durability {
        match self {
            Step::CreateTemp | Step::Write | Step::SyncFile | Step::Rename => Durability::NotWritten,
            Step::SyncDir => Durability::Unknown,
        }
    }
}

/// A failed disk operation, with the step it failed at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskError {
    pub step: Step,
    pub error: String,
}

impl std::fmt::Display for DiskError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({:?})", self.error, self.step)
    }
}

/// What an editor's store needs from the disk.
pub trait Disk: Send + Sync {
    /// Replaces (or creates) `path` with `bytes`, whole or not at all.
    fn write_atomically(&self, path: &Path, bytes: &[u8]) -> Result<(), DiskError>;
    /// The file's bytes, or `None` when it is absent.
    fn read(&self, path: &Path) -> std::io::Result<Option<Vec<u8>>>;
    fn remove(&self, path: &Path) -> std::io::Result<()>;
}

/// The real disk.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealDisk;

impl Disk for RealDisk {
    fn write_atomically(&self, path: &Path, bytes: &[u8]) -> Result<(), DiskError> {
        let fail = |step: Step| move |e: std::io::Error| DiskError { step, error: e.to_string() };
        let parent = path.parent().ok_or(DiskError { step: Step::CreateTemp, error: "no parent directory".into() })?;
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let temp = parent.join(format!(".{name}.{}.tmp", std::process::id()));
        let result = (|| {
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temp)
                .map_err(fail(Step::CreateTemp))?;
            file.write_all(bytes).map_err(fail(Step::Write))?;
            file.sync_all().map_err(fail(Step::SyncFile))?;
            std::fs::rename(&temp, path).map_err(fail(Step::Rename))?;
            std::fs::File::open(parent)
                .and_then(|d| d.sync_all())
                .map_err(fail(Step::SyncDir))
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        result
    }

    fn read(&self, path: &Path) -> std::io::Result<Option<Vec<u8>>> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn remove(&self, path: &Path) -> std::io::Result<()> {
        match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

/// A disk that fails a chosen step for paths that match, then behaves. For tests.
#[derive(Debug, Default)]
pub struct FaultyDisk {
    faults: Mutex<Vec<Fault>>,
    /// Paths the fault hit, in order, so a test can assert which write failed.
    pub hits: Mutex<Vec<PathBuf>>,
}

#[derive(Debug, Clone)]
struct Fault {
    step: Step,
    /// A path ends with this to be hit (`".meta"`, `"pending.json"`, or `""` for every path).
    suffix: String,
    /// How many more times it fails; `None` for always.
    times: Option<usize>,
}

impl FaultyDisk {
    pub fn new() -> FaultyDisk {
        FaultyDisk::default()
    }

    /// Fails `step` for paths ending with `suffix`, `times` times (`None`: until cleared).
    pub fn fail(&self, step: Step, suffix: &str, times: Option<usize>) {
        self.faults.lock().unwrap_or_else(|p| p.into_inner()).push(Fault { step, suffix: suffix.into(), times });
    }

    pub fn heal(&self) {
        self.faults.lock().unwrap_or_else(|p| p.into_inner()).clear();
    }

    fn fault_for(&self, path: &Path) -> Option<Step> {
        let mut faults = self.faults.lock().unwrap_or_else(|p| p.into_inner());
        let text = path.to_string_lossy();
        let at = faults.iter().position(|f| text.ends_with(&f.suffix) && f.times != Some(0))?;
        let step = faults[at].step;
        if let Some(n) = faults[at].times.as_mut() {
            *n -= 1;
        }
        self.hits.lock().unwrap_or_else(|p| p.into_inner()).push(path.to_path_buf());
        Some(step)
    }
}

impl Disk for FaultyDisk {
    fn write_atomically(&self, path: &Path, bytes: &[u8]) -> Result<(), DiskError> {
        match self.fault_for(path) {
            // A failure before the rename leaves the destination as it was.
            Some(step @ (Step::CreateTemp | Step::Write | Step::SyncFile | Step::Rename)) => {
                Err(DiskError { step, error: format!("injected failure at {step:?}") })
            }
            // A failure after the rename: the bytes are there, their durability is not known.
            Some(Step::SyncDir) => {
                RealDisk.write_atomically(path, bytes)?;
                Err(DiskError { step: Step::SyncDir, error: "injected failure at SyncDir".into() })
            }
            None => RealDisk.write_atomically(path, bytes),
        }
    }

    fn read(&self, path: &Path) -> std::io::Result<Option<Vec<u8>>> {
        RealDisk.read(path)
    }

    fn remove(&self, path: &Path) -> std::io::Result<()> {
        RealDisk.remove(path)
    }
}
