//! Private per-user directories for scratch files and kept state.
//!
//! Everything here exists because `/tmp` is shared. A fixed name there — a payload handed to
//! curl, a screenshot handed to a vision model, a task's output — is a name any other account on
//! the machine can create first: as a symlink, so our write lands on a file of their choosing, or
//! as a file of their own, so what we read back is theirs. Even a name we win the race for is
//! readable by everyone under the default umask, and these files hold page text, screenshots and
//! command output. So nothing of ours goes in `/tmp`, and there is deliberately no fallback to it:
//! when neither a runtime dir nor a home is known, the caller gets an error, not a shared path.
//!
//! A directory is only handed out after checking it is a real directory (not a link), owned by
//! the effective uid, and mode 0700 — whether we just made it or found it already there — and
//! that nothing above it, down from the runtime dir or home, can be written by other accounts.

use std::io;
use std::path::{Path, PathBuf};

/// The directory for short-lived files: `$XDG_RUNTIME_DIR/yantrik`, else `$HOME/.cache/yantrik/tmp`.
///
/// The runtime dir comes first because it is what `/tmp` should have been for this: per user,
/// private, and emptied at logout. It is the same directory the service sockets live in, which is
/// fine — both are ours and 0700, and the socket scan only looks at `app-*.sock`.
pub fn scratch_dir() -> io::Result<PathBuf> {
    scratch_dir_from(env_path("XDG_RUNTIME_DIR"), env_path("HOME"))
}

/// A named file inside [`scratch_dir`]. The name must be a plain file name, not a path.
pub fn scratch_file(name: &str) -> io::Result<PathBuf> {
    Ok(scratch_dir()?.join(plain_name(name)?))
}

/// [`scratch_file`] as a `String`, for the helpers and external commands that take a `&str`. A
/// path that is not UTF-8 is an error, not a lossy guess that names some other file.
pub fn scratch_file_string(name: &str) -> io::Result<String> {
    scratch_file(name)?.into_os_string().into_string().map_err(|p| {
        io::Error::new(io::ErrorKind::InvalidData, format!("scratch path is not UTF-8: {}", p.to_string_lossy()))
    })
}

/// A private directory for state that must outlive the session:
/// `$XDG_STATE_HOME/yantrik/<name>`, else `$HOME/.local/state/yantrik/<name>`.
pub fn state_dir(name: &str) -> io::Result<PathBuf> {
    state_dir_from(env_path("XDG_STATE_HOME"), env_path("HOME"), name)
}

// ── Resolution, with the environment passed in so tests need not mutate it ──────────────────

fn scratch_dir_from(runtime: Option<PathBuf>, home: Option<PathBuf>) -> io::Result<PathBuf> {
    // The runtime dir itself is never created here: if it is set but missing (WSL without
    // systemd, a bare ssh session) the session has no runtime dir, and making one under /run is
    // neither possible for a user nor ours to do.
    if let Some(runtime) = runtime.filter(|r| r.is_dir()) {
        let dir = runtime.join("yantrik");
        match prepare(&runtime, &dir) {
            Ok(()) => return Ok(dir),
            Err(e) => tracing::warn!(dir = %dir.display(), error = %e, "runtime scratch dir refused; using the one under home"),
        }
    }
    let home = home.ok_or_else(|| no_place("neither XDG_RUNTIME_DIR nor HOME is usable"))?;
    let dir = home.join(".cache").join("yantrik").join("tmp");
    prepare(&home, &dir)?;
    Ok(dir)
}

fn state_dir_from(state_home: Option<PathBuf>, home: Option<PathBuf>, name: &str) -> io::Result<PathBuf> {
    let name = plain_name(name)?;
    let (base, dir) = match (state_home, home) {
        (Some(state), _) => (state.clone(), state.join("yantrik").join(name)),
        (None, Some(home)) => (home.clone(), home.join(".local/state/yantrik").join(name)),
        (None, None) => return Err(no_place("neither XDG_STATE_HOME nor HOME is set")),
    };
    prepare(&base, &dir)?;
    Ok(dir)
}

