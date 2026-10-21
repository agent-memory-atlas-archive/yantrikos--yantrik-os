//! The replace that keeps what it replaced ([`super::disk::Disk::exchange`] on the real disk).
//!
//! On Linux the new file and the old trade names in one step (`renameat2(RENAME_EXCHANGE)`),
//! so what was at the path at that instant is kept at the temp name instead of being unlinked
//! unseen. Three rules keep that step from doing harm the plain rename never did:
//!
//! - Only a regular file is replaced. The exchange would trade places with a directory, a FIFO
//!   or a link as readily as with a file (a plain rename refuses a directory), so anything else
//!   found at the temp name afterwards is swapped straight back and the save fails as before.
//! - What was replaced is read back without following a link, without blocking on a FIFO, and
//!   only up to a limit. A file that cannot be read back so is left under its temp name
//!   ([`Displaced::Unread`]), never deleted unread.
//! - When nothing was at the path, the file is published with `RENAME_NOREPLACE`, so a file
//!   another program made in that instant is exchanged with, not replaced.
//!
//! Where the kernel or the filesystem has no such rename (NFS, 9p, FUSE, an old kernel, a
//! sandbox that forbids it), a plain rename is used and the answer is [`Displaced::Unknown`].

use std::io::{Read as _, Write as _};
use std::path::Path;

use super::disk::{DiskError, Displaced, Exchanged, RealDisk, Step};

/// Read back at most this much of a replaced file; a larger one is kept under its temp name.
pub(super) const READ_BACK_LIMIT: u64 = 64 * 1024 * 1024;

/// How many times a file appearing in the instant before a no-replace publish is exchanged with.
const PUBLISH_TRIES: usize = 3;

pub(super) fn exchange(path: &Path, bytes: &[u8]) -> Result<Exchanged, DiskError> {
    let fail = |step: Step| move |e: std::io::Error| DiskError { step, error: e.to_string() };
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(DiskError { step: Step::Check, error: "the file is a link, and is not replaced".into() });
    }
    let (temp, mut file) = RealDisk::create_temp(path, true).map_err(fail(Step::CreateTemp))?;
    let written = file
        .write_all(bytes)
        .map_err(fail(Step::Write))
        .and_then(|()| file.sync_all().map_err(fail(Step::SyncFile)));
    drop(file);
    if let Err(e) = written {
        let _ = std::fs::remove_file(&temp);
        return Err(e);
    }
    let displaced = swap_in(&temp, path, READ_BACK_LIMIT)?;
    let parent = path.parent().unwrap_or(Path::new("."));
    // The new bytes are in place. A failed directory sync makes them unsure, and what they
    // replaced is still handed back: it exists nowhere else.
    let unsure = std::fs::File::open(parent).and_then(|d| d.sync_all()).map_err(fail(Step::SyncDir)).err();
    Ok(Exchanged { displaced, unsure })
}

/// Puts the written `temp` at `path` and says what it took the place of. On an error nothing at
/// `path` was replaced and `temp` is gone.
pub(super) fn swap_in(temp: &Path, path: &Path, limit: u64) -> Result<Displaced, DiskError> {
    let fail = |e: std::io::Error| DiskError { step: Step::Rename, error: e.to_string() };
    match place(temp, path) {
        Err(e) => {
            let _ = std::fs::remove_file(temp);
            Err(fail(e))
        }
        Ok(Placed::Created) => Ok(Displaced::Nothing),
        Ok(Placed::Renamed) => Ok(Displaced::Unknown),
        Ok(Placed::Exchanged) => {
            let regular = std::fs::symlink_metadata(temp).is_ok_and(|m| m.file_type().is_file());
            if regular {
                return Ok(read_back(temp, limit));
            }
            // A directory, a FIFO, a device or a link was at the path: put it back.
            match exchange_names(temp, path) {
                Ok(()) => {
                    let _ = std::fs::remove_file(temp);
                    Err(DiskError {
                        step: Step::Rename,
                        error: "what is at the path is not a regular file, and is not replaced".into(),
                    })
                }
                // Could not put it back: ours is in place, and theirs stays under the temp name.
                Err(_) => Ok(Displaced::Unread(temp.to_path_buf())),
            }
        }
    }
}

