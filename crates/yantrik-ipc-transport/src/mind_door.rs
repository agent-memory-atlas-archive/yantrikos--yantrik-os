//! The mind door (#411): where minds, running as their own account, reach the desktop.
//!
//! Every mind used to run as the person, so it reached every socket in the person's runtime
//! directory and everything else the person owns: their files, their compositor, their shell's
//! memory. Minds now run as `yantrik-mind`. They cannot open the person's runtime directory
//! (0700), so each service the person runs also listens here, in a directory the person owns and
//! the minds' group may only enter (`/run/yantrik-minds`, 2750 person:yantrik-minds, sockets 0660).
//!
//! On this door the kernel says who is calling (`SO_PEERCRED`): a connection is served only when
//! its uid is the mind account's, and a caller with that uid is a mind wherever it is met. The
//! person's own sockets never admit that uid, so the uid alone answers "is this a mind", with no
//! walk of `/proc` and nothing the caller can say about itself.
//!
//! The directory is made at boot by the updater's tmpfiles entry. When it is missing, or not
//! exactly as described, there is no door: the person's sockets work as before and nothing is
//! served to anybody else.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The mind account and its group.
pub const MIND_USER: &str = "yantrik-mind";
pub const MIND_GROUP: &str = "yantrik-minds";

/// Where the door is, unless `YANTRIK_MIND_RUN` names it (the mind units set it; tests use it).
pub const DEFAULT_DIR: &str = "/run/yantrik-minds";

/// The door directory's path, as named. Not a promise that it exists.
pub fn dir() -> PathBuf {
    std::env::var_os("YANTRIK_MIND_RUN")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_DIR))
}

/// The mind account's uid, if this machine has one.
#[cfg(unix)]
pub fn mind_uid() -> Option<u32> {
    static UID: OnceLock<Option<u32>> = OnceLock::new();
    *UID.get_or_init(|| lookup_user(MIND_USER))
}

/// The minds' group, if this machine has one.
#[cfg(unix)]
pub fn mind_gid() -> Option<u32> {
    static GID: OnceLock<Option<u32>> = OnceLock::new();
    *GID.get_or_init(|| lookup_group(MIND_GROUP))
}

/// Whether a caller with this uid is a mind. The one test every door and every "who is asking"
/// uses.
#[cfg(unix)]
pub fn is_mind(uid: u32) -> bool {
    mind_uid() == Some(uid)
}

#[cfg(not(unix))]
pub fn is_mind(_uid: u32) -> bool {
    false
}

#[cfg(unix)]
fn lookup_user(name: &str) -> Option<u32> {
    let name = std::ffi::CString::new(name).ok()?;
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buf = vec![0 as libc::c_char; 4096];
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: getpwnam_r with a NUL-terminated name, a zeroed passwd and a buffer we own.
    let rc = unsafe { libc::getpwnam_r(name.as_ptr(), &mut pwd, buf.as_mut_ptr(), buf.len(), &mut result) };
    (rc == 0 && !result.is_null()).then_some(pwd.pw_uid)
}

#[cfg(unix)]
fn lookup_group(name: &str) -> Option<u32> {
    let name = std::ffi::CString::new(name).ok()?;
    let mut grp: libc::group = unsafe { std::mem::zeroed() };
    let mut buf = vec![0 as libc::c_char; 8192];
    let mut result: *mut libc::group = std::ptr::null_mut();
    // SAFETY: getgrnam_r with a NUL-terminated name, a zeroed group and a buffer we own.
    let rc = unsafe { libc::getgrnam_r(name.as_ptr(), &mut grp, buf.as_mut_ptr(), buf.len(), &mut result) };
    (rc == 0 && !result.is_null()).then_some(grp.gr_gid)
}

/// What a door directory must be for this process to serve on it: owned by this process's uid,
/// group the minds' group, mode exactly 2750 (the person writes, the minds' group enters, new
/// sockets take the group, nobody else sees in). Anything else and there is no door.
pub fn acceptable(owner: u32, group: u32, mode: u32, me: u32, minds: u32) -> bool {
    owner == me && group == minds && mode & 0o7777 == 0o2750
}

/// The door directory, when this process should serve on it.
#[cfg(unix)]
pub fn serving_dir() -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    mind_uid()?;
    let minds = mind_gid()?;
    let dir = dir();
    let meta = std::fs::metadata(&dir).ok()?;
    // SAFETY: getuid cannot fail.
    let me = unsafe { libc::getuid() };
    if !meta.is_dir() || !acceptable(meta.uid(), meta.gid(), meta.mode(), me, minds) {
        tracing::debug!(dir = %dir.display(), "the mind door is not set up as expected; not serving on it");
        return None;
    }
    Some(dir)
}

#[cfg(not(unix))]
pub fn serving_dir() -> Option<PathBuf> {
    None
}

/// Where the door socket for the service listening at `address` goes: the same file name in the
/// door directory, for a service socket in this session's own socket directory. Anything bound
/// elsewhere (a test's temp path, an explicit address) gets no door.
pub fn door_for(address: &Path, socket_dir: &Path, door: &Path) -> Option<PathBuf> {
    let name = address.file_name()?;
    (address.parent()? == socket_dir).then(|| door.join(name))
}

/// The address a client in a mind's process dials for a service: the door, when this process
/// was told where it is (`YANTRIK_MIND_RUN`); `None` for everyone else, who use their own
/// runtime directory.
pub fn client_address(service_id: &str) -> Option<String> {
    std::env::var_os("YANTRIK_MIND_RUN")
        .filter(|v| !v.is_empty())
        .map(|d| format!("{}/{service_id}.sock", PathBuf::from(d).display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_door_is_served_only_on_a_directory_exactly_as_the_tmpfiles_entry_makes_it() {
        let (me, minds) = (1000, 990);
        assert!(acceptable(me, minds, 0o42750, me, minds), "person:yantrik-minds 2750");
        assert!(!acceptable(0, minds, 0o42750, me, minds), "someone else's directory");
        assert!(!acceptable(me, 100, 0o42750, me, minds), "another group could enter");
        assert!(!acceptable(me, minds, 0o42770, me, minds), "the minds could plant a socket");
        assert!(!acceptable(me, minds, 0o40750, me, minds), "without setgid the sockets are not the group's");
        assert!(!acceptable(me, minds, 0o42755, me, minds), "anyone could look in");
    }

    #[test]
    fn only_a_service_socket_of_this_session_gets_a_door() {
        let run = Path::new("/run/user/1000/yantrik");
        let door = Path::new("/run/yantrik-minds");
        assert_eq!(
            door_for(Path::new("/run/user/1000/yantrik/app-shell.sock"), run, door),
            Some(PathBuf::from("/run/yantrik-minds/app-shell.sock"))
        );
        assert_eq!(door_for(Path::new("/tmp/test-x/app.sock"), run, door), None);
    }
}
