//! What a shell starts with, whatever the app was holding or ignoring when it started it: no
//! descriptor but its own terminal, every signal at its default and none blocked.
//!
//! A test binary of its own, because it changes the whole process's signal dispositions and holds
//! descriptors open on purpose.

use std::path::Path;
use std::sync::Arc;

use yantrik_terminal::Session;

/// Not a shell: `cat` changes no signal and opens nothing, so what it shows is what it was given.
fn start() -> Session {
    Session::spawn_shell("/bin/cat", &[], Path::new("/tmp"), 24, 80, Arc::new(|| {})).unwrap()
}

fn mask(pid: u32, field: &str) -> u64 {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    let line = status.lines().find_map(|l| l.strip_prefix(field)).unwrap();
    u64::from_str_radix(line.trim(), 16).unwrap()
}

#[test]
fn a_shell_starts_clean_whatever_the_app_was_holding_and_ignoring() {
    // A descriptor opened without close-on-exec, as a graphics library does, on a number no shell
    // would pick for itself.
    let leaked = unsafe {
        let fd = libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY);
        assert!(fd >= 0);
        assert_eq!(libc::dup2(fd, 40), 40);
        libc::close(fd);
        40
    };
    // Signals ignored and blocked, as a launch from a non-interactive `&` leaves them.
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_IGN);
        libc::signal(libc::SIGQUIT, libc::SIG_IGN);
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGINT);
        libc::sigaddset(&mut set, libc::SIGQUIT);
        libc::sigaddset(&mut set, libc::SIGUSR1);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
    }

    let session = start();
    let pid = session.pid();
    let open: Vec<String> = std::fs::read_dir(format!("/proc/{pid}/fd"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(!open.contains(&leaked.to_string()), "the shell inherited fd {leaked}: {open:?}");
    let mut open: Vec<u32> = open.iter().filter_map(|f| f.parse().ok()).collect();
    open.sort();
    assert_eq!(open, [0, 1, 2], "only the terminal");

    let ignored = mask(pid, "SigIgn:");
    let blocked = mask(pid, "SigBlk:");
    let bit = |signal: i32| 1u64 << (signal - 1);
    for signal in [libc::SIGINT, libc::SIGQUIT] {
        assert_eq!(ignored & bit(signal), 0, "signal {signal} is ignored in the shell: {ignored:x}");
    }
    assert_eq!(blocked, 0, "no signal is blocked in the shell: {blocked:x}");
    session.shutdown();

    // A shell that cannot be started is still reported: the pipe std uses for that survives.
    let missing = Session::spawn_shell("/no/such/shell", &[], Path::new("/tmp"), 24, 80, Arc::new(|| {}));
    assert!(missing.is_err());
}
