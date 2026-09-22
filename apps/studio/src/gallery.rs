//! The pictures, the folders they land in, and the sidecar that says how each was made.
//!
//! Everything Studio generates is written under `~/Pictures/Studio/<date>/` with a `.json` beside
//! it holding the prompt, the seed, the backend, the model and the seconds. Two reasons, and the
//! second is the interesting one: a person can look at a picture later and know what produced it,
//! and a mind can read that sidecar back and answer "make another like this" without being told.
//! The sidecar is the part that makes generation something the rest of the OS can reason about
//! rather than a pile of PNGs.
//!
//! Nothing here knows about Slint. Reading and writing files is separated from showing them so
//! the gallery can be tested in a temporary directory.

use std::path::{Path, PathBuf};

use std::time::UNIX_EPOCH;

use chrono::{DateTime, Local, SecondsFormat};
use serde::{Deserialize, Serialize};

/// How many pictures `describe` carries. A gallery is a glance, not a transcript: the caller that
/// wants more can read the folder, which is the point of writing real files there.
pub const LISTING_CAP: usize = 12;

/// How far back the listing looks before giving up. Without a bound, a folder a person has been
/// filling for a year would be walked on every `describe`, which happens on the UI thread.
const SCAN_LIMIT: usize = 400;

/// The longest edge of a thumbnail. Small enough that a gallery of twelve costs a fraction of a
/// megabyte to hold and almost nothing to draw.
pub const THUMBNAIL_EDGE: u32 = 160;

/// What is written beside every picture. Every field is something a person or a mind would ask
/// about a generated image, and nothing is a field that only this app understands.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sidecar {
    /// The sentence that made the picture.
    pub prompt: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub negative: String,
    pub seed: u64,
    /// Which backend made it: `comfyui`, `openai-images` or `fake`.
    pub backend: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    /// How long it took, in seconds, as the person waited for it.
    pub seconds: f64,
    pub width: u32,
    pub height: u32,
    /// The size the backend was actually asked for, when it differs from the file's own. A hosted
    /// service that only accepts three sizes, or a placeholder capped for speed, is recorded here
    /// rather than being quietly resized.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sent: String,
    /// RFC 3339, in local time, because that is what a person reads.
    pub created: String,
    /// Where the picture came from, when it came from another picture: "variation of <name>"
    /// or "upscale of <name>". Empty for a first generation.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub made_from: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cfg: Option<f64>,
    /// Written by this app, so a reader knows the shape it is looking at.
    #[serde(default = "default_made_by")]
    pub made_by: String,
}

fn default_made_by() -> String {
    "yantrik-studio".to_string()
}

impl Sidecar {
    /// The path of the sidecar that belongs beside an image.
    pub fn path_for(image: &Path) -> PathBuf {
        image.with_extension("json")
    }

    /// Write beside the image, atomically. A sidecar is the record of how a picture was made and
    /// a half-written one is worse than none, because it parses as nothing and the picture then
    /// looks like it was not made here.
    pub fn write(&self, image: &Path) -> Result<PathBuf, String> {
        let path = Self::path_for(image);
        let temporary = path.with_extension("json.new");
        let body = serde_json::to_vec_pretty(self)
            .map_err(|e| format!("the sidecar could not be written ({e})"))?;
        std::fs::write(&temporary, body)
            .map_err(|e| format!("{} could not be written ({e})", temporary.display()))?;
        std::fs::rename(&temporary, &path)
            .map_err(|e| format!("{} could not be put in place ({e})", path.display()))?;
        Ok(path)
    }

    /// Read a sidecar back. A missing or unreadable one is `None`, not an error: a picture in the
    /// gallery without a sidecar is still a picture, and the listing says which ones lack one.
    pub fn read(image: &Path) -> Option<Sidecar> {
        let text = std::fs::read_to_string(Self::path_for(image)).ok()?;
        match serde_json::from_str::<Sidecar>(&text) {
            Ok(sidecar) => Some(sidecar),
            Err(e) => {
                tracing::warn!("{} is not a sidecar this version can read ({e})", image.display());
                None
            }
        }
    }
}

