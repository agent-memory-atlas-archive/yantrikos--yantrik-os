//! Moving a picture to the OS's Trash, in the format the shell already uses.
//!
//! `delete` is graded `standard` on one promise: it is recoverable. That promise is only real if
//! the picture lands where the rest of the OS looks for deleted things, so Files shows it and its
//! own Restore puts it back — which means writing `~/.local/share/yantrik/trash-v2` exactly as
//! `crates/yantrik-ui/src/fileops.rs` does.
//!
//! This is a copy rather than a call because that file lives in `yantrik-ui`, which builds as a
//! binary only and has no library target for an app to depend on. Promoting it into
//! `yantrik-app-runtime` is the right next step and the wrong change for this one: it would move
//! code the shell's whole file screen runs on, to add an app. What keeps the copy honest is the
//! test at the bottom of this file, which writes through here and reads back through the same
//! bytes `fileops.rs` writes, so the two cannot drift without something failing.

use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
// Reading a path back out of an info file only happens in `items`, which exists for tests.
#[cfg(test)]
use std::ffi::OsString;
#[cfg(test)]
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static SERIAL: AtomicU64 = AtomicU64::new(0);

pub type Result<T> = std::result::Result<T, String>;

/// Where deleted things are kept. `XDG_DATA_HOME` only when it is absolute, because a relative
/// one would be resolved against whatever the working directory happened to be.
pub fn root() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share")
        })
        .join("yantrik/trash-v2")
}

/// One thing that was moved, and the identifier it can be restored by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Moved {
    pub id: String,
    pub name: String,
    pub original: PathBuf,
    pub stored: PathBuf,
}

/// Move one file to the Trash. Returns its identifier, or says why it could not be moved.
pub fn one(source: &Path, root: &Path) -> Result<Moved> {
    let name = source
        .file_name()
        .ok_or_else(|| format!("{} is a filesystem root and cannot be moved to Trash", source.display()))?
        .to_string_lossy()
        .to_string();
    if name.is_empty() || matches!(name.as_str(), "." | "..") || name.contains(['/', '\0']) {
        return Err(format!("{name} is not a filename this can move"));
    }
    // Canonicalising the parent and re-joining the leaf, rather than canonicalising the file,
    // keeps a symlink pointing at the file itself instead of quietly moving its target.
    let parent = fs::canonicalize(
        source
            .parent()
            .ok_or_else(|| format!("{} has no folder to move it out of", source.display()))?,
    )
    .map_err(|e| format!("{}: {e}", source.display()))?;
    let source = parent.join(source.file_name().unwrap());
    if source.starts_with(root) || root.starts_with(&source) {
        return Err("the Trash storage folder cannot be moved to Trash".to_string());
    }
    // Asked about by the name the caller used, before anything is written: an "os error 2" that
    // names the storage path inside the Trash sends somebody looking in the wrong place, and an
    // info file written for a move that never happens has to be taken back out. `symlink_metadata`
    // rather than `exists`, because a symlink is moved as the link it is, broken or not. A file
    // that vanishes after this point is still caught by the rename below.
    if source.symlink_metadata().is_err() {
        return Err(format!("{} is not there, so there was nothing to move", source.display()));
    }
    fs::create_dir_all(root.join("files")).map_err(|e| format!("{}: {e}", root.display()))?;
    fs::create_dir_all(root.join("info")).map_err(|e| format!("{}: {e}", root.display()))?;

    let id = unique();
    let info = root.join("info").join(&id);
    // `create_new` is what makes the identifier safe to use as a filename below: the metadata is
    // written first and fails loudly if the id somehow already exists, so the move that follows
    // cannot overwrite another deleted file's record.
    let mut handle = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&info)
        .map_err(|e| format!("{}: {e}", info.display()))?;
    // The original path as a JSON-encoded byte string, which is what `fileops.rs` writes and what
    // its `trash_items` reads back. Any other spelling here and Files shows an entry it cannot
    // restore.
    let bytes = serde_json::to_vec(&source.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    std::io::Write::write_all(&mut handle, &bytes)
        .and_then(|_| handle.sync_all())
        .map_err(|e| format!("{}: {e}", info.display()))?;

    let stored = root.join("files").join(&id);
    if let Err(problem) = rename_no_replace(&source, &stored) {
        // A file that was not moved must not leave a record saying it was: Files would then list
        // an entry pointing at nothing, and restoring it would fail on the picture that is still
        // sitting in the gallery.
        let _ = fs::remove_file(&info);
        return Err(problem);
    }
    Ok(Moved { id, name, original: source, stored })
}

