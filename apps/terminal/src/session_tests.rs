use super::*;
use std::time::{Duration, Instant};
fn wait(mut condition: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < until, "PTY condition timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn session() -> Session {
    Session::spawn_shell(
        "/bin/sh",
        &["-i"],
        Path::new("/tmp"),
        24,
        80,
        Arc::new(|| {}),
    )
    .unwrap()
}
#[test]
fn persistent_shell_and_interactive_input() {
    let s = session();
    s.write(b"export YANTRIK_TEST=retained; cd /; printf 'READY:%s\\n' \"$YANTRIK_TEST\"\n")
        .unwrap();
    wait(|| s.snapshot().text.contains("READY:retained"));
    s.write(b"printf 'STATE:%s:%s\\n' \"$PWD\" \"$YANTRIK_TEST\"\n")
        .unwrap();
    wait(|| s.snapshot().text.contains("STATE:/:retained"));
    s.write(b"read answer; printf 'ANSWER:%s\\n' \"$answer\"\n")
        .unwrap();
    s.write(b"interactive\n").unwrap();
    wait(|| s.snapshot().text.contains("ANSWER:interactive"));
    assert_eq!(s.cwd(), Path::new("/"));
}
#[test]
fn interrupt_resize_ansi_and_exit() {
    let s = session();
    s.resize(30, 100).unwrap();
    wait(|| s.snapshot().size == (30, 100));
    s.write(b"stty size; printf '\\033[31mRED_OK\\033[0m\\n'; sleep 30\n")
        .unwrap();
    wait(|| {
        s.snapshot().text.contains("30 100")
            && s.snapshot().runs.iter().any(|r| {
                r.text.contains("RED_OK") && r.fg == color(vt100::Color::Idx(1), (0, 0, 0))
            })
    });
    wait(|| s.has_children());
    s.write(b"\x03").unwrap();
    wait(|| !s.has_children());
    s.write(b"printf 'INTERRUPTED_OK\\n'\n").unwrap();
    wait(|| {
        s.snapshot()
            .text
            .lines()
            .any(|l| l.trim() == "INTERRUPTED_OK")
    });
    s.write(b"exit 7\n").unwrap();
    wait(|| !s.alive());
    assert_eq!(s.snapshot().exit, Some(7));
    assert!(s.write(b"x").is_err());
}
#[test]
fn bounded_history_independent_tabs_and_cleanup() {
    let s = session();
    let other = session();
    s.write(
        b"i=0; while [ $i -lt 1200 ]; do echo line-$i; i=$((i+1)); done; echo HISTORY_DONE\n",
    )
    .unwrap();
    wait(|| s.snapshot().text.lines().any(|line| line == "HISTORY_DONE"));
    let history = s.history();
    assert!(history.len() <= HISTORY_LINES + 24);
    assert!(history.iter().any(|l| l == "line-1199"));
    assert!(!other.snapshot().text.contains("line-1199"));
    s.set_scrollback(100);
    assert_eq!(s.snapshot().scrollback, 100);
    assert!(!s.snapshot().cursor_visible);
    let pid = s.pid();
    drop(s);
    wait(|| !Path::new(&format!("/proc/{pid}")).exists());
    assert!(other.alive());
}

/// A process that has nothing to do with a PTY forks while shells are being started, again and
/// again: none of its children may come up holding a PTY master. The master is close-on-exec from
/// the moment it exists, so no fork anywhere in the process can copy it.
#[test]
fn no_forking_thread_ever_inherits_a_pty_master() {
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    let stop = Arc::new(AtomicBool::new(false));
    let children = Arc::new(AtomicUsize::new(0));
    let leaks = Arc::new(Mutex::new(Vec::<String>::new()));
    let forkers: Vec<_> = (0..3)
        .map(|_| {
            let (stop, children, leaks) = (stop.clone(), children.clone(), leaks.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let out = std::process::Command::new("/bin/sh")
                        .args(["-c", "for f in /proc/$$/fd/*; do readlink \"$f\"; done"])
                        .output()
                        .unwrap();
                    children.fetch_add(1, Ordering::Relaxed);
                    let seen = String::from_utf8_lossy(&out.stdout).into_owned();
                    if seen.contains("ptmx") {
                        leaks.lock().unwrap().push(seen);
                    }
                }
            })
        })
        .collect();
    for _ in 0..300 {
        let s = session();
        s.shutdown();
    }
    stop.store(true, Ordering::Relaxed);
    forkers.into_iter().for_each(|t| t.join().unwrap());
    let leaks = leaks.lock().unwrap();
    assert!(leaks.is_empty(), "{} of {} children held a ptmx", leaks.len(), children.load(Ordering::Relaxed));
}