/// Make `dir` (somewhere under `base`) and every directory between them, and check the lot.
///
/// The leaf being ours and 0700 is not enough on its own: whoever can write to a directory above
/// it can rename it away and put a link in its place after we have looked. So `base` is judged
/// before anything is made under it, and every directory from `base` down must be owned by us or
/// by root and not writable by everyone. That is also what turns away a `HOME` or runtime dir
/// that is really `/tmp`, rather than quietly building our private dir inside it.
fn prepare(base: &Path, dir: &Path) -> io::Result<()> {
    let uid = current_uid();
    // `metadata`, following links, for the base alone: a /home that is a link to /usr/home is a
    // normal machine. Below it everything is ours to have made, and a link there is refused.
    trusted(base, &std::fs::metadata(base)?, uid)?;
    make_parent(dir)?;
    ensure_private_as(dir, uid)?;
    let mut between = dir.parent();
    while let Some(p) = between.filter(|p| *p != base && p.starts_with(base)) {
        trusted(p, &std::fs::symlink_metadata(p)?, uid)?;
        between = p.parent();
    }
    Ok(())
}

/// An environment path, only if it is absolute. The XDG spec says a relative value is to be
/// ignored, and a relative path would resolve against whatever the working directory is.
fn env_path(var: &str) -> Option<PathBuf> {
    absolute(std::env::var_os(var))
}

fn absolute(value: Option<std::ffi::OsString>) -> Option<PathBuf> {
    value.map(PathBuf::from).filter(|p| p.is_absolute())
}

fn plain_name(name: &str) -> io::Result<&str> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("not a plain file name: {name:?}")));
    }
    Ok(name)
}

fn make_parent(dir: &Path) -> io::Result<()> {
    match dir.parent() {
        Some(parent) if !parent.is_dir() => std::fs::create_dir_all(parent),
        _ => Ok(()),
    }
}

fn no_place(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, format!("no private directory for yantrik files: {why}"))
}

// ── The check itself ─────────────────────────────────────────────────────────────────────────

#[cfg(unix)]
fn current_uid() -> u32 {
    // SAFETY: geteuid cannot fail and touches no memory.
    unsafe { libc::geteuid() }
}

#[cfg(not(unix))]
fn current_uid() -> u32 {
    0
}

#[cfg(unix)]
fn ensure_private_as(dir: &Path, uid: u32) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

    // Only `mkdir` when it is missing. On an existing directory `mkdir` is still a request to
    // make one, and a Landlock ruleset without MAKE_DIR answers that with EACCES before the
    // kernel gets as far as EEXIST — the trap `socket_dir` in yantrik-ipc-transport fell into.
    if std::fs::symlink_metadata(dir).is_err() {
        match std::fs::DirBuilder::new().mode(0o700).create(dir) {
            Ok(()) => {}
            // Somebody made it between the look and the mkdir; the checks below decide.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    // `symlink_metadata`, not `metadata`: a link to a directory someone else controls must be
    // refused as a link, not followed and judged by where it points.
    let meta = std::fs::symlink_metadata(dir)?;
    let refuse = |why: &str| io::Error::new(io::ErrorKind::PermissionDenied, format!("{}: {why}", dir.display()));
    if meta.file_type().is_symlink() {
        return Err(refuse("is a symlink"));
    }
    if !meta.is_dir() {
        return Err(refuse("is not a directory"));
    }
    if meta.uid() != uid {
        return Err(refuse(&format!("owned by uid {}, not {uid}", meta.uid())));
    }
    // Ours but loose (made under an odd umask, or by an older build): tighten rather than refuse.
    // Skipped when already 0700, so a sandboxed caller is never asked for a chmod it cannot do.
    if meta.mode() & 0o777 != 0o700 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// A directory on the way down to ours: a real directory, owned by us or root, and not one that
/// every account can write to (and so rename our directory out of).
#[cfg(unix)]
fn trusted(p: &Path, meta: &std::fs::Metadata, uid: u32) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let refuse = |why: String| io::Error::new(io::ErrorKind::PermissionDenied, format!("{}: {why}", p.display()));
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(refuse("is not a plain directory".into()));
    }
    if meta.uid() != uid && meta.uid() != 0 {
        return Err(refuse(format!("owned by uid {}, neither us ({uid}) nor root", meta.uid())));
    }
    if meta.mode() & 0o002 != 0 {
        return Err(refuse("is writable by every account".into()));
    }
    Ok(())
}

