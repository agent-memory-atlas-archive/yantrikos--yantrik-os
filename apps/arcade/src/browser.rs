//! The headless browser as a process: how it is started, where its profile lives, and how it is
//! made to die with the app.
//!
//! A verify or a screenshot starts a Chromium with `--remote-debugging-port`, which any local
//! process can drive. Two rules keep one from being left behind or reached: the browser dies with
//! the app (`PR_SET_PDEATHSIG`, and `kill_live` when the app ends on purpose), and each job has a
//! profile of its own, made private and new, never a path another job or another user could
//! predict or share.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// The process ids of the browsers started and not yet stopped.
static LIVE: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// Has the child receive SIGKILL when the thread that started it ends, which is the app's own end
/// whatever way it comes (a logout, a crash, a kill). The thread is a long-lived pool worker, so
/// the signal does not come early: the kernel ties it to the thread, not to the process.
pub fn die_with_parent(command: &mut Command) {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt as _;
        // The parent as it is now. If it dies between the fork and the prctl below, the signal is
        // never armed; the child sees that its parent is no longer this one and ends itself.
        let parent = std::process::id() as libc::pid_t;
        // SAFETY: the closure runs between fork and exec and makes only async-signal-safe calls
        // (prctl, getppid, _exit) and reads errno.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong, 0, 0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    libc::_exit(1);
                }
                Ok(())
            });
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = command;
}

/// Note a started browser, so the app ending on purpose can stop it.
pub fn register(pid: u32) {
    LIVE.lock().unwrap_or_else(|e| e.into_inner()).push(pid);
}

/// Note that a browser has been stopped and reaped.
pub fn forget(pid: u32) {
    LIVE.lock().unwrap_or_else(|e| e.into_inner()).retain(|p| *p != pid);
}

/// Stops every browser still running: the app is ending. Returns how many were signalled.
pub fn kill_live() -> usize {
    let pids: Vec<u32> = std::mem::take(&mut *LIVE.lock().unwrap_or_else(|e| e.into_inner()));
    for pid in &pids {
        #[cfg(unix)]
        // SAFETY: a plain kill(2) of a process this app started.
        unsafe {
            libc::kill(*pid as libc::pid_t, libc::SIGKILL);
        }
    }
    pids.len()
}

/// The folder profiles are made in: under the runtime directory when the session has one, else
/// under the state directory. Never `/tmp`.
pub fn profile_base(runtime: Option<OsString>, state: Option<OsString>, home: Option<OsString>) -> PathBuf {
    if let Some(dir) = runtime.filter(|d| !d.is_empty()).map(PathBuf::from).filter(|d| d.is_dir()) {
        return dir.join("yantrik-arcade");
    }
    let state = state
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(home.unwrap_or_else(|| "/".into())).join(".local/state"));
    state.join("yantrik/arcade/profiles")
}

/// A new private profile for one job: a folder that did not exist, 0700, named by the process, a
/// serial and the clock.
pub fn new_profile() -> Result<PathBuf, String> {
    let base = profile_base(
        std::env::var_os("XDG_RUNTIME_DIR"),
        std::env::var_os("XDG_STATE_HOME"),
        std::env::var_os("HOME"),
    );
    profile_in(&base)
}

pub fn profile_in(base: &Path) -> Result<PathBuf, String> {
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    crate::library::private_folder(base).map_err(|e| format!("cannot make {} private: {e}", base.display()))?;
    sweep_dead(base);
    for _ in 0..8 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let profile = base.join(format!("job-{}-{}-{nanos:09}", std::process::id(), SERIAL.fetch_add(1, Ordering::Relaxed)));
        let mut made = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            made.mode(0o700);
        }
        match made.create(&profile) {
            Ok(()) => return Ok(profile),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("cannot create a profile directory: {e}")),
        }
    }
    Err("cannot find a free profile directory".into())
}

