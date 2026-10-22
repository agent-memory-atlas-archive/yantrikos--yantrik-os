//! Another program's version a save replaced, kept beside the file instead of lost.
//!
//! A save replaces the file by an atomic exchange (`Disk::exchange`), so the bytes it took the
//! place of come back at the same instant ours went in. When they are not what the tab knew was
//! there, another program wrote the file between the save's check and its replace, and those bytes
//! are kept: as a file of their own beside the original, never over anything (`create_new`), and
//! private (mode 0600) since they may be.
//!
//! The exchange reads back at most 64 MiB. A larger file it took the place of is left unread under
//! a hidden temporary name beside the original (`Displaced::Unread`); it is given a visible name
//! here when it can be, and the notice says where it is either way. Nothing here ever deletes it.

use std::path::{Path, PathBuf};

use yantrik_app::documents::disk::{Disk, Step};

/// Where another program's version is now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kept {
    /// Kept as this file, beside the original.
    Beside(PathBuf),
    /// Too large to read back, or unreadable: kept where the exchange left it, at this path.
    Unread(PathBuf),
    /// It could not be written anywhere, and why.
    Lost(String),
}

/// How many names beside the original are tried before giving up.
const NAMES: usize = 100;

/// Another program's `bytes`, which a save of `original` replaced, kept beside it.
pub fn keep_bytes(disk: &dyn Disk, original: &Path, bytes: &[u8]) -> Kept {
    let mut last = String::new();
    for n in 1..=NAMES {
        let path = beside(original, n);
        match disk.create_new(&path, bytes) {
            Ok(()) => return Kept::Beside(path),
            // Made, its directory sync aside: it is there to be found.
            Err(e) if e.step == Step::SyncDir => return Kept::Beside(path),
            // Something already has that name: try the next.
            Err(e) if e.step == Step::Rename && path.exists() => last = e.error,
            Err(e) => return Kept::Lost(e.error),
        }
    }
    Kept::Lost(last)
}

/// A file the exchange left unread at `temp`, given a visible name beside `original` when it can
/// be (a second name, then the hidden one removed), and left where it is when not.
pub fn keep_unread(original: &Path, temp: &Path) -> Kept {
    for n in 1..=NAMES {
        let path = beside(original, n);
        match std::fs::hard_link(temp, &path) {
            Ok(()) => {
                let _ = std::fs::remove_file(temp);
                return Kept::Beside(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => break,
        }
    }
    Kept::Unread(temp.to_path_buf())
}

/// `notes (outside version).txt`, then `notes (outside version 2).txt`, beside `notes.txt`.
fn beside(original: &Path, n: usize) -> PathBuf {
    let stem = original.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = original.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let tag = if n == 1 { "outside version".to_string() } else { format!("outside version {n}") };
    original.with_file_name(format!("{stem} ({tag}){ext}"))
}

/// The sentence that says where another program's version of `file` is.
pub fn said(kept: &Kept, file: &Path) -> String {
    let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    match kept {
        Kept::Beside(path) => format!(
            "Another program wrote {name} while it was being saved; its version is kept beside it as {}.",
            path.display()
        ),
        Kept::Unread(path) => format!(
            "Another program put a file at {name} while it was being saved, too large to check (over \
             64 MiB); it is kept unread at {}.",
            path.display()
        ),
        Kept::Lost(why) => format!(
            "Another program wrote {name} while it was being saved, and its version could not be kept \
             ({why})."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_names_beside_the_original_keep_its_extension() {
        let p = Path::new("/d/notes.txt");
        assert_eq!(beside(p, 1), Path::new("/d/notes (outside version).txt"));
        assert_eq!(beside(p, 2), Path::new("/d/notes (outside version 2).txt"));
        assert_eq!(beside(Path::new("/d/Makefile"), 1), Path::new("/d/Makefile (outside version)"));
    }
}