/// Windows dev builds: the profile's own directories are per user already.
#[cfg(not(unix))]
fn trusted(_p: &Path, _meta: &std::fs::Metadata, _uid: u32) -> io::Result<()> {
    Ok(())
}

#[cfg(not(unix))]
fn ensure_private_as(dir: &Path, _uid: u32) -> io::Result<()> {
    std::fs::create_dir_all(dir)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    fn mode(p: &Path) -> u32 {
        std::fs::symlink_metadata(p).unwrap().mode() & 0o777
    }

    #[test]
    fn makes_the_runtime_dir_private_and_reuses_it() {
        let root = tempfile::tempdir().unwrap();
        let dir = scratch_dir_from(Some(root.path().into()), None).unwrap();
        assert_eq!(dir, root.path().join("yantrik"));
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(std::fs::metadata(&dir).unwrap().uid(), current_uid());
        // A second call finds it and hands back the same place.
        assert_eq!(scratch_dir_from(Some(root.path().into()), None).unwrap(), dir);
    }

    #[test]
    fn tightens_a_loose_directory_of_our_own() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("yantrik");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        scratch_dir_from(Some(root.path().into()), None).unwrap();
        assert_eq!(mode(&dir), 0o700);
    }

    #[test]
    fn refuses_a_symlink() {
        let root = tempfile::tempdir().unwrap();
        let elsewhere = root.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        let dir = root.path().join("yantrik");
        std::os::unix::fs::symlink(&elsewhere, &dir).unwrap();
        let err = ensure_private_as(&dir, current_uid()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");
        // And the resolver goes past it to home rather than using it.
        let home = root.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let got = scratch_dir_from(Some(root.path().into()), Some(home.clone())).unwrap();
        assert_eq!(got, home.join(".cache/yantrik/tmp"));
    }

    #[test]
    fn refuses_a_directory_someone_else_owns() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("yantrik");
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
        assert_eq!(got, home.join(".cache/yantrik/tmp"));
        assert_eq!(mode(&got), 0o700);
        let got = scratch_dir_from(None, Some(home.clone())).unwrap();
        assert_eq!(got, home.join(".cache/yantrik/tmp"));
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
    fn state_dir_is_private_and_named() {
        let root = tempfile::tempdir().unwrap();
        let got = state_dir_from(Some(root.path().into()), None, "quarantine").unwrap();
        assert_eq!(got, root.path().join("yantrik/quarantine"));
        assert_eq!(mode(&got), 0o700);
        let home = root.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let got = state_dir_from(None, Some(home.clone()), "quarantine").unwrap();
        assert_eq!(got, home.join(".local/state/yantrik/quarantine"));
    }

    #[test]
    fn refuses_a_base_every_account_can_write() {
        let root = tempfile::tempdir().unwrap();
        let run = root.path().join("run");
        std::fs::create_dir(&run).unwrap();
        std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o777)).unwrap();
        let home = root.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let got = scratch_dir_from(Some(run.clone()), Some(home.clone())).unwrap();
        assert_eq!(got, home.join(".cache/yantrik/tmp"));
        // Judged before anything was made in it.
        assert!(!run.join("yantrik").exists());
        assert!(scratch_dir_from(Some(run), None).is_err());
    }

    #[test]
    fn refuses_a_loose_directory_on_the_way_down() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        std::fs::create_dir_all(home.join(".cache")).unwrap();
        std::fs::set_permissions(home.join(".cache"), std::fs::Permissions::from_mode(0o777)).unwrap();
        let err = scratch_dir_from(None, Some(home)).unwrap_err();
        assert!(err.to_string().contains("writable by every account"), "{err}");
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
    fn file_names_cannot_climb_out() {
        for bad in ["", ".", "..", "../x", "a/b"] {
            assert!(plain_name(bad).is_err(), "{bad:?}");
        }
        assert_eq!(plain_name("yantrik-see-payload.json").unwrap(), "yantrik-see-payload.json");
    }
}