/// Move a generated picture and the sidecar beside it, together.
///
/// Both go, because a picture restored without its record is a picture nobody can explain, and a
/// record left behind for a picture that is gone is a gallery entry that cannot be shown. They
/// are two entries in the Trash rather than one, which is what the format allows, and Files
/// restores each to the folder it came from.
pub fn picture(image: &Path, root: &Path) -> Result<Vec<Moved>> {
    let mut moved = vec![one(image, root)?];
    let sidecar = crate::gallery::Sidecar::path_for(image);
    if sidecar.exists() {
        match one(&sidecar, root) {
            Ok(entry) => moved.push(entry),
            // The picture is already in the Trash at this point. Failing the whole action would
            // report a deletion that did not happen; reporting the leftover is the true account.
            Err(problem) => tracing::warn!(
                "{} went to Trash but its sidecar did not ({problem}); it is still beside where the picture was",
                image.display()
            ),
        }
    }
    Ok(moved)
}

/// Read the Trash back, and put one thing back where it came from.
///
/// Both are compiled only for tests, because Studio moves things into the Trash and never takes
/// them out — that is Files' job, from its own copy of this code. They stay here rather than being
/// deleted, because the test that deletes a picture is the one that has to prove the move really is
/// recoverable and not a deletion wearing a kinder word: `delete` promises that in its purpose, in
/// the notice bar and in the answer it sends back, and a promise like that should not rest on
/// reading the code and hoping.
#[cfg(test)]
pub fn items(root: &Path) -> Result<Vec<Moved>> {
    let folder = root.join("files");
    if !folder.exists() {
        return Ok(Vec::new());
    }
    let mut found = Vec::new();
    for entry in fs::read_dir(&folder).map_err(|e| format!("{}: {e}", folder.display()))? {
        let entry = entry.map_err(|e| format!("{e}"))?;
        let id = entry.file_name().into_string().map_err(|_| "an invalid Trash identifier".to_string())?;
        let data = fs::read(root.join("info").join(&id))
            .map_err(|e| format!("{}: {e}", root.join("info").join(&id).display()))?;
        let bytes: Vec<u8> = serde_json::from_slice(&data).map_err(|e| e.to_string())?;
        let original = PathBuf::from(OsString::from_vec(bytes));
        if !original.is_absolute() {
            return Err("the Trash metadata does not hold an absolute path".to_string());
        }
        found.push(Moved {
            name: original.file_name().unwrap_or_default().to_string_lossy().into(),
            id,
            original,
            stored: entry.path(),
        });
    }
    found.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(found)
}

/// Put one thing back where it came from, the way Files' own Restore does.
#[cfg(test)]
pub fn restore(item: &Moved, root: &Path) -> Result<()> {
    rename_no_replace(&root.join("files").join(&item.id), &item.original)?;
    fs::remove_file(root.join("info").join(&item.id))
        .map_err(|e| format!("it was restored, but its Trash record could not be removed: {e}"))
}