/// One row of the gallery: the file, what its sidecar says, and the thumbnail to show for it.
#[derive(Clone, Debug)]
pub struct Record {
    pub path: PathBuf,
    pub name: String,
    pub prompt: String,
    pub seed: u64,
    pub backend: String,
    pub model: String,
    pub seconds: f64,
    pub width: u32,
    pub height: u32,
    pub created: String,
    pub made_from: String,
    /// Whether a sidecar was found. `false` means the row below is what the filesystem could
    /// say, and the surface says so rather than presenting a guess as a record.
    pub has_sidecar: bool,
    /// Decoded pixels for the grid, as RGB. Built on the worker thread; Slint's own image type is
    /// not `Send`, so raw bytes are what crosses back to the UI.
    pub thumbnail: Option<Thumbnail>,
}

#[derive(Clone, Debug)]
pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

impl Record {
    /// The state fragment for one row. Keys that would say nothing are left out, so the JSON a
    /// caller reads is the JSON that means something.
    pub fn json(&self) -> serde_json::Value {
        let mut row = serde_json::Map::new();
        row.insert("path".into(), serde_json::json!(self.path.display().to_string()));
        if !self.prompt.is_empty() {
            row.insert("prompt".into(), serde_json::json!(self.prompt));
        }
        if self.has_sidecar {
            row.insert("seed".into(), serde_json::json!(self.seed));
            row.insert("backend".into(), serde_json::json!(self.backend));
            row.insert("seconds".into(), serde_json::json!((self.seconds * 10.0).round() / 10.0));
            row.insert("size".into(), serde_json::json!(format!("{}x{}", self.width, self.height)));
            if !self.model.is_empty() {
                row.insert("model".into(), serde_json::json!(self.model));
            }
            if !self.made_from.is_empty() {
                row.insert("made_from".into(), serde_json::json!(self.made_from));
            }
        } else {
            // The honest version of a row with no sidecar. A caller that saw a seed here would
            // believe it, and there is nothing to believe.
            row.insert("sidecar".into(), serde_json::json!("missing"));
        }
        row.insert("created".into(), serde_json::json!(self.created));
        serde_json::Value::Object(row)
    }
}

/// Where the pictures go: `~/Pictures/Studio`, or the folder the configuration names.
pub fn output_root(configured: &str) -> PathBuf {
    let base = match configured.trim() {
        "" => pictures_dir(),
        path => expand_home(path),
    };
    // A configured folder is used as given; the default gets Studio's own subfolder, so this app
    // does not write its output loose into a folder a person already has pictures in.
    if configured.trim().is_empty() {
        base.join("Studio")
    } else {
        base
    }
}

fn pictures_dir() -> PathBuf {
    match std::env::var_os("XDG_PICTURES_DIR") {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => home().join("Pictures"),
    }
}

pub fn home() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => PathBuf::from("/tmp"),
    }
}

pub fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None => PathBuf::from(path),
    }
}

/// Today's folder, `~/Pictures/Studio/2026-09-22`. A date in the path rather than in the filename
/// alone, because a folder a person can open in Files and browse by day is worth more than a flat
/// directory of a thousand images.
pub fn day_folder(root: &Path, when: DateTime<Local>) -> PathBuf {
    root.join(when.format("%Y-%m-%d").to_string())
}

/// A filename that does not exist yet, in `folder`. The time and a slice of the seed make it
/// readable and very nearly unique; the loop makes it actually unique, because two generations in
/// the same second with the same seed is exactly what a mind asking for four of a thing does.
pub fn unique_file(folder: &Path, when: DateTime<Local>, seed: u64) -> PathBuf {
    let stem = format!("{}-{:05}", when.format("%H%M%S"), seed % 100_000);
    let mut candidate = folder.join(format!("{stem}.png"));
    let mut attempt = 2;
    while candidate.exists() || Sidecar::path_for(&candidate).exists() {
        candidate = folder.join(format!("{stem}-{attempt}.png"));
        attempt += 1;
        if attempt > 999 {
            // Nothing sane left to try; a nanosecond suffix cannot collide with a filename a
            // person chose.
            return folder.join(format!(
                "{stem}-{}.png",
                when.timestamp_subsec_nanos()
            ));
        }
    }
    candidate
}

