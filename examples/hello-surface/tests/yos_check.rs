//! The example as a person runs it — the `hello-surface` program on its own socket — held to the
//! protocol by the OS's own checker, `yos check`, and driven with `yos act` the way the example's
//! header says to.
//!
//! Each test gets a machine of its own: `HOME` and `XDG_RUNTIME_DIR` in a scratch directory, with
//! the settings this OS ships (a `sensitive` ceiling, `ask` mode), so nothing on the developer's
//! desktop is read or touched. `yos` is `deploy/yantrik-os/yos`, run with `python3`; a machine
//! without `python3` skips these tests and says so, and CI has it.

#![cfg(unix)]

use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

/// A private machine, and the example serving on it.
struct Machine {
    root: PathBuf,
    program: Option<Child>,
}

impl Machine {
    fn new(tag: &str) -> Machine {
        let root = std::env::temp_dir().join(format!("hello-surface-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let config = root.join("home/.config/yantrik");
        std::fs::create_dir_all(&config).expect("a home of our own");
        std::fs::create_dir_all(root.join("run")).expect("a runtime dir of our own");
        std::fs::write(config.join("settings.yaml"), "tool_permission: sensitive\n").unwrap();
        std::fs::write(config.join("mind-mode.json"), r#"{"mode":"ask","session_rules":[]}"#).unwrap();
        Machine { root, program: None }
    }

    fn on_it<'a>(&self, command: &'a mut Command) -> &'a mut Command {
        command.env("HOME", self.root.join("home")).env("XDG_RUNTIME_DIR", self.root.join("run"))
    }

    fn socket(&self) -> PathBuf {
        self.root.join("run/yantrik/app-counter.sock")
    }

    /// Start `hello-surface` and wait until its socket answers.
    fn serve(&mut self) {
        let program = self
            .on_it(&mut Command::new(env!("CARGO_BIN_EXE_hello-surface")))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start hello-surface");
        self.program = Some(program);
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if UnixStream::connect(self.socket()).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("hello-surface never answered on {}", self.socket().display());
    }

    /// `yos <args>` on this machine, or `None` where there is no `python3` to run it with.
    fn yos(&self, args: &[&str]) -> Option<Output> {
        let yos = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../deploy/yantrik-os/yos");
        match self.on_it(Command::new("python3").arg(&yos).args(args)).output() {
            Ok(output) => Some(output),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("skipped: no python3 on this machine to run {}", yos.display());
                None
            }
            Err(e) => panic!("could not run {}: {e}", yos.display()),
        }
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        if let Some(mut program) = self.program.take() {
            let _ = program.kill();
            let _ = program.wait();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn yos_check_finds_nothing_wrong() {
    let mut machine = Machine::new("check");
    machine.serve();
    let Some(out) = machine.yos(&["check", "counter", "--json"]) else { return };
    let report: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("yos check printed no JSON ({e}): {}{}", text(&out.stdout), text(&out.stderr)));
    let rows = report["surfaces"][0]["checks"].as_array().expect("one surface, checked").clone();
    let failed: Vec<_> = rows.iter().filter(|r| r["status"] == "fail").collect();
    assert!(out.status.success() && report["ok"] == true && failed.is_empty(), "yos check counter failed: {failed:#?}");
    // Every check the protocol lists was made, and none was skipped for a reason that would hide
    // a fault (`missing` is skipped honestly: no action of the counter requires an argument).
    for check in ["ping", "describe", "protocol", "schema", "grades", "params", "secrets", "revision", "steady", "method", "empty", "unknown", "undeclared", "types", "stale"] {
        let row = rows.iter().find(|r| r["check"] == check).unwrap_or_else(|| panic!("no `{check}` row: {rows:#?}"));
        assert_eq!(row["status"], "pass", "{row:#?}");
    }
}

#[test]
fn yos_act_drives_it_as_the_header_says() {
    let mut machine = Machine::new("act");
    machine.serve();
    let Some(out) = machine.yos(&["act", "counter", "increment", "by=2"]) else { return };
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(text(&out.stdout).contains("Counter — 2"), "{}", text(&out.stdout));
    assert!(text(&out.stdout).contains("accepted: True, settled: True"), "{}", text(&out.stdout));

    // `by` left out: the declared default, 1.
    let out = machine.yos(&["act", "counter", "increment"]).unwrap();
    assert!(text(&out.stdout).contains("Counter — 3"), "{}", text(&out.stdout));

    // Not an integer: refused before the handler, in the dispatch's words.
    let out = machine.yos(&["act", "counter", "increment", "by=two"]).unwrap();
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("`increment` argument `by` must be an integer, and a string arrived"),
        "{}",
        text(&out.stderr)
    );

    // `reset` is sensitive and this machine is in ask mode: without a person to press Allow, the
    // app itself refuses, and says how a grant is got.
    let out = machine.yos(&["act", "counter", "reset", "--no-ask"]).unwrap();
    assert!(!out.status.success());
    assert!(text(&out.stderr).contains("GRANT: counter.reset is graded `sensitive`"), "{}", text(&out.stderr));
    let out = machine.yos(&["describe", "counter"]).unwrap();
    assert!(text(&out.stdout).contains("Counter — 3"), "the refused reset changed nothing: {}", text(&out.stdout));
}
