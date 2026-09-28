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
//! that nothing on the way down to it from the runtime dir or home can be written by another
//! account. The checks themselves are in `check.rs`.

mod check;
#[cfg(all(test, unix))]
mod tests;

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use check::{current_uid, ensure_private_as, prepare, still_private};

pub use check::create_private_file;

/// The scratch directory's name under `$XDG_RUNTIME_DIR`.
///
/// Not `yantrik`: that is the service socket directory (`socket_dir` in yantrik-ipc-transport),
/// holding companion.sock, vault.sock and the apps' single-instance pid files, which are opened
/// with a plain create that follows links. The file tools may write into scratch — it is where
/// they leave a diagram or a screenshot for the model to open again — so if the two were one
/// directory, a prompt-injected model could plant `notes.pid -> ~/.ssh/authorized_keys` there
/// and have the next app launch truncate it.
pub const SCRATCH_NAME: &str = "yantrik-scratch";

/// The directory for short-lived files: `$XDG_RUNTIME_DIR/yantrik-scratch`, else
/// `$HOME/.cache/yantrik/tmp`. Canonical, so it compares cleanly against resolved paths.
///
/// The runtime dir comes first because it is what `/tmp` should have been for this: per user,
/// private, and emptied at logout.
///
/// The answer is remembered, because `validate_path` asks on every file-tool call; but it is
/// re-checked (still a 0700 directory of ours, not a link) each time before it is handed out, and
/// looked up afresh if it has gone — a runtime dir is emptied at logout, and a process can
/// outlive that.
pub fn scratch_dir() -> io::Result<PathBuf> {
    static CACHED: Mutex<Option<PathBuf>> = Mutex::new(None);
    let mut cached = CACHED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(dir) = cached.as_ref().filter(|dir| still_private(dir)) {
        return Ok(dir.clone());
    }
    let dir = scratch_dir_from(env_path("XDG_RUNTIME_DIR"), env_path("HOME"))?;
    *cached = Some(dir.clone());
    Ok(dir)
}

/// A named file inside [`scratch_dir`]. The name must be a plain file name, not a path.
pub fn scratch_file(name: &str) -> io::Result<PathBuf> {
    Ok(scratch_dir()?.join(plain_name(name)?))
}

/// [`scratch_file`] as a `String`, for the helpers and external commands that take a `&str`. A
/// path that is not UTF-8 is an error, not a lossy guess that names some other file.
pub fn scratch_file_string(name: &str) -> io::Result<String> {
    into_string(scratch_file(name)?)
}

/// Create (or empty) the named scratch file for writing, refusing to go through a link.
/// See [`create_private_file`] for what is refused and why.
pub fn create_scratch(name: &str) -> io::Result<File> {
    create_private_file(&scratch_file(name)?)
}

/// Write `contents` to the named scratch file through [`create_scratch`], and return its path as
/// the string a command line takes (curl's `-d @file`, dot's input).
pub fn write_scratch(name: &str, contents: &[u8]) -> io::Result<String> {
    use std::io::Write;
    let path = scratch_file_string(name)?;
    create_scratch(name)?.write_all(contents)?;
    Ok(path)
}

/// A scratch path for an external program (grim, dot, ffmpeg, piper) to write, with whatever was
/// already at that name removed first.
///
/// We cannot pass `O_NOFOLLOW` to another program's `open`, so the name is cleared instead: a
/// link left there — by an earlier run, or by the model through the file tools — is unlinked, not
/// followed, and the program creates a fresh file of its own.
pub fn scratch_target(name: &str) -> io::Result<String> {
    let path = scratch_file(name)?;
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    into_string(path)
}

/// A private (0700) directory inside [`scratch_dir`], made if missing — for a run that needs a
/// whole directory of its own, such as a script's stand-in `HOME`.
pub fn scratch_subdir(name: &str) -> io::Result<PathBuf> {
    let dir = scratch_dir()?.join(plain_name(name)?);
    ensure_private_as(&dir, current_uid())?;
    Ok(dir)
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
        match prepare(&runtime, Path::new(SCRATCH_NAME)) {
            Ok(dir) => return Ok(dir),
            Err(e) => tracing::warn!(
                runtime = %runtime.display(), error = %e,
                "runtime scratch dir refused; using the one under home"
            ),
        }
    }
    let home = home.ok_or_else(|| no_place("neither XDG_RUNTIME_DIR nor HOME is usable"))?;
    prepare(&home, Path::new(".cache/yantrik/tmp"))
}

fn state_dir_from(state_home: Option<PathBuf>, home: Option<PathBuf>, name: &str) -> io::Result<PathBuf> {
    let name = plain_name(name)?;
    match (state_home, home) {
        (Some(state), _) => prepare(&state, &Path::new("yantrik").join(name)),
        (None, Some(home)) => prepare(&home, &Path::new(".local/state/yantrik").join(name)),
        (None, None) => Err(no_place("neither XDG_STATE_HOME nor HOME is set")),
    }
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

fn into_string(path: PathBuf) -> io::Result<String> {
    path.into_os_string().into_string().map_err(|p| {
        io::Error::new(io::ErrorKind::InvalidData, format!("scratch path is not UTF-8: {}", p.to_string_lossy()))
    })
}

fn no_place(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, format!("no private directory for yantrik files: {why}"))
}
