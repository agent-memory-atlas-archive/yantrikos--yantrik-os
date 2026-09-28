use super::check::{ensure_private_as, prepare};
use super::*;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

fn mode(p: &Path) -> u32 {
    std::fs::symlink_metadata(p).unwrap().mode() & 0o777
}

fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap()
}

#[test]
fn makes_the_runtime_dir_private_and_reuses_it() {
    let root = tempfile::tempdir().unwrap();
    let dir = scratch_dir_from(Some(root.path().into()), None).unwrap();
    assert_eq!(dir, canon(root.path()).join(SCRATCH_NAME));
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(std::fs::metadata(&dir).unwrap().uid(), current_uid());
    // A second call finds it and hands back the same place.
    assert_eq!(scratch_dir_from(Some(root.path().into()), None).unwrap(), dir);
}

#[test]
fn scratch_is_never_the_socket_dir() {
    // `$XDG_RUNTIME_DIR/yantrik` holds the service sockets and the apps' pid files; the file tools
    // may write into scratch, so the two must never be one directory.
    let root = tempfile::tempdir().unwrap();
    let dir = scratch_dir_from(Some(root.path().into()), None).unwrap();
    assert_eq!(dir.file_name().unwrap(), "yantrik-scratch");
    assert_ne!(dir, canon(root.path()).join("yantrik"));
    assert!(!root.path().join("yantrik").exists());
}

#[test]
fn tightens_a_loose_directory_of_our_own() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join(SCRATCH_NAME);
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    scratch_dir_from(Some(root.path().into()), None).unwrap();
    assert_eq!(mode(&dir), 0o700);
}

#[test]
fn refuses_a_symlinked_leaf() {
    let root = tempfile::tempdir().unwrap();
    let elsewhere = root.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    let dir = root.path().join(SCRATCH_NAME);
    std::os::unix::fs::symlink(&elsewhere, &dir).unwrap();
    let err = ensure_private_as(&dir, current_uid()).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");
    // And the resolver goes past it to home rather than using it.
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let got = scratch_dir_from(Some(root.path().into()), Some(home.clone())).unwrap();
    assert_eq!(got, canon(&home).join(".cache/yantrik/tmp"));
}

#[test]
fn follows_a_symlinked_intermediate_whose_target_is_trusted() {
    // ~/.cache on another disk is an ordinary machine, not an attack.
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let disk = root.path().join("disk-cache");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&disk).unwrap();
    std::os::unix::fs::symlink(&disk, home.join(".cache")).unwrap();
    let got = scratch_dir_from(None, Some(home)).unwrap();
    assert_eq!(got, canon(&disk).join("yantrik/tmp"));
    assert_eq!(mode(&got), 0o700);
}

#[test]
fn refuses_a_symlinked_intermediate_that_lands_somewhere_loose() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let loose = root.path().join("loose");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&loose).unwrap();
    std::fs::set_permissions(&loose, std::fs::Permissions::from_mode(0o777)).unwrap();
    std::os::unix::fs::symlink(&loose, home.join(".cache")).unwrap();
    assert!(scratch_dir_from(None, Some(home)).is_err());
    // Judged before anything was made inside it.
    assert!(!loose.join("yantrik").exists());
}

#[test]
fn refuses_a_directory_someone_else_owns() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join(SCRATCH_NAME);
    std::fs::create_dir(&dir).unwrap();
    // Not root in a test run, so the stranger is simulated by asking on behalf of another uid.
    let err = ensure_private_as(&dir, current_uid().wrapping_add(1)).unwrap_err();
    assert!(err.to_string().contains("owned by uid"), "{err}");
}

#[test]
fn falls_back_to_home_when_the_runtime_dir_is_missing() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let got = scratch_dir_from(Some(root.path().join("no-such-run")), Some(home.clone())).unwrap();
    assert_eq!(got, canon(&home).join(".cache/yantrik/tmp"));
    assert_eq!(mode(&got), 0o700);
    let got = scratch_dir_from(None, Some(home.clone())).unwrap();
    assert_eq!(got, canon(&home).join(".cache/yantrik/tmp"));
}

#[test]
fn never_falls_back_to_tmp() {
    let err = scratch_dir_from(None, None).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::NotFound);
    assert!(state_dir_from(None, None, "quarantine").is_err());
    // A relative XDG value is ignored rather than resolved against the working directory.
    assert_eq!(absolute(Some("relative/run".into())), None);
}