/// The gallery, newest first, up to `limit` rows. Reads a sidecar only for the rows it returns.
pub fn listing(root: &Path, limit: usize, thumbnails: bool) -> Vec<Record> {
    let Ok(days) = std::fs::read_dir(root) else { return Vec::new() };
    // Date folders sort lexicographically in date order, so walking them backwards is walking
    // time backwards without reading a single file's metadata to find out.
    let mut folders: Vec<PathBuf> = days
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    folders.sort();
    folders.reverse();

    let mut found: Vec<PathBuf> = Vec::new();
    let mut examined = 0;
    for folder in folders {
        let Ok(entries) = std::fs::read_dir(&folder) else { continue };
        let mut files: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_file()
                    && path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(yantrik_image_core::is_image)
            })
            .collect();
        files.sort();
        files.reverse();
        for file in files {
            if examined >= SCAN_LIMIT || found.len() >= limit {
                break;
            }
            examined += 1;
            found.push(file);
        }
        if examined >= SCAN_LIMIT || found.len() >= limit {
            break;
        }
    }

    let mut records: Vec<Record> = found.iter().map(|path| record(path, thumbnails)).collect();
    // Within a day the filenames already run in time order, but a picture copied in from
    // somewhere else has its own, so the gallery is sorted by what the file says about itself.
    //
    // The tie-break runs backwards on purpose. `created` is precise to a second, and a batch is
    // several pictures inside one second, so without a tie-break the order of a batch would depend
    // on the order `read_dir` happened to return; with an ascending one it would depend on the
    // seed. Either way a re-read of the folder could disagree with the engine, which puts each new
    // shot at the front as it lands — and then `gallery.newest` would name a different picture as
    // the newest depending on whether the folder had been read again since. Filenames start with
    // the time they were written, so descending on the path is descending on the moment, and a
    // collision suffix (`-2`, `-3`) sorts later, which is also true of when it was made.
    records.sort_by(|a, b| b.created.cmp(&a.created).then_with(|| b.path.cmp(&a.path)));
    records.truncate(limit);
    records
}

/// One row, from the file and its sidecar. Never fails: a picture with no sidecar is still a
/// picture, and the row says which of the two it is.
pub fn record(path: &Path, thumbnail: bool) -> Record {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let modified = std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|stamp| stamp.duration_since(UNIX_EPOCH).ok())
        // `from_timestamp` answers in UTC, and the dates here are read by a person and filed under
        // a folder named for their own day, so the one that is displayed is the local one.
        .and_then(|elapsed| DateTime::from_timestamp(elapsed.as_secs() as i64, 0))
        .map(|stamp| stamp.with_timezone(&Local))
        .unwrap_or_else(Local::now);
    let created = modified.to_rfc3339_opts(SecondsFormat::Secs, false);

    let (width, height) = match Sidecar::read(path) {
        Some(sidecar) => {
            return Record {
                prompt: sidecar.prompt,
                seed: sidecar.seed,
                backend: sidecar.backend,
                model: sidecar.model,
                seconds: sidecar.seconds,
                width: sidecar.width,
                height: sidecar.height,
                created: if sidecar.created.is_empty() { created } else { sidecar.created },
                made_from: sidecar.made_from,
                has_sidecar: true,
                path: path.to_path_buf(),
                name,
                thumbnail: if thumbnail { read_thumbnail(path) } else { None },
            }
        }
        None => dimensions(path),
    };

    Record {
        path: path.to_path_buf(),
        name,
        prompt: String::new(),
        seed: 0,
        backend: String::new(),
        model: String::new(),
        seconds: 0.0,
        width: width.unwrap_or(0),
        height: height.unwrap_or(0),
        created,
        made_from: String::new(),
        has_sidecar: false,
        thumbnail: if thumbnail { read_thumbnail(path) } else { None },
    }
}