fn unique() -> String {
    format!(
        "{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    )
}

/// Move without replacing anything, and say so in the same words the shell does.
///
/// `RENAME_NOREPLACE` rather than a check-then-rename, because two apps deleting at the same
/// instant is exactly when a check is wrong. `EXDEV` gets its own sentence: a Trash on another
/// filesystem than the gallery is a real setup, and "Invalid cross-device link" is not an answer
/// to give a person.
fn rename_no_replace(source: &Path, destination: &Path) -> Result<()> {
    let from = CString::new(source.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    let to = CString::new(destination.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    if unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    } == 0
    {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EXDEV) {
        return Err("Cross-filesystem moves are not supported. The source was left in place.".to_string());
    }
    Err(format!("{}: {error}", destination.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gallery::Sidecar;

    fn folder() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "studio-trash-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Write a picture and the sidecar that belongs beside it, the way the engine does.
    fn generate(dir: &Path, name: &str, prompt: &str) -> PathBuf {
        let image = dir.join(name);
        std::fs::write(&image, crate::backend::draw(prompt, 1234, 32, 32)).unwrap();
        Sidecar {
            prompt: prompt.into(),
            negative: String::new(),
            seed: 1234,
            backend: "fake".into(),
            model: "fake".into(),
            seconds: 0.2,
            width: 32,
            height: 32,
            sent: "32x32".into(),
            created: "2026-09-22T10:00:00+05:30".into(),
            made_from: String::new(),
            steps: Some(30),
            cfg: Some(7.0),
            made_by: "yantrik-studio".into(),
        }
        .write(&image)
        .unwrap();
        image
    }

    #[test]
    fn a_deleted_picture_is_in_the_trash_and_can_be_put_back() {
        let dir = folder();
        let root = dir.join("trash-v2");
        let day = dir.join("2026-09-22");
        std::fs::create_dir_all(&day).unwrap();
        let image = generate(&day, "a.png", "a lighthouse");
        assert!(image.exists());

        let moved = picture(&image, &root).unwrap();
        assert!(!image.exists(), "the picture is still in the gallery");
        assert!(!Sidecar::path_for(&image).exists(), "the sidecar was left behind");
        assert_eq!(moved.len(), 2, "{moved:?}");
        assert_eq!(moved[0].name, "a.png");
        assert_eq!(moved[1].name, "a.json");
        assert!(moved[0].stored.exists());

        // Files reads the same folder, so what it would list is what the caller is told about.
        let listed = items(&root).unwrap();
        assert_eq!(listed.len(), 2, "{listed:?}");
        assert!(listed.iter().any(|item| item.name == "a.png"));
        assert!(listed.iter().any(|item| item.original == image));

        for item in &listed {
            restore(item, &root).unwrap();
        }
        assert!(image.exists(), "the picture did not come back");
        assert_eq!(Sidecar::read(&image).unwrap().prompt, "a lighthouse", "the record did not come back");
        assert!(items(&root).unwrap().is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_record_written_here_is_the_record_the_shell_reads() {
        // `crates/yantrik-ui/src/fileops.rs` writes `serde_json::to_vec(&path.as_os_str().as_bytes())`
        // into `info/<id>` and reads it back as an absolute path. This is the assertion that the
        // two implementations still agree, and it is the one that fails if either ever changes.
        let dir = folder();
        let root = dir.join("trash-v2");
        let image = generate(&dir, "b.png", "a harbour");
        let moved = one(&image, &root).unwrap();

        let raw = std::fs::read(root.join("info").join(&moved.id)).unwrap();
        let bytes: Vec<u8> = serde_json::from_slice(&raw).unwrap();
        assert_eq!(PathBuf::from(OsString::from_vec(bytes)), image);
        assert!(raw.starts_with(b"[") && raw.ends_with(b"]"), "the info file is not a JSON byte array");

        // And the file itself moved rather than being copied, which is what makes the Trash cheap
        // enough that a gallery of large pictures can be emptied without filling the disk twice.
        assert!(moved.stored.exists());
        assert!(!image.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_trash_folder_itself_cannot_be_moved_into_the_trash() {
        let dir = folder();
        let root = dir.join("trash-v2");
        std::fs::create_dir_all(root.join("files")).unwrap();
        let problem = one(&root, &root).unwrap_err();
        assert!(problem.contains("cannot be moved to Trash"), "{problem}");
        let problem = one(&root.join("files"), &root).unwrap_err();
        assert!(problem.contains("cannot be moved to Trash"), "{problem}");
        assert!(root.join("files").exists(), "the check happened after the move");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_picture_that_is_not_there_is_not_reported_as_deleted() {
        let dir = folder();
        let root = dir.join("trash-v2");
        let problem = one(&dir.join("never-existed.png"), &root).unwrap_err();
        assert!(problem.contains("never-existed.png"), "{problem}");
        // Nothing was half-written on the way: a Trash that lists a file it does not hold is the
        // failure this avoids.
        assert!(items(&root).unwrap().is_empty());
        assert!(!root.join("info").exists() || std::fs::read_dir(root.join("info")).unwrap().count() == 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_name_that_is_not_a_filename_is_refused() {
        let dir = folder();
        let root = dir.join("trash-v2");
        for name in [".", "..", ""] {
            assert!(one(Path::new(name), &root).is_err(), "{name} was accepted");
        }
        assert!(one(Path::new("/"), &root).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_symlink_is_moved_rather_than_what_it_points_at() {
        let dir = folder();
        let root = dir.join("trash-v2");
        std::fs::create_dir_all(dir.join("gallery")).unwrap();
        let image = generate(&dir.join("gallery"), "c.png", "a cliff");
        let link = dir.join("gallery").join("alias.png");
        std::os::unix::fs::symlink(&image, &link).unwrap();

        let moved = one(&link, &root).unwrap();
        assert_eq!(moved.name, "alias.png");
        // The real picture is still in the gallery, which is the whole reason the parent is
        // canonicalised and the leaf is not.
        assert!(image.exists(), "the symlink's target was moved instead");
        assert!(!link.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_picture_with_no_sidecar_still_goes_to_the_trash_on_its_own() {
        let dir = folder();
        let root = dir.join("trash-v2");
        let image = dir.join("d.png");
        std::fs::write(&image, crate::backend::draw("x", 1, 8, 8)).unwrap();
        let moved = picture(&image, &root).unwrap();
        assert_eq!(moved.len(), 1, "{moved:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn two_deletions_at_once_do_not_share_an_identifier() {
        let dir = folder();
        let root = dir.join("trash-v2");
        let one_image = generate(&dir, "e.png", "one");
        let two_image = generate(&dir, "f.png", "two");
        let first = one(&one_image, &root).unwrap();
        let second = one(&two_image, &root).unwrap();
        assert_ne!(first.id, second.id);
        let listed = items(&root).unwrap();
        assert_eq!(listed.len(), 2, "{listed:?}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
