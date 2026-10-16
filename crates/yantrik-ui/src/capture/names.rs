//! Where a capture is saved and what it is called.
//!
//! `Screenshot 2026-10-15 at 14.32.07.png` and `Recording 2026-10-15 at 14.32.07.mp4`, in local
//! time: the name a person sees in Files sorts by when it was taken and says what it is. The
//! old names were `screenshot-2026-10-15-093207.png` in UTC, so a picture taken at 15:02 in
//! Kolkata was filed as 09:32 — the time on no clock the person owns. Dots in the time, not
//! colons: a colon is a path separator to half the machines a file gets copied to.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Local};

/// What a capture is, which decides its folder, its word and its extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Screenshot,
    Recording,
}

impl Kind {
    fn word(self) -> &'static str {
        match self {
            Kind::Screenshot => "Screenshot",
            Kind::Recording => "Recording",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Kind::Screenshot => "png",
            Kind::Recording => "mp4",
        }
    }

    /// The folder under the home: `~/Pictures/Screenshots` or `~/Videos/Recordings`.
    pub fn dir(self, home: &Path) -> PathBuf {
        match self {
            Kind::Screenshot => home.join("Pictures/Screenshots"),
            Kind::Recording => home.join("Videos/Recordings"),
        }
    }
}

/// The file name for a capture taken at `at`.
pub fn file_name(kind: Kind, at: &DateTime<Local>) -> String {
    format!("{} {} at {}.{}", kind.word(), at.format("%Y-%m-%d"), at.format("%H.%M.%S"), kind.extension())
}

/// The person's home, from the environment the session gave the shell.
pub fn home() -> Result<PathBuf, String> {
    std::env::var("HOME")
        .ok()
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not set, so there is nowhere to save a capture".to_string())
}

/// A path in the kind's folder that nothing is at yet, the folder made if it was missing. Two
/// screenshots in one second (Shift+Print pressed twice) are `… at 14.32.07.png` and
/// `… at 14.32.07 (2).png`, never one written over the other.
pub fn fresh_path(kind: Kind, home: &Path, at: &DateTime<Local>) -> Result<PathBuf, String> {
    let dir = kind.dir(home);
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not make {}: {e}", dir.display()))?;
    let name = file_name(kind, at);
    let first = dir.join(&name);
    if !first.exists() {
        return Ok(first);
    }
    let stem = name.trim_end_matches(&format!(".{}", kind.extension())).to_string();
    (2..1000)
        .map(|n| dir.join(format!("{stem} ({n}).{}", kind.extension())))
        .find(|p| !p.exists())
        .ok_or_else(|| format!("{} already holds a thousand captures with this name", dir.display()))
}

/// Whether `path` is a capture this desktop saved: a file directly in one of the two folders,
/// named as above. The notification buttons (Open, Show in Files, Delete) act only on such a
/// path, so a notification that names any other file — whoever filed it — moves nothing.
pub fn is_capture(path: &Path, home: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else { return false };
    let Some(parent) = path.parent() else { return false };
    [Kind::Screenshot, Kind::Recording].into_iter().any(|kind| {
        parent == kind.dir(home)
            && name.starts_with(&format!("{} ", kind.word()))
            && name.ends_with(&format!(".{}", kind.extension()))
            && !name.contains('/')
    }) && !path.components().any(|c| matches!(c, std::path::Component::ParentDir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 10, 15, 14, 32, 7).unwrap()
    }

    #[test]
    fn a_capture_is_named_for_what_it_is_and_when_by_the_local_clock() {
        assert_eq!(file_name(Kind::Screenshot, &at()), "Screenshot 2026-10-15 at 14.32.07.png");
        assert_eq!(file_name(Kind::Recording, &at()), "Recording 2026-10-15 at 14.32.07.mp4");
        let home = Path::new("/home/p");
        assert_eq!(Kind::Screenshot.dir(home), Path::new("/home/p/Pictures/Screenshots"));
        assert_eq!(Kind::Recording.dir(home), Path::new("/home/p/Videos/Recordings"));
    }

    #[test]
    fn two_captures_in_one_second_are_two_files() {
        let home = std::env::temp_dir().join(format!("yantrik-names-{}", std::process::id()));
        let first = fresh_path(Kind::Screenshot, &home, &at()).unwrap();
        std::fs::write(&first, b"x").unwrap();
        let second = fresh_path(Kind::Screenshot, &home, &at()).unwrap();
        assert_ne!(first, second);
        assert!(second.ends_with("Screenshot 2026-10-15 at 14.32.07 (2).png"), "{second:?}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn only_a_file_this_desktop_saved_counts_as_a_capture() {
        let home = Path::new("/home/p");
        assert!(is_capture(Path::new("/home/p/Pictures/Screenshots/Screenshot 2026-10-15 at 14.32.07.png"), home));
        assert!(is_capture(Path::new("/home/p/Videos/Recordings/Recording 2026-10-15 at 14.32.07 (2).mp4"), home));
        for not in [
            "/home/p/Documents/taxes.pdf",
            "/home/p/Pictures/Screenshots/notes.txt",
            "/home/p/Pictures/Screenshots/../../.ssh/id_ed25519",
            "/home/p/Pictures/Screenshots/sub/Screenshot x.png",
            "/home/p/Videos/Recordings/Screenshot 2026-10-15 at 14.32.07.png",
            "/etc/Pictures/Screenshots/Screenshot a.png",
        ] {
            assert!(!is_capture(Path::new(not), home), "{not}");
        }
    }
}