/// The pixel size of a file, without decoding the whole of it.
pub fn dimensions(path: &Path) -> (Option<u32>, Option<u32>) {
    let Ok(reader) = image::ImageReader::open(path) else { return (None, None) };
    // The two Results in this chain carry different errors, one IO and one the decoder's, and
    // neither is something a caller of this can act on, so both collapse to "unknown".
    match reader.with_guessed_format().ok().and_then(|reader| reader.into_dimensions().ok()) {
        Some((width, height)) => (Some(width), Some(height)),
        None => (None, None),
    }
}

/// The pixel size of encoded image bytes, for a picture that has not been written yet.
pub fn png_size(bytes: &[u8]) -> (Option<u32>, Option<u32>) {
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes));
    match reader.with_guessed_format().ok().and_then(|reader| reader.into_dimensions().ok()) {
        Some((width, height)) => (Some(width), Some(height)),
        None => (None, None),
    }
}

/// Decode and shrink one picture. Done on a worker thread, because decoding twelve JPEGs is not
/// something to do on the thread that owns the window.
pub fn read_thumbnail(path: &Path) -> Option<Thumbnail> {
    let image = image::open(path).ok()?;
    let resized = image.thumbnail(THUMBNAIL_EDGE, THUMBNAIL_EDGE).to_rgb8();
    let (width, height) = resized.dimensions();
    Some(Thumbnail { width, height, rgb: resized.into_raw() })
}

/// The longest edge `upscale` will produce. Large enough to be useful on a 4K screen, small
/// enough that a resample stays inside a second and the result stays a file a person wants.
pub const LONGEST_EDGE: u32 = 8192;

/// Decode, resize by a factor, and hand back the encoded PNG. This is what `upscale` does, and it
/// is a resample rather than a model pass — the action's purpose says so plainly, because a
/// button labelled "upscale" that quietly runs a diffusion model and one that resamples are not
/// the same promise.
pub fn resample(path: &Path, factor: u32, longest_edge: u32) -> Result<Vec<u8>, String> {
    let image = image::open(path)
        .map_err(|e| format!("{} could not be opened as a picture ({e})", path.display()))?;
    let (width, height) = (image.width(), image.height());
    let factor = factor.clamp(1, 8);
    let mut target = (
        width.saturating_mul(factor).max(width),
        height.saturating_mul(factor).max(height),
    );
    let longest = target.0.max(target.1);
    if longest > longest_edge {
        let scale = longest_edge as f64 / longest as f64;
        target = (
            ((target.0 as f64) * scale).round().max(1.0) as u32,
            ((target.1 as f64) * scale).round().max(1.0) as u32,
        );
    }
    if target == (width, height) {
        return Err(format!(
            "{} is already {width}x{height}, which is as large as resampling it by {factor} can make it within {longest_edge} pixels",
            path.display()
        ));
    }
    let resized = image::imageops::resize(
        &image,
        target.0,
        target.1,
        // Lanczos3 is the slowest of the filters and the one that keeps an edge intact at 2x. A
        // resample of a picture this size takes well under a second, so there is no reason to
        // pick a cheaper one.
        image::imageops::FilterType::Lanczos3,
    );
    let mut bytes = Vec::new();
    resized
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
        .map_err(|e| format!("the resized picture could not be encoded ({e})"))?;
    Ok(bytes)
}

