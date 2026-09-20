//! A picture, the pictures beside it, and what its file says about it.
//!
//! This logic was inside the shell, reachable only from the Images screen, while
//! `yantrik-image-viewer` shipped as a 24 MB binary that could not open a file at all: it had no
//! argument handling, no control surface, and every navigation callback wrote a log line. The
//! launcher routed "images" to the shell screen instead, so the app was unreachable and nobody
//! noticed it was hollow.
//!
//! One copy, used by both. Nothing here draws or knows about Slint, so it can be tested without
//! a desktop.

use std::path::{Path, PathBuf};

/// The file extensions the viewer will open.
///
/// The list is here rather than in each caller because "is this a picture" was answered in two
/// places that could drift, and a viewer that disagrees with the file browser about what it can
/// open is worse than one that opens nothing.
pub const IMAGE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "webp", "svg", "ico", "tif", "tiff", "avif", "jxl",
];

/// Whether this name looks like a picture this viewer can show.
pub fn is_image(name: &str) -> bool {
    match name.rsplit_once('.') {
        Some((_, ext)) => IMAGE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()),
        None => false,
    }
}

/// The pictures in one folder, and which of them is on screen.
#[derive(Debug, Default, Clone)]
pub struct Gallery {
    images: Vec<PathBuf>,
    index: usize,
}

impl Gallery {
    /// The pictures sitting beside `path`, with `path` itself selected.
    ///
    /// A file that is not there, or a folder that cannot be read, gives a gallery holding just
    /// that path: the viewer then shows one missing picture and says so, rather than opening
    /// empty and leaving the person wondering which file it thought it had.
    pub fn open(path: &Path) -> Self {
        let dir = path.parent().unwrap_or(Path::new("/"));
        let mut images: Vec<PathBuf> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let candidate = entry.path();
                if !candidate.is_file() {
                    continue;
                }
                let name = candidate.file_name().unwrap_or_default().to_string_lossy().to_string();
                if is_image(&name) {
                    images.push(candidate);
                }
            }
        }
        images.sort_by_key(|p| p.file_name().unwrap_or_default().to_ascii_lowercase());

        match images.iter().position(|p| p == path) {
            Some(index) => Self { images, index },
            // Asked for something the folder does not hold: it has gone, or it is not a picture.
            // It stays selected so the viewer names the file that was asked for — falling back to
            // the first picture in the folder showed a different image and said nothing — and the
            // rest of the folder is still there to move through.
            None => {
                images.insert(0, path.to_path_buf());
                Self { images, index: 0 }
            }
        }
    }

    /// A gallery holding nothing, which is what the viewer opens with when it was given no file.
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }

    pub fn len(&self) -> usize {
        self.images.len()
    }

    pub fn index(&self) -> usize {
        self.index
    }

    pub fn paths(&self) -> &[PathBuf] {
        &self.images
    }

    /// The picture on screen.
    pub fn current(&self) -> Option<&PathBuf> {
        self.images.get(self.index)
    }

    /// The next picture, wrapping at the end of the folder.
    pub fn next(&mut self) -> Option<&PathBuf> {
        if self.images.is_empty() {
            return None;
        }
        self.index = (self.index + 1) % self.images.len();
        self.current()
    }

    /// The previous picture, wrapping at the start.
    pub fn prev(&mut self) -> Option<&PathBuf> {
        if self.images.is_empty() {
            return None;
        }
        self.index = if self.index == 0 { self.images.len() - 1 } else { self.index - 1 };
        self.current()
    }

    /// Select a picture by path if this gallery holds it.
    pub fn select(&mut self, path: &Path) -> bool {
        match self.images.iter().position(|p| p == path) {
            Some(i) => {
                self.index = i;
                true
            }
            None => false,
        }
    }

    /// "3 / 12", or nothing at all when there is no folder to count through.
    pub fn counter_text(&self) -> String {
        if self.images.is_empty() {
            String::new()
        } else {
            format!("{} / {}", self.index + 1, self.images.len())
        }
    }
}

/// What the file itself says about a picture. Every field is a string because every one of them
/// is shown as written, and an absent field is empty rather than a placeholder.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ImageInfo {
    pub dimensions: String,
    pub file_size: String,
    pub format: String,
    pub camera: String,
    pub focal_length: String,
    pub iso: String,
    pub exposure: String,
    pub date_taken: String,
    pub gps: String,
}

/// Human file size, at the precision a person reading it cares about.
pub fn format_file_size(bytes: u64) -> String {
    if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

/// Read what the file says about itself. Never fails: a picture with no EXIF is normal, and a
/// file that cannot be read still has a name and a size worth showing.
pub fn read_info(path: &Path) -> ImageInfo {
    let mut info = ImageInfo::default();

    if let Ok(meta) = std::fs::metadata(path) {
        info.file_size = format_file_size(meta.len());
    }
    if let Some(ext) = path.extension() {
        info.format = ext.to_string_lossy().to_uppercase();
    }

    let Ok(file) = std::fs::File::open(path) else { return info };
    let mut reader = std::io::BufReader::new(&file);
    let Ok(exif) = exif::Reader::new().read_from_container(&mut reader) else { return info };

    let dimension = |tag_w: exif::Tag, tag_h: exif::Tag| -> Option<String> {
        let w = exif.get_field(tag_w, exif::In::PRIMARY)?.value.get_uint(0)?;
        let h = exif.get_field(tag_h, exif::In::PRIMARY)?.value.get_uint(0)?;
        (w > 0 && h > 0).then(|| format!("{w} \u{00d7} {h}"))
    };
    info.dimensions = dimension(exif::Tag::PixelXDimension, exif::Tag::PixelYDimension)
        .or_else(|| dimension(exif::Tag::ImageWidth, exif::Tag::ImageLength))
        .unwrap_or_default();

    let text = |tag: exif::Tag| -> String {
        exif.get_field(tag, exif::In::PRIMARY)
            .map(|f| f.display_value().to_string().trim_matches('"').to_string())
            .unwrap_or_default()
    };

    info.camera = text(exif::Tag::Model);
    let make = text(exif::Tag::Make);
    if !make.is_empty() {
        if info.camera.is_empty() {
            info.camera = make;
        } else if !info.camera.starts_with(&make) {
            info.camera = format!("{make} {}", info.camera);
        }
    }

    info.focal_length = text(exif::Tag::FocalLength);
    info.iso = text(exif::Tag::PhotographicSensitivity);
    info.exposure = text(exif::Tag::ExposureTime);
    info.date_taken = match text(exif::Tag::DateTimeOriginal) {
        s if !s.is_empty() => s,
        _ => text(exif::Tag::DateTime),
    };

    let lat = text(exif::Tag::GPSLatitude);
    let lon = text(exif::Tag::GPSLongitude);
    if !lat.is_empty() && !lon.is_empty() {
        info.gps = format!(
            "{lat} {}, {lon} {}",
            text(exif::Tag::GPSLatitudeRef),
            text(exif::Tag::GPSLongitudeRef)
        );
    }

    info
}

/// The dimensions to show when EXIF did not carry them, given what the renderer measured.
pub fn dimensions_from_size(width: u32, height: u32) -> String {
    if width > 0 && height > 0 {
        format!("{width} \u{00d7} {height}")
    } else {
        String::new()
    }
}
