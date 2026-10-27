//! Writing a deck, or a copy of it, to a file: the framework's save, with yPresent's sentences,
//! and the one place a deck is given a name when nobody chose one.

use std::path::{Path, PathBuf};

use yantrik_app::documents::disk::Disk;
use yantrik_app::documents::files::save as framework;
pub use yantrik_app::documents::files::save::{SaveFailed, SaveJob, Wrote};
use yantrik_app::documents::SaveError;

use super::names::unused_path;
use super::rules::{DeckRules, ExportRules};

/// Where a save with no path of its own goes: the deck's title in the default folder `dir`.
#[derive(Debug, Clone)]
pub struct Unnamed {
    pub title: String,
    pub dir: PathBuf,
}

fn refused(sentence: impl Into<String>) -> Box<SaveFailed> {
    Box::new(SaveFailed { error: SaveError::new(sentence), kept: None })
}

/// The target of a save that has none: the default folder made if it is not there, and a name in
/// it nothing is using. The folder is this app's own fixed place, so making it is not a path an
/// agent chose, and the agent rule still judges the file inside it.
fn choose(unnamed: &Unnamed) -> Result<PathBuf, Box<SaveFailed>> {
    let dir = &unnamed.dir;
    std::fs::create_dir_all(dir).map_err(|e| refused(format!("could not make {}: {e}", dir.display())))?;
    unused_path(dir, &unnamed.title).map_err(refused)
}

/// Whether the regular file at `path` begins with `header`, read without following a link.
fn starts_with(path: &Path, header: &str) -> bool {
    use std::io::Read as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let Ok(file) = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path)
    else {
        return false;
    };
    let mut head = Vec::new();
    file.take(header.len() as u64).read_to_end(&mut head).is_ok() && head == header.as_bytes()
}

/// Saves the deck. With `unnamed`, the name is chosen here, in the job, where the disk may be read.
pub fn save_deck(
    mut job: SaveJob,
    unnamed: Option<Unnamed>,
    disk: &dyn Disk,
) -> Result<Wrote, Box<SaveFailed>> {
    if let Some(unnamed) = &unnamed {
        job.target = choose(unnamed)?;
    }
    framework::save(&DeckRules, &job, disk)
}

/// Writes a copy: Markdown or an outline. A default name beside the deck is one this app made
/// from the deck's own, and a second export of the same deck replaces the first, as it always
/// did; a name the caller gave is never replaced.
///
/// `make` is the folder to make first, when the name is one this app chose in its own.
/// `replaces` is what an earlier export of this deck starts with: a name this app chose replaces
/// a file already there only when that file starts with it, so a person's own `talk.md`, or
/// another deck's export of the same name, is never written over.
pub fn export_copy(
    job: SaveJob,
    make: Option<&Path>,
    replaces: Option<&str>,
    disk: &dyn Disk,
) -> Result<Wrote, Box<SaveFailed>> {
    if let Some(dir) = make {
        std::fs::create_dir_all(dir).map_err(|e| refused(format!("could not make {}: {e}", dir.display())))?;
    }
    if let Some(header) = replaces {
        if let Ok(meta) = std::fs::symlink_metadata(&job.target) {
            if !meta.is_file() || !starts_with(&job.target, header) {
                return Err(refused(format!(
                    "{} is already there and is not an earlier export of this deck, so it was not replaced. Give                      `export_markdown` a path of your own.",
                    job.target.display()
                )));
            }
        }
    }
    framework::save(&ExportRules, &job, disk)
}