/// Whether a path is inside Studio's own output tree. `delete` and `upscale` are checked against
/// this: an app that will move any file on the machine to the Trash because a caller named it is
/// a different thing from an app that manages its own gallery, and only the second is what was
/// asked for.
pub fn is_inside(path: &Path, root: &Path) -> bool {
    let Ok(path) = std::fs::canonicalize(path) else { return false };
    let Ok(root) = std::fs::canonicalize(root) else { return false };
    path.starts_with(&root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn folder() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "studio-gallery-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sidecar(prompt: &str) -> Sidecar {
        Sidecar {
            prompt: prompt.into(),
            negative: "blurry".into(),
            seed: 1234,
            backend: "fake".into(),
            model: "fake".into(),
            seconds: 0.4,
            width: 64,
            height: 48,
            sent: "64x48".into(),
            created: "2026-09-22T10:00:00+05:30".into(),
            made_from: String::new(),
            steps: Some(30),
            cfg: Some(7.0),
            made_by: default_made_by(),
        }
    }

    #[test]
    fn the_pictures_go_in_a_dated_folder_under_pictures() {
        // The base is read from this machine rather than pinned by setting HOME here. The
        // environment is process-wide and the tests in one binary run at the same time, so a test
        // that moved HOME to check something would be changing what every other test saw —
        // including the ones asserting that a path is written with `~/…` in it.
        let base = match std::env::var_os("XDG_PICTURES_DIR") {
            Some(value) if !value.is_empty() => PathBuf::from(value),
            _ => home().join("Pictures"),
        };
        assert_eq!(output_root(""), base.join("Studio"));
        // A configured folder is used exactly as given, so a person can point Studio at a disk.
        assert_eq!(output_root("/srv/pictures"), PathBuf::from("/srv/pictures"));
        assert_eq!(output_root("~/art"), home().join("art"));
        let when = Local.with_ymd_and_hms(2026, 9, 22, 10, 0, 0).unwrap();
        assert_eq!(day_folder(&base.join("Studio"), when), base.join("Studio/2026-09-22"));
    }

    #[test]
    fn a_sidecar_written_beside_a_picture_reads_back_as_the_same_thing() {
        let dir = folder();
        let image = dir.join("2026-09-22").join("101500-01234.png");
        std::fs::create_dir_all(image.parent().unwrap()).unwrap();
        std::fs::write(&image, crate::backend::draw("a lighthouse", 1234, 64, 48)).unwrap();

        let written = sidecar("a lighthouse in fog");
        let path = written.write(&image).unwrap();
        assert_eq!(path, dir.join("2026-09-22").join("101500-01234.json"));
        assert!(!path.with_extension("json.new").exists(), "the temporary file was left behind");

        let read = Sidecar::read(&image).unwrap();
        assert_eq!(read.prompt, "a lighthouse in fog");
        assert_eq!(read.negative, "blurry");
        assert_eq!(read.seed, 1234);
        assert_eq!(read.backend, "fake");
        assert_eq!(read.seconds, 0.4);
        assert_eq!(read.sent, "64x48");
        assert_eq!(read.steps, Some(30));
        assert_eq!(read.made_by, "yantrik-studio");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_seed_no_signed_integer_holds_survives_the_sidecar() {
        // The whole promise of a seed is that the picture can be made again from it.
        let dir = folder();
        let image = dir.join("big.png");
        std::fs::write(&image, crate::backend::draw("x", 1, 8, 8)).unwrap();
        let mut written = sidecar("x");
        written.seed = u64::MAX - 1;
        written.write(&image).unwrap();
        let text = std::fs::read_to_string(Sidecar::path_for(&image)).unwrap();
        assert!(text.contains("18446744073709551614"), "{text}");
        assert_eq!(Sidecar::read(&image).unwrap().seed, u64::MAX - 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_sidecar_from_an_older_version_still_reads() {
        // Fields added later must not make pictures already on disk unreadable, and fields a
        // hand-written sidecar omits must not make it a parse failure.
        let dir = folder();
        let image = dir.join("old.png");
        std::fs::write(&image, crate::backend::draw("x", 1, 8, 8)).unwrap();
        std::fs::write(
            Sidecar::path_for(&image),
            r#"{"prompt":"an older picture","seed":7,"backend":"comfyui","seconds":41.2,
                "width":1024,"height":1024,"created":"2026-01-01T09:00:00+05:30"}"#
                .replace('\n', ""),
        )
        .unwrap();
        let read = Sidecar::read(&image).unwrap();
        assert_eq!(read.prompt, "an older picture");
        assert_eq!(read.negative, "");
        assert_eq!(read.steps, None);
        assert_eq!(read.made_by, "yantrik-studio");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_picture_with_no_sidecar_is_still_listed_and_says_it_has_no_record() {
        let dir = folder();
        let day = dir.join("2026-09-22");
        std::fs::create_dir_all(&day).unwrap();
        std::fs::write(day.join("101500-01234.png"), crate::backend::draw("a lighthouse", 1234, 64, 48)).unwrap();
        std::fs::write(day.join("copied-in.png"), crate::backend::draw("elsewhere", 9, 32, 32)).unwrap();
        sidecar("a lighthouse in fog").write(&day.join("101500-01234.png")).unwrap();

        let rows = listing(&dir, LISTING_CAP, false);
        assert_eq!(rows.len(), 2, "{rows:?}");
        let with = rows.iter().find(|row| row.has_sidecar).unwrap();
        assert_eq!(with.prompt, "a lighthouse in fog");
        let without = rows.iter().find(|row| !row.has_sidecar).unwrap();
        assert_eq!(without.name, "copied-in.png");
        // The size still comes from the file, so the row is not empty; the seed and backend are
        // not invented for it.
        assert_eq!((without.width, without.height), (32, 32));
        assert_eq!(without.seed, 0);
        assert_eq!(without.json()["sidecar"], serde_json::json!("missing"));
        assert!(without.json().get("seed").is_none());
        assert!(with.json().get("sidecar").is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_gallery_is_newest_first_and_stops_at_the_limit() {
        let dir = folder();
        for (day, hour) in [("2026-09-20", 9), ("2026-09-22", 10), ("2026-09-21", 11)] {
            let folder = dir.join(day);
            std::fs::create_dir_all(&folder).unwrap();
            let image = folder.join(format!("{hour}0000-00001.png"));
            std::fs::write(&image, crate::backend::draw(day, 1, 16, 16)).unwrap();
            let mut record = sidecar(day);
            record.created = format!("{day}T{hour:02}:00:00+05:30");
            record.write(&image).unwrap();
        }
        let rows = listing(&dir, LISTING_CAP, false);
        let order: Vec<&str> = rows.iter().map(|row| row.prompt.as_str()).collect();
        assert_eq!(order, ["2026-09-22", "2026-09-21", "2026-09-20"], "{order:?}");

        let two = listing(&dir, 2, false);
        assert_eq!(two.len(), 2);
        assert_eq!(two[0].prompt, "2026-09-22");

        assert!(listing(&dir.join("nothing-here"), LISTING_CAP, false).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn only_pictures_are_listed_not_the_sidecars_beside_them() {
        let dir = folder();
        let day = dir.join("2026-09-22");
        std::fs::create_dir_all(&day).unwrap();
        std::fs::write(day.join("a.png"), crate::backend::draw("a", 1, 16, 16)).unwrap();
        sidecar("a").write(&day.join("a.png")).unwrap();
        std::fs::write(day.join("notes.txt"), "not a picture").unwrap();
        std::fs::write(day.join("a.json.new"), "{}").unwrap();
        let rows = listing(&dir, LISTING_CAP, false);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].name, "a.png");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_row_carries_what_a_person_would_ask_about_the_picture() {
        let dir = folder();
        let day = dir.join("2026-09-22");
        std::fs::create_dir_all(&day).unwrap();
        let image = day.join("101500-01234.png");
        std::fs::write(&image, crate::backend::draw("a lighthouse", 1234, 64, 48)).unwrap();
        let mut written = sidecar("a lighthouse in fog");
        written.backend = "comfyui".into();
        written.model = "dreamshaperXL_v21.safetensors".into();
        written.seconds = 41.23;
        written.width = 1024;
        written.height = 768;
        written.made_from = "variation of 091200-00007.png".into();
        written.write(&image).unwrap();

        let row = listing(&dir, 1, false).remove(0);
        let value = row.json();
        assert_eq!(value["prompt"], serde_json::json!("a lighthouse in fog"));
        assert_eq!(value["seed"], serde_json::json!(1234));
        assert_eq!(value["backend"], serde_json::json!("comfyui"));
        assert_eq!(value["model"], serde_json::json!("dreamshaperXL_v21.safetensors"));
        // Rounded to a tenth: the seconds are worth knowing and the microseconds are not.
        assert_eq!(value["seconds"], serde_json::json!(41.2));
        assert_eq!(value["size"], serde_json::json!("1024x768"));
        assert_eq!(value["made_from"], serde_json::json!("variation of 091200-00007.png"));
        assert!(value["path"].as_str().unwrap().ends_with("101500-01234.png"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_thumbnail_is_decoded_and_shrunk_to_something_a_grid_can_show() {
        let dir = folder();
        let image = dir.join("big.png");
        std::fs::write(&image, crate::backend::draw("a lighthouse", 1, 640, 480)).unwrap();
        let thumbnail = read_thumbnail(&image).unwrap();
        assert!(thumbnail.width <= THUMBNAIL_EDGE && thumbnail.height <= THUMBNAIL_EDGE);
        assert_eq!(thumbnail.rgb.len(), (thumbnail.width * thumbnail.height * 3) as usize);
        assert_eq!((thumbnail.width, thumbnail.height), (160, 120));
        assert!(read_thumbnail(&dir.join("not-there.png")).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resampling_doubles_a_picture_and_says_so_when_there_is_no_room_to() {
        let dir = folder();
        let image = dir.join("small.png");
        std::fs::write(&image, crate::backend::draw("a lighthouse", 1, 64, 48)).unwrap();
        let bytes = resample(&image, 2, LONGEST_EDGE).unwrap();
        assert_eq!(png_size(&bytes), (Some(128), Some(96)));

        // A ceiling the picture has already reached is reported rather than written back as a
        // copy of itself, which would look like an upscale that did nothing.
        let problem = resample(&image, 2, 64).unwrap_err();
        assert!(problem.contains("64x48"), "{problem}");
        assert!(problem.contains("as large as"), "{problem}");

        // A factor of one is the same picture, and saying so is better than writing a duplicate.
        assert!(resample(&image, 1, LONGEST_EDGE).is_err());
        assert!(resample(&dir.join("nothing.png"), 2, LONGEST_EDGE)
            .unwrap_err()
            .contains("could not be opened"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_filename_that_already_exists_is_not_reused() {
        let dir = folder();
        std::fs::create_dir_all(&dir).unwrap();
        let when = Local.with_ymd_and_hms(2026, 9, 22, 10, 15, 0).unwrap();
        let first = unique_file(&dir, when, 1234);
        assert_eq!(first.file_name().unwrap(), "101500-01234.png");
        std::fs::write(&first, b"x").unwrap();
        let second = unique_file(&dir, when, 1234);
        assert_eq!(second.file_name().unwrap(), "101500-01234-2.png");
        std::fs::write(&second, b"x").unwrap();
        assert_eq!(unique_file(&dir, when, 1234).file_name().unwrap(), "101500-01234-3.png");
        // A sidecar left behind by a failed write is also enough to move the name on, so a
        // picture cannot end up wearing a record that belongs to another one.
        let third = unique_file(&dir, when, 99999);
        assert_eq!(third.file_name().unwrap(), "101500-99999.png");
        std::fs::write(Sidecar::path_for(&third), b"{}").unwrap();
        assert_eq!(unique_file(&dir, when, 99999).file_name().unwrap(), "101500-99999-2.png");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn only_studios_own_gallery_is_inside_studios_own_gallery() {
        let dir = folder();
        let day = dir.join("2026-09-22");
        std::fs::create_dir_all(&day).unwrap();
        let image = day.join("a.png");
        std::fs::write(&image, b"x").unwrap();
        assert!(is_inside(&image, &dir));
        assert!(is_inside(&day, &dir));
        assert!(!is_inside(&dir, &day), "the folder itself is not inside a day of it");
        assert!(!is_inside(Path::new("/etc/passwd"), &dir));
        assert!(!is_inside(&dir.join("nope.png"), &dir));
        // A symlink pointing out of the gallery is still outside it, which is what canonicalize
        // is doing here.
        #[cfg(unix)]
        {
            let link = dir.join("link");
            std::os::unix::fs::symlink("/etc", &link).unwrap();
            assert!(!is_inside(&link.join("passwd"), &dir));
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
