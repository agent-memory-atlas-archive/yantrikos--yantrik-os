//! Proof that a file is the one a download made, and nothing else.
//!
//! The rule: the app changes a file only when it has shown the file is its own, and the showing is
//! done on the same open file descriptor it then writes to. A path can be swapped between a check
//! and an open; a descriptor cannot.
//!
//! What is recorded when the app creates a file, and again each time it has written to it, is a
//! stamp: device, inode, birth time, modification time and size. All of them must match. Device
//! and inode alone are not enough, because a filesystem hands a freed inode to the next file made,
//! so a person who deletes the partial file and saves their own under that name could match. A
//! new file has another birth time, and any rewrite (in place or by replacement) changes the
//! modification time or the size, which the app compares with what it last wrote itself.
//!
//! Where the filesystem gives no birth time the stamp carries `-` in its place and compares equal
//! only to a stamp that also has none; the other four fields then do the work, and a rewrite that
//! restores both the size and the time is the one thing they cannot see. The PR body says so.
//!
//! This is Linux only. Elsewhere no stamp can be taken, so the app never deletes a partial file
//! and never resumes one: it refuses rather than guess.

use super::*;

/// The stamp format. A row saved with another format (an earlier build wrote `dev:ino`) has no
/// stamp this build can check, so it is treated as having none.
const TAG: &str = "v2:";

/// The stamp of the file open as `file`, as text; `None` if it is not a plain file, or the
/// platform cannot say.
pub(super) fn stamp_fd(file: &File) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        sys::stamp(file.as_raw_fd(), c"", libc::AT_EMPTY_PATH)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = file;
        None
    }
}

/// Is this text a stamp this build can check?
pub(super) fn has_stamp(text: &str) -> bool {
    text.starts_with(TAG)
}

/// The size recorded in a stamp: how many bytes the app had written when it took it.
pub(super) fn recorded_size(stamp: &str) -> Option<u64> {
    stamp.strip_prefix(TAG)?.rsplit(':').next()?.parse().ok()
}

/// Open the download's partial file to append, and prove on that descriptor that it is the one
/// the download wrote. `Ok(None)` if nothing is at the path. Keep the descriptor for the whole
/// transfer; do not open the path again.
pub(super) fn open_owned(download: &Download) -> Result<Option<File>, String> {
    let mut options = OpenOptions::new();
    options.append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // `O_NONBLOCK` so a pipe put at the path cannot hold the thread; it is refused below.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    let path = download.path();
    let file = match options.open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    if has_stamp(&download.file_id) && stamp_fd(&file).as_deref() == Some(download.file_id.as_str()) {
        Ok(Some(file))
    } else {
        Err(format!("the file at {} is not provably this download's own", path.display()))
    }
}

/// Delete the file at `path` if, and only if, it still has the stamp `expected`.
///
/// Linux cannot unlink by descriptor, so: open the folder, open the file by name through it
/// without following a link, take its stamp from that descriptor, look at the name again with
/// `statx` just before, and `unlinkat` that name. What remains is the few microseconds between
/// that last look and the unlink, in which a file with the same stamp would have to be put there
/// by someone who can already write to this folder.
pub(super) fn remove_if_ours(path: &Path, expected: &str) -> bool {
    if !has_stamp(expected) {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        sys::unlink_if(path, expected)
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

#[cfg(target_os = "linux")]
mod sys {
    use super::*;
    use std::ffi::{CStr, CString};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;

    /// `statx` on `dirfd`/`name`: the stamp of a plain file, `None` for anything else.
    pub(super) fn stamp(dirfd: i32, name: &CStr, flags: i32) -> Option<String> {
        // SAFETY: `st` is a plain-data out parameter and `name` a NUL-terminated string.
        let mut st: libc::statx = unsafe { std::mem::zeroed() };
        let mask = libc::STATX_BASIC_STATS | libc::STATX_BTIME;
        let rc = unsafe { libc::statx(dirfd, name.as_ptr(), flags, mask, &mut st) };
        if rc != 0 || u32::from(st.stx_mode) & libc::S_IFMT != libc::S_IFREG {
            return None;
        }
        let dev = (u64::from(st.stx_dev_major) << 32) | u64::from(st.stx_dev_minor);
        let birth = if st.stx_mask & libc::STATX_BTIME != 0 {
            format!("{}.{}", st.stx_btime.tv_sec, st.stx_btime.tv_nsec)
        } else {
            "-".to_string()
        };
        Some(format!(
            "{TAG}{dev}:{}:{birth}:{}.{}:{}",
            st.stx_ino, st.stx_mtime.tv_sec, st.stx_mtime.tv_nsec, st.stx_size
        ))
    }

    pub(super) fn unlink_if(path: &Path, expected: &str) -> bool {
        let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else { return false };
        let Ok(name) = CString::new(name.as_bytes()) else { return false };
        let Ok(dir_c) = CString::new(dir.as_os_str().as_bytes()) else { return false };
        // SAFETY: both strings are NUL-terminated; a failed call returns -1, which is checked.
        let dirfd = unsafe { libc::open(dir_c.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) };
        if dirfd < 0 {
            return false;
        }
        let dirfd = unsafe { OwnedFd::from_raw_fd(dirfd) };
        let fd = unsafe {
            libc::openat(
                dirfd.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return false;
        }
        let file = unsafe { File::from_raw_fd(fd) };
        if stamp_fd(&file).as_deref() != Some(expected) {
            return false;
        }
        #[cfg(test)]
        super::seam::run("before_unlink", path);
        if stamp(dirfd.as_raw_fd(), &name, libc::AT_SYMLINK_NOFOLLOW).as_deref() != Some(expected) {
            return false;
        }
        unsafe { libc::unlinkat(dirfd.as_raw_fd(), name.as_ptr(), 0) == 0 }
    }
}

/// A hook the tests use to swap a file at a chosen moment. Keyed by path, so tests that run side
/// by side in their own folders never see each other's.
#[cfg(test)]
pub(super) mod seam {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, OnceLock};

    type Hook = Box<dyn FnOnce() + Send>;
    type Hooks = Mutex<HashMap<(String, PathBuf), Hook>>;

    fn hooks() -> &'static Hooks {
        static HOOKS: OnceLock<Hooks> = OnceLock::new();
        HOOKS.get_or_init(Default::default)
    }

    /// Run `hook` once, the next time the app reaches `point` for `path`.
    pub fn on(point: &str, path: &Path, hook: impl FnOnce() + Send + 'static) {
        hooks().lock().unwrap().insert((point.to_string(), path.to_path_buf()), Box::new(hook));
    }

    pub fn run(point: &str, path: &Path) {
        let hook = hooks().lock().unwrap().remove(&(point.to_string(), path.to_path_buf()));
        if let Some(hook) = hook {
            hook();
        }
    }
}
