//! The disk seam and the watched files, each rule as a test (design/notes-gate-2026-10-21.md §3).

use std::path::PathBuf;

use super::disk::{Disk, FaultyDisk, RealDisk, Step};
use super::*;

fn fp(text: &str) -> Fingerprint {
    Fingerprint::of_bytes(text.as_bytes())
}

/// One poll's sightings, without the arming each was taken under.
fn polled(watched: &Watched) -> Vec<(String, Observed)> {
    watched.poll().into_iter().map(|(key, seen, _)| (key, seen)).collect()
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("yantrik-documents-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_failure_before_the_rename_leaves_the_file_as_it_was() {
    let dir = scratch("before-rename");
    let path = dir.join("note.md");
    RealDisk.write_atomically(&path, b"old").unwrap();
    let disk = FaultyDisk::new();
    for step in [Step::CreateTemp, Step::Write, Step::SyncFile, Step::Rename] {
        disk.fail(step, "note.md", Some(1));
        let err = disk.write_atomically(&path, b"new").unwrap_err();
        assert_eq!(err.step, step);
        assert_eq!(step.durability(), Durability::NotWritten);
        assert_eq!(std::fs::read(&path).unwrap(), b"old");
    }
    let leftovers: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).collect();
    assert_eq!(leftovers.len(), 1, "no temp file is left behind");
    disk.write_atomically(&path, b"new").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"new", "a fault used up, the disk behaves");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failure_after_the_rename_has_the_bytes_but_not_their_durability() {
    let dir = scratch("after-rename");
    let path = dir.join("note.md");
    let disk = FaultyDisk::new();
    disk.fail(Step::SyncDir, ".md", None);
    let err = disk.write_atomically(&path, b"new").unwrap_err();
    assert_eq!(err.step.durability(), Durability::Unknown);
    assert_eq!(std::fs::read(&path).unwrap(), b"new");
    // A fault scoped to one suffix spares the rest: metadata and content fail apart.
    RealDisk.write_atomically(&dir.join("note.meta"), b"m").unwrap();
    assert!(disk.write_atomically(&dir.join("note.meta"), b"m2").is_ok());
    assert_eq!(disk.hits.lock().unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- the watched files ----

#[test]
fn the_watch_reports_a_change_from_the_baseline_once_and_a_deletion() {
    let dir = scratch("watched");
    let path = dir.join("note.md");
    std::fs::write(&path, "v0").unwrap();
    let watched = Watched::new();
    watched.add("doc-1", path.clone(), Some(Observed::File(fp("v0"))));
    assert!(polled(&watched).is_empty(), "the file it was read from is not a change");
    std::fs::write(&path, "theirs").unwrap();
    assert_eq!(polled(&watched), vec![("doc-1".to_string(), Observed::File(fp("theirs")))]);
    assert!(polled(&watched).is_empty(), "reported once");
    std::fs::remove_file(&path).unwrap();
    assert_eq!(polled(&watched), vec![("doc-1".to_string(), Observed::Gone)]);
    watched.remove("doc-1");
    std::fs::write(&path, "back").unwrap();
    assert!(polled(&watched).is_empty(), "an unwatched file is not reported");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_file_that_cannot_be_looked_at_is_not_a_change() {
    let dir = scratch("unreadable");
    // A path under a plain file cannot be looked at (not a directory): skipped, not "gone".
    std::fs::write(dir.join("plain"), "a file, not a directory").unwrap();
    let watched = Watched::new();
    watched.add("doc-1", dir.join("plain").join("note.md"), Some(Observed::File(fp("v0"))));
    assert!(polled(&watched).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_replace_keeps_the_files_mode_and_a_new_file_is_private() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = scratch("mode");
    let path = dir.join("note.md");
    std::fs::write(&path, "old").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    RealDisk.write_atomically(&path, b"new").unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "a private note stays private");
    let fresh = dir.join("fresh.md");
    RealDisk.write_atomically(&fresh, b"x").unwrap();
    let mode = std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_link_is_not_replaced() {
    let dir = scratch("link");
    let target = dir.join("target.md");
    std::fs::write(&target, "kept").unwrap();
    let link = dir.join("note.md");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let err = RealDisk.write_atomically(&link, b"new").unwrap_err();
    assert_eq!(err.step, Step::Check);
    assert_eq!(err.step.durability(), Durability::NotWritten);
    assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "kept");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_create_never_replaces_what_is_there_and_a_new_file_is_private() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = scratch("create");
    let path = dir.join("note.md");
    RealDisk.create_new(&path, b"first").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"first");
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    let err = RealDisk.create_new(&path, b"second").unwrap_err();
    assert_eq!(err.step, Step::Rename);
    assert_eq!(err.step.durability(), Durability::NotWritten);
    assert_eq!(std::fs::read(&path).unwrap(), b"first", "nothing already there is replaced");
    // Nor a link, even one that points nowhere.
    let link = dir.join("dangling.md");
    std::os::unix::fs::symlink(dir.join("nowhere"), &link).unwrap();
    assert!(RealDisk.create_new(&link, b"x").is_err());
    assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
    let names: Vec<String> =
        std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(names.len(), 2, "no temp file is left behind: {names:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_create_fails_at_the_chosen_step() {
    let dir = scratch("create-faults");
    let path = dir.join("note.md");
    let disk = FaultyDisk::new();
    disk.fail(Step::Write, ".md", Some(1));
    assert_eq!(disk.create_new(&path, b"x").unwrap_err().step, Step::Write);
    assert!(!path.exists(), "a failure before the link leaves nothing");
    disk.fail(Step::SyncDir, ".md", Some(1));
    assert_eq!(disk.create_new(&path, b"x").unwrap_err().step.durability(), Durability::Unknown);
    assert_eq!(std::fs::read(&path).unwrap(), b"x", "after the link the bytes are there");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_large_file_unchanged_is_not_reported_and_a_change_to_it_is() {
    let dir = scratch("large");
    let path = dir.join("big.md");
    let big = vec![b'a'; 17 * 1024 * 1024];
    std::fs::write(&path, &big).unwrap();
    let watched = Watched::new();
    watched.add("doc-1", path.clone(), Some(Observed::File(Fingerprint::of_bytes(&big))));
    assert!(polled(&watched).is_empty(), "compared like with like, at any size");
    assert!(polled(&watched).is_empty());
    let mut changed = big.clone();
    changed.push(b'b');
    std::fs::write(&path, &changed).unwrap();
    assert_eq!(polled(&watched), vec![("doc-1".to_string(), Observed::File(Fingerprint::of_bytes(&changed)))]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_look_taken_before_a_re_arm_is_dropped() {
    let dir = scratch("rearm");
    let path = dir.join("note.md");
    std::fs::write(&path, "v1").unwrap();
    let watched = Watched::new();
    watched.add("doc-1", path.clone(), Some(Observed::File(fp("v0"))));
    assert_eq!(polled(&watched), vec![("doc-1".to_string(), Observed::File(fp("v1")))]);
    // Re-armed at what is there: a look from now on reports only a change from it.
    watched.add("doc-1", path.clone(), Some(Observed::File(fp("v1"))));
    assert!(polled(&watched).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_sighting_names_the_arming_it_was_taken_under() {
    let dir = scratch("arming");
    let path = dir.join("note.md");
    std::fs::write(&path, "v1").unwrap();
    let watched = Watched::new();
    watched.add("doc-1", path.clone(), Some(Observed::File(fp("v0"))));
    let (_, _, arming) = watched.poll().pop().expect("a change");
    assert!(watched.is_current("doc-1", arming));
    watched.add("doc-1", path, Some(Observed::File(fp("v1"))));
    assert!(!watched.is_current("doc-1", arming), "re-armed since");
    assert!(!watched.is_current("doc-2", arming));
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_replace_that_keeps_the_length_and_the_time_is_still_seen() {
    let dir = scratch("same-stamp");
    let path = dir.join("note.md");
    std::fs::write(&path, "aaaa").unwrap();
    let watched = Watched::new();
    watched.add("doc-1", path.clone(), Some(Observed::File(fp("aaaa"))));
    assert!(polled(&watched).is_empty());
    // Another program restores a copy: same length, the old modification time, a new inode.
    let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    let other = dir.join("restore.tmp");
    std::fs::write(&other, "bbbb").unwrap();
    std::fs::File::options().write(true).open(&other).unwrap().set_modified(mtime).unwrap();
    std::fs::rename(&other, &path).unwrap();
    assert_eq!(polled(&watched), vec![("doc-1".to_string(), Observed::File(fp("bbbb")))]);
    let _ = std::fs::remove_dir_all(&dir);
}