/// The replaced file's bytes, the temp removed; or the temp kept, unread, when it cannot be read
/// back safely (opened without following a link or blocking, a regular file, within `limit`).
fn read_back(temp: &Path, limit: u64) -> Displaced {
    let bytes = (|| {
        let file = open_quietly(temp)?;
        let meta = file.metadata()?;
        if !meta.is_file() || meta.len() > limit {
            return Err(std::io::Error::other("not a regular file within the limit"));
        }
        let mut bytes = Vec::new();
        file.take(limit + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > limit {
            return Err(std::io::Error::other("grew past the limit"));
        }
        Ok(bytes)
    })();
    match bytes {
        Ok(bytes) => {
            let _ = std::fs::remove_file(temp);
            Displaced::Bytes(bytes)
        }
        Err(_) => Displaced::Unread(temp.to_path_buf()),
    }
}

#[cfg(unix)]
fn open_quietly(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
}

#[cfg(not(unix))]
fn open_quietly(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
}

/// How the new file took its place.
enum Placed {
    /// Exchanged with what was at the path, which is now at the temp name.
    Exchanged,
    /// Nothing was at the path, and nothing was replaced.
    Created,
    /// Renamed over whatever was there, unseen: no exchange on this filesystem.
    Renamed,
}

fn place(temp: &Path, path: &Path) -> std::io::Result<Placed> {
    for _ in 0..PUBLISH_TRIES {
        match exchange_names(temp, path) {
            Ok(()) => return Ok(Placed::Exchanged),
            Err(e) if is_absent(&e) => match publish_new(temp, path) {
                Ok(()) => return Ok(Placed::Created),
                // Made by someone else in that instant: exchange with it instead.
                Err(e) if is_present(&e) => continue,
                Err(e) if unsupported(&e) => break,
                Err(e) => return Err(e),
            },
            Err(e) if unsupported(&e) => break,
            Err(e) => return Err(e),
        }
    }
    std::fs::rename(temp, path)?;
    Ok(Placed::Renamed)
}

#[cfg(target_os = "linux")]
fn exchange_names(a: &Path, b: &Path) -> std::io::Result<()> {
    rename2(a, b, libc::RENAME_EXCHANGE)
}

#[cfg(target_os = "linux")]
fn publish_new(temp: &Path, path: &Path) -> std::io::Result<()> {
    rename2(temp, path, libc::RENAME_NOREPLACE)
}

#[cfg(target_os = "linux")]
fn rename2(a: &Path, b: &Path, flags: libc::c_uint) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;
    let a = CString::new(a.as_os_str().as_bytes())?;
    let b = CString::new(b.as_os_str().as_bytes())?;
    // SAFETY: both strings are valid and NUL-terminated for the call, which only reads them.
    // The raw syscall is used because not every libc this OS runs on wraps renameat2.
    let r = unsafe { libc::syscall(libc::SYS_renameat2, libc::AT_FDCWD, a.as_ptr(), libc::AT_FDCWD, b.as_ptr(), flags) };
    if r == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
fn is_absent(e: &std::io::Error) -> bool {
    e.raw_os_error() == Some(libc::ENOENT)
}

#[cfg(target_os = "linux")]
fn is_present(e: &std::io::Error) -> bool {
    e.raw_os_error() == Some(libc::EEXIST)
}

/// No such rename here: a filesystem without it (EINVAL), a kernel without it (ENOSYS), or a
/// sandbox that forbids the call (EPERM; a real permission error fails the plain rename too).
#[cfg(target_os = "linux")]
fn unsupported(e: &std::io::Error) -> bool {
    matches!(e.raw_os_error(), Some(libc::EINVAL) | Some(libc::ENOSYS) | Some(libc::EPERM))
}

#[cfg(not(target_os = "linux"))]
fn exchange_names(_: &Path, _: &Path) -> std::io::Result<()> {
    Err(std::io::ErrorKind::Unsupported.into())
}

#[cfg(not(target_os = "linux"))]
fn publish_new(_: &Path, _: &Path) -> std::io::Result<()> {
    Err(std::io::ErrorKind::Unsupported.into())
}

#[cfg(not(target_os = "linux"))]
fn is_absent(_: &std::io::Error) -> bool {
    false
}

#[cfg(not(target_os = "linux"))]
fn is_present(_: &std::io::Error) -> bool {
    false
}

#[cfg(not(target_os = "linux"))]
fn unsupported(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::Unsupported
}