/// Removes the profiles of jobs whose app is gone (it crashed or was killed, so `Drop` never ran):
/// directories named `job-<pid>-...` directly in `base`, for a pid that no longer exists. Nothing
/// else is touched: not a link, not a file, not another name, not our own pid's.
fn sweep_dead(base: &Path) {
    let Ok(entries) = std::fs::read_dir(base) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|n| n.strip_prefix("job-")).and_then(|n| n.split('-').next()).and_then(|p| p.parse::<u32>().ok()) else {
            continue;
        };
        let path = entry.path();
        let plain_dir = std::fs::symlink_metadata(&path).map(|m| m.file_type().is_dir()).unwrap_or(false);
        if plain_dir && pid != std::process::id() && !process_exists(pid) {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

#[cfg(unix)]
fn process_exists(pid: u32) -> bool {
    // SAFETY: signal 0 only asks whether the process exists; EPERM means it does, and is not ours.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) }
}

#[cfg(not(unix))]
fn process_exists(_: u32) -> bool {
    true
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn alive(pid: u32) -> bool {
        // SAFETY: signal 0 only asks whether the process exists.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }

    fn gone_within(pid: u32, secs: u64) -> bool {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        while std::time::Instant::now() < until {
            let mut status = 0;
            // SAFETY: reaps a child of this test process, if it is one.
            let reaped = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
            if reaped == pid as libc::pid_t || !alive(pid) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn a_browser_dies_when_the_thread_that_started_it_ends() {
        let pid = std::thread::spawn(|| {
            let mut sleeper = Command::new("sleep");
            sleeper.arg("60");
            die_with_parent(&mut sleeper);
            sleeper.spawn().unwrap().id()
        })
        .join()
        .unwrap();
        assert!(gone_within(pid, 5), "the child outlived its parent");
    }

    #[test]
    fn ending_on_purpose_stops_every_live_browser() {
        let mut child = Command::new("sleep").arg("60").spawn().unwrap();
        register(child.id());
        // The list is shared with every test that ends an app, so nothing here counts it: this
        // child must be dead, whoever's `kill_live` it was that stopped it.
        kill_live();
        let status = child.wait().unwrap();
        assert!(!status.success(), "killed, not finished");
    }

    #[test]
    fn a_profile_left_by_a_dead_app_is_removed_and_nothing_else_is() {
        let base = std::env::temp_dir().join(format!("arcade-sweep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let mut gone = Command::new("true").spawn().unwrap();
        let dead = gone.id();
        gone.wait().unwrap();
        let own = base.join(format!("job-{}-0-0", std::process::id()));
        let stale = base.join(format!("job-{dead}-3-5"));
        let other_name = base.join(format!("keep-{dead}-3-5"));
        let a_file = base.join(format!("job-{dead}-file"));
        let elsewhere = base.join("elsewhere");
        let link = base.join(format!("job-{dead}-link"));
        for dir in [&own, &stale, &other_name, &elsewhere] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(elsewhere.join("precious"), "x").unwrap();
        std::fs::write(&a_file, "x").unwrap();
        std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
        profile_in(&base).unwrap();
        assert!(!stale.exists(), "the dead app's profile went");
        for kept in [&own, &other_name, &a_file, &elsewhere] {
            assert!(kept.exists(), "{} was not ours to remove", kept.display());
        }
        assert!(std::fs::symlink_metadata(&link).is_ok(), "a link is left alone");
        assert!(elsewhere.join("precious").exists(), "and never followed");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn each_job_gets_a_profile_of_its_own_that_only_the_person_can_enter() {
        let base = std::env::temp_dir().join(format!("arcade-profiles-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let a = profile_in(&base).unwrap();
        let b = profile_in(&base).unwrap();
        assert_ne!(a, b);
        for dir in [&base, &a, &b] {
            assert_eq!(std::fs::metadata(dir).unwrap().permissions().mode() & 0o777, 0o700, "{}", dir.display());
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn profiles_go_under_the_runtime_directory_or_the_state_directory_never_tmp() {
        let runtime = std::env::temp_dir();
        assert_eq!(profile_base(Some(runtime.clone().into()), None, None), runtime.join("yantrik-arcade"));
        let fallback = profile_base(Some("/no/such/runtime".into()), Some("/state".into()), Some("/home/p".into()));
        assert_eq!(fallback, PathBuf::from("/state/yantrik/arcade/profiles"));
        let home = profile_base(None, None, Some("/home/p".into()));
        assert_eq!(home, PathBuf::from("/home/p/.local/state/yantrik/arcade/profiles"));
        assert!(!home.starts_with("/tmp"));
    }
}
