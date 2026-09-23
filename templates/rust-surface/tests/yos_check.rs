//! The program as the desktop runs it, held to the protocol by the OS's own checker: `my-surface`
//! served on a private machine, and `yos check my-surface` run against it.
//!
//! `yos` is looked for where it lives: `$YOS` if set, `deploy/yantrik-os/yos` inside the
//! yantrik-os repository, `/opt/yantrik/bin/yos` on a Yantrik machine, then `PATH`. Where there is
//! none the test says so and passes without checking — on a Yantrik machine, and in the
//! repository's CI, there always is one.

#![cfg(unix)]

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use yantrik_surface::serde_json::{self, Value};

/// A private machine: `HOME` and `XDG_RUNTIME_DIR` of the test's own, with the settings this OS
/// ships (a `sensitive` ceiling, `ask` mode), and the program serving on it.
struct Machine {
    root: PathBuf,
    program: Option<Child>,
}

impl Machine {
    fn new() -> Machine {
        let root = std::env::temp_dir().join(format!("my-surface-check-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let config = root.join("home/.config/yantrik");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir_all(root.join("run")).unwrap();
        std::fs::write(config.join("settings.yaml"), "tool_permission: sensitive\n").unwrap();
        std::fs::write(config.join("mind-mode.json"), r#"{"mode":"ask","session_rules":[]}"#).unwrap();
        Machine { root, program: None }
    }

    fn on_it<'a>(&self, command: &'a mut Command) -> &'a mut Command {
        command.env("HOME", self.root.join("home")).env("XDG_RUNTIME_DIR", self.root.join("run"))
    }

    fn serve(&mut self) {
        let program = self
            .on_it(&mut Command::new(env!("CARGO_BIN_EXE_my-surface")))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start my-surface");
        self.program = Some(program);
        let socket = self.root.join("run/yantrik/app-my-surface.sock");
        let deadline = Instant::now() + Duration::from_secs(20);
        while UnixStream::connect(&socket).is_err() {
            assert!(Instant::now() < deadline, "my-surface never answered on {}", socket.display());
            std::thread::sleep(Duration::from_millis(50));
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

/// How to run `yos` here, or `None`.
fn yos() -> Option<Command> {
    let in_repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/yantrik-os/yos");
    let candidates = [std::env::var_os("YOS").map(PathBuf::from), Some(in_repo), Some("/opt/yantrik/bin/yos".into())];
    for path in candidates.into_iter().flatten() {
        if path.is_file() {
            let mut command = Command::new("python3");
            command.arg(path);
            return Some(command);
        }
    }
    Command::new("yos").arg("--help").output().ok().map(|_| Command::new("yos"))
}

#[test]
fn yos_check_finds_nothing_wrong() {
    let Some(mut check) = yos() else {
        eprintln!("skipped: no yos on this machine (set YOS=/path/to/yos)");
        return;
    };
    let mut machine = Machine::new();
    machine.serve();
    let out = machine.on_it(check.args(["check", "my-surface", "--json"])).output().expect("run yos");
    let report: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!("yos check printed no JSON ({e}): {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
    });
    let failed: Vec<&Value> = report["surfaces"][0]["checks"]
        .as_array()
        .map(|rows| rows.iter().filter(|r| r["status"] == "fail").collect())
        .unwrap_or_default();
    assert!(out.status.success() && report["ok"] == true, "yos check my-surface failed: {failed:#?}");
}
