//! Local music library scan: walk a folder for audio files and read what a file name alone
//! can tell us.
//!
//! Tags and durations are deliberately out of scope here: they need a metadata crate, and this
//! issue is only the scan. So the file stem is the title, `artist`/`album`/`genre` stay empty,
//! `duration_secs` stays `0.0` and the duration text is `"--:--"`.

use std::path::Path;

/// One track found by a library scan. Pure data, no UI types.
pub struct Track {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub genre: String,
    pub duration_secs: f64,
    pub path: String,
}

const AUDIO_EXTENSIONS: &[&str] = &["flac", "mp3", "ogg", "opus", "wav", "m4a"];

fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| AUDIO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// Walk `root` recursively, keeping one `Track` per recognised audio file, sorted by title so
/// the result is stable between scans.
pub fn scan_folder(root: &Path) -> Vec<Track> {
    let mut tracks = Vec::new();
    walk(root, &mut tracks);
    tracks.sort_by(|a, b| a.title.cmp(&b.title));
    tracks
}

fn walk(dir: &Path, tracks: &mut Vec<Track>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, tracks);
        } else if is_audio(&path) {
            tracks.push(track_from(&path));
        }
    }
}

fn track_from(path: &Path) -> Track {
    let title = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string();
    Track {
        title,
        artist: String::new(),
        album: String::new(),
        genre: String::new(),
        duration_secs: 0.0,
        path: path.to_string_lossy().into_owned(),
    }
}

/// `m:ss` for a known duration, `"--:--"` when it is unknown (0.0) or non-finite.
pub fn duration_text(secs: f64) -> String {
    if !secs.is_finite() || secs <= 0.0 {
        return "--:--".to_string();
    }
    let total = secs.round() as u64;
    let minutes = total / 60;
    let seconds = total % 60;
    format!("{}:{:02}", minutes, seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique temp directory per call, so parallel tests never share a path.
    fn temp_dir(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "yantrik-music-player-{tag}-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn scan_finds_audio_and_ignores_the_rest() {
        let root = temp_dir("scan");
        std::fs::write(root.join("a.flac"), b"").expect("a.flac");
        std::fs::write(root.join("b.MP3"), b"").expect("b.MP3");
        std::fs::write(root.join("c.txt"), b"not audio").expect("c.txt");
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).expect("sub dir");
        std::fs::write(sub.join("d.opus"), b"").expect("d.opus");

        let tracks = scan_folder(&root);
        let titles: Vec<&str> = tracks.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, vec!["a", "b", "d"], "c.txt is excluded, the three audio files are kept");

        let a = tracks.iter().find(|t| t.title == "a").expect("a.flac kept");
        assert_eq!(a.title, "a");
        assert!(a.path.ends_with("a.flac"), "path is the file's own: {}", a.path);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn scan_of_a_missing_folder_is_empty() {
        let missing = temp_dir("missing").join("does-not-exist");
        let tracks = scan_folder(&missing);
        assert!(tracks.is_empty(), "a missing folder yields no tracks, not a panic");
    }
}
