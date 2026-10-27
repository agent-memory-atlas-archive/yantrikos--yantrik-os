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

/// The browsers started and not yet stopped: process id and profile folder.
static LIVE: Mutex<Vec<(u32, PathBuf)>> = Mutex::new(Vec::new());

/// Has the child receive SIGKILL when the thread that started it ends, which is the app's own end
/// whatever way it comes (a logout, a crash, a kill). The thread is a long-lived pool worker, so
/// the signal does not come early: the kernel ties it to the thread, not to the process.
pub fn die_with_parent(command: &mut Command) {
    #[cfg(unix)]
    {
        // A group of its own, so one signal reaches the browser and every helper it starts.
        std::os::unix::process::CommandExt::process_group(command, 0);
    }
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

/// Note a started browser, so the app ending on purpose can stop it and clear its profile.
pub fn register(pid: u32, profile: &Path) {
    LIVE.lock().unwrap_or_else(|e| e.into_inner()).push((pid, profile.to_path_buf()));
}

/// Note that a browser is being stopped and reaped. Before the reap, never after: a reaped pid can
/// be reused, and `kill_live` must not signal it.
pub fn forget(pid: u32) {
    LIVE.lock().unwrap_or_else(|e| e.into_inner()).retain(|(p, _)| *p != pid);
}

/// Stops the browser started as `pid` and everything it started (its group), at once.
fn kill_group(pid: u32) {
    #[cfg(unix)]
    // SAFETY: kill(2) of the process group of a browser this app started; its group id is its pid.
    unsafe {
        libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
        // And the process itself, for one that is not a group leader.
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
}

/// Stops every browser still running and clears the profile of each: the app is ending. Returns
/// how many were signalled. A profile left for the next start's sweep is only for a kill -9.
pub fn kill_live() -> usize {
    let live: Vec<(u32, PathBuf)> = std::mem::take(&mut *LIVE.lock().unwrap_or_else(|e| e.into_inner()));
    for (pid, _) in &live {
        kill_group(*pid);
    }
    for (_, profile) in &live {
        remove_profile(profile);
    }
    live.len()
}

/// Removes a job's profile. The browser's helper processes may take a moment to stop writing into
/// it after the group is signalled, so a folder that is not empty yet is tried again.
pub fn remove_profile(profile: &Path) {
    for attempt in 0..3 {
        match std::fs::remove_dir_all(profile) {
            Ok(()) => return,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(_) if attempt < 2 => std::thread::sleep(std::time::Duration::from_millis(100)),
            Err(e) => tracing::warn!(profile = %profile.display(), error = %e, "could not remove a browser profile"),
        }
    }
}

/// A headless browser, started for one job. Dropping it stops the browser and its helpers, waits
/// for them, and removes the profile, in that order.
pub struct Headless {
    child: std::process::Child,
    pub ws_url: String,
    pub browser: String,
    profile: PathBuf,
}

impl Headless {
    /// Starts `binary` on `url` with `profile` as its profile, in a group of its own, to die with
    /// the app, with its temporary files inside the profile: Chromium makes
    /// `/tmp/org.chromium.Chromium.*` per run, and there it goes with the profile.
    pub fn start(binary: &Path, url: &str, profile: &Path) -> Result<Headless, String> {
        let tmp = profile.join("tmp");
        let mut command = Command::new(binary);
        command
            // Software rendering: WSL here has no GPU path for headless Chromium.
            .args([
                "--headless=new",
                "--use-gl=angle",
                "--use-angle=swiftshader",
                "--enable-unsafe-swiftshader",
                "--no-sandbox",
                "--disable-dev-shm-usage",
                "--mute-audio",
                "--hide-scrollbars",
                "--window-size=1280,720",
                "--remote-debugging-port=0",
            ])
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg(url)
            .env("TMPDIR", &tmp)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        Headless::spawn(command, binary, profile, &tmp)
    }

    /// The part of [`Headless::start`] that does not know Chromium's flags, so a test can start a
    /// stand-in.
    fn spawn(mut command: Command, binary: &Path, profile: &Path, tmp: &Path) -> Result<Headless, String> {
        let fail = |what: String| {
            remove_profile(profile);
            what
        };
        crate::library::private_folder(tmp).map_err(|e| fail(format!("cannot make the browser's temporary folder: {e}")))?;
        die_with_parent(&mut command);
        let child = command.spawn().map_err(|e| fail(format!("cannot start {}: {e}", binary.display())))?;
        register(child.id(), profile);
        Ok(Headless { child, ws_url: String::new(), browser: binary.display().to_string(), profile: profile.to_path_buf() })
    }
}

impl Drop for Headless {
    fn drop(&mut self) {
        let pid = self.child.id();
        // Off the list before the reap: a reaped pid can be reused, and kill_live must not hit it.
        forget(pid);
        // The helpers (zygote, GPU, renderers) are in the group and keep writing into the profile
        // until they die, so they go first; only then is the profile removed.
        kill_group(pid);
        let _ = self.child.kill();
        let _ = self.child.wait();
        remove_profile(&self.profile);
    }
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
        register(child.id(), &std::env::temp_dir().join("arcade-nonexistent-profile"));
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

    /// A stand-in browser: a main process and a helper in its group that keeps writing into the
    /// profile and into its temporary folder, as Chromium's zygote and renderers do.
    fn stand_in(profile: &Path) -> Result<Headless, String> {
        let tmp = profile.join("tmp");
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("( while :; do mkdir -p \"$TMPDIR/org.chromium.Chromium.$$\"; touch \"$1/cache-$$-$RANDOM\"; sleep 0.005; done ) & sleep 60")
            .arg("fake-browser")
            .arg(profile)
            .env("TMPDIR", &tmp);
        Headless::spawn(command, Path::new("sh"), profile, &tmp)
    }

    /// 20 jobs' worth of a browser whose helpers are still writing when the job ends: none leaves
    /// its profile, or the temporary folder Chromium makes, behind.
    #[test]
    fn twenty_jobs_leave_no_profile_and_no_browser_temporary_folder() {
        let base = std::env::temp_dir().join(format!("arcade-leak-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        for _ in 0..20 {
            let profile = profile_in(&base).unwrap();
            let job = stand_in(&profile).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(40));
            // Other tests end apps, and an app ending stops every live browser, this one's too: so
            // only what is left after the job is asserted.
            drop(job);
            assert!(!profile.exists(), "{} was left behind", profile.display());
        }
        let left: Vec<_> = std::fs::read_dir(&base).unwrap().flatten().map(|e| e.path()).collect();
        assert!(left.is_empty(), "left in the profile folder: {left:?}");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The app ending on purpose (a window close or SIGTERM) clears the profiles of live jobs, and
    /// does not leave them for the next start's sweep.
    #[test]
    fn ending_on_purpose_clears_the_profile_of_a_live_job() {
        let base = std::env::temp_dir().join(format!("arcade-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let profile = profile_in(&base).unwrap();
        let job = stand_in(&profile).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(40));
        kill_live();
        assert!(!profile.exists(), "the live job's profile went with it");
        drop(job);
        let _ = std::fs::remove_dir_all(&base);
    }
}