#[test]
fn refuses_a_base_other_accounts_can_write() {
    for loose_mode in [0o777, 0o770] {
        let root = tempfile::tempdir().unwrap();
        let run = root.path().join("run");
        std::fs::create_dir(&run).unwrap();
        std::fs::set_permissions(&run, std::fs::Permissions::from_mode(loose_mode)).unwrap();
        let home = root.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let got = scratch_dir_from(Some(run.clone()), Some(home.clone())).unwrap();
        assert_eq!(got, canon(&home).join(".cache/yantrik/tmp"), "{loose_mode:o}");
        // Judged before anything was made in it.
        assert!(!run.join(SCRATCH_NAME).exists());
        assert!(scratch_dir_from(Some(run), None).is_err());
    }
}

#[test]
fn refuses_a_loose_directory_on_the_way_down() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    std::fs::create_dir_all(home.join(".cache")).unwrap();
    std::fs::set_permissions(home.join(".cache"), std::fs::Permissions::from_mode(0o777)).unwrap();
    let err = scratch_dir_from(None, Some(home)).unwrap_err();
    assert!(err.to_string().contains("writable by other accounts"), "{err}");
}

#[test]
fn a_home_or_runtime_dir_that_is_tmp_is_refused() {
    // Only meaningful where /tmp is the shared, world-writable directory it usually is.
    let tmp = Path::new("/tmp");
    if std::fs::metadata(tmp).map(|m| m.mode() & 0o002 == 0).unwrap_or(true) {
        return;
    }
    assert!(scratch_dir_from(None, Some(tmp.into())).is_err());
    assert!(scratch_dir_from(Some(tmp.into()), None).is_err());
    assert!(state_dir_from(Some(tmp.into()), None, "quarantine").is_err());
}

#[test]
fn state_dir_is_private_and_named() {
    let root = tempfile::tempdir().unwrap();
    let got = state_dir_from(Some(root.path().into()), None, "quarantine").unwrap();
    assert_eq!(got, canon(root.path()).join("yantrik/quarantine"));
    assert_eq!(mode(&got), 0o700);
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let got = state_dir_from(None, Some(home.clone()), "quarantine").unwrap();
    assert_eq!(got, canon(&home).join(".local/state/yantrik/quarantine"));
}

#[test]
fn prepare_takes_only_plain_relative_paths() {
    let root = tempfile::tempdir().unwrap();
    assert!(prepare(root.path(), Path::new("../escape")).is_err());
    assert!(prepare(root.path(), Path::new("/etc")).is_err());
    assert!(prepare(root.path(), Path::new("")).is_err());
}

#[test]
fn file_names_cannot_climb_out() {
    for bad in ["", ".", "..", "../x", "a/b"] {
        assert!(plain_name(bad).is_err(), "{bad:?}");
    }
    assert_eq!(plain_name("yantrik-see-payload.json").unwrap(), "yantrik-see-payload.json");
}

// ── Creating files ───────────────────────────────────────────────────────────────────────────

#[test]
fn creates_a_private_file_and_empties_it_on_reuse() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("payload.json");
    create_private_file(&path).unwrap().write_all(b"first, and longer").unwrap();
    assert_eq!(mode(&path), 0o600);
    create_private_file(&path).unwrap().write_all(b"second").unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
}

#[test]
fn will_not_write_through_a_link_at_the_name() {
    let root = tempfile::tempdir().unwrap();
    let precious = root.path().join("authorized_keys");
    std::fs::write(&precious, "ssh-ed25519 AAAA").unwrap();
    let planted = root.path().join("notes.pid");
    std::os::unix::fs::symlink(&precious, &planted).unwrap();
    assert!(create_private_file(&planted).is_err());
    assert_eq!(std::fs::read_to_string(&precious).unwrap(), "ssh-ed25519 AAAA");
}

#[test]
fn will_not_empty_a_file_that_has_another_name() {
    let root = tempfile::tempdir().unwrap();
    let precious = root.path().join("authorized_keys");
    std::fs::write(&precious, "ssh-ed25519 AAAA").unwrap();
    let planted = root.path().join("task.out");
    std::fs::hard_link(&precious, &planted).unwrap();
    assert!(create_private_file(&planted).is_err());
    assert_eq!(std::fs::read_to_string(&precious).unwrap(), "ssh-ed25519 AAAA");
}

#[test]
fn a_fifo_at_the_name_is_an_error_not_a_hang() {
    let root = tempfile::tempdir().unwrap();
    let fifo = root.path().join("payload.json");
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: a valid NUL-terminated path; mkfifo touches nothing else.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    assert!(create_private_file(&fifo).is_err());
}
