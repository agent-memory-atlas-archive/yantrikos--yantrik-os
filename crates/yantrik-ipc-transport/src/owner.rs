//! Owned names: a socket belongs to the process that is answering on it, and the one name a grant
//! is spent through belongs to the shell.
//!
//! # Why this exists
//!
//! Two holes, one idea (design `surface-sdk`, section 6; `docs/surface-protocol.md`, "Owned
//! names").
//!
//! **Binding took whatever was at the path.** `serve_unix` unlinked any file at its address and
//! bound a new socket there. A second copy of an app, a service started twice, or anything else
//! that picked the same name silently took it from the process that was using it: the first one
//! kept running, listening on an inode nobody could reach any more, and every caller was answered
//! by the newcomer. [`claim`] asks first. A socket that answers `rpc.ping` belongs to somebody,
//! and the bind is refused with a sentence that says so; only a socket nobody is listening on — the
//! file a crashed process left behind — is removed.
//!
//! **Nobody checked who answered as the shell.** Every grant a person gives is spent by an app
//! calling `app-shell`'s `consume_approval` (see `gate::spend_grant`), and the app believed
//! whatever answered on that path. Anything that could bind `app-shell.sock` before the shell did,
//! or after it died, could answer "spent" to every grant — and so stand in for the person's Allow.
//! [`must_be_the_shell`] reads the peer's pid from the kernel (`SO_PEERCRED`, filled in at
//! `connect` time from the listening process) and `/proc/<pid>/exe`, and refuses unless it is a
//! `yantrik-ui` binary: `/opt/yantrik/bin/yantrik-ui`, or a developer's own build of it.
//!
//! # What it does not do
//!
//! Same-uid limits stand (#154). A process running as the person can build or copy a binary called
//! `yantrik-ui` and run it; this stops accidents and casual impersonation — a stray test server, a
//! second shell, a script that picked the wrong name — not hostile code that already runs as the
//! person. That boundary is the uid, and nothing on this socket can move it.

use std::path::Path;
use std::time::Duration;

use crate::server::PeerCred;

/// The shell's program name. The rule is the file name, not the directory, so the installed
/// shell and a developer's `target/release/yantrik-ui` both pass and nothing else does.
pub const SHELL_BINARY: &str = "yantrik-ui";

/// What Linux appends to `/proc/<pid>/exe` when the file a process was started from has since been
/// replaced or removed. An update replaces `/opt/yantrik/bin/yantrik-ui` under a running shell,
/// and that shell is still the shell until it restarts.
const DELETED: &str = " (deleted)";

/// Whether `exe` — `/proc/<pid>/exe` resolved — is a `yantrik-ui` binary.
///
/// Pure, so the rule is a test rather than a promise in a comment.
pub fn is_shell_binary(exe: &str) -> bool {
    let exe = exe.strip_suffix(DELETED).unwrap_or(exe);
    exe.starts_with('/') && crate::peer_identity::basename(exe) == SHELL_BINARY
}

/// The program behind a pid, as `/proc` says it, or `None` when it cannot be read.
#[cfg(target_os = "linux")]
pub fn exe_of(pid: i32) -> Option<String> {
    if pid <= 0 {
        return None;
    }
    std::fs::read_link(format!("/proc/{pid}/exe")).ok().map(|p| p.to_string_lossy().to_string())
}

#[cfg(not(target_os = "linux"))]
pub fn exe_of(pid: i32) -> Option<String> {
    let _ = pid;
    None
}

/// Who is on the other end of a connected unix stream, as the kernel recorded it.
///
/// For the connecting side this is the process that was listening — the credentials are taken at
/// `listen`/`connect` time, not from anything the peer writes, which is the whole point.
#[cfg(target_os = "linux")]
pub fn peer_of(stream: &std::os::unix::net::UnixStream) -> Option<PeerCred> {
    use std::os::fd::AsRawFd;
    let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `cred` and `len` are valid for writes of the sizes given, and the descriptor is
    // owned by `stream`, which outlives this call.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut libc::ucred as *mut libc::c_void,
            &mut len,
        )
    };
    (rc == 0).then_some(PeerCred { pid: cred.pid, uid: cred.uid, gid: cred.gid })
}

#[cfg(all(unix, not(target_os = "linux")))]
pub fn peer_of(stream: &std::os::unix::net::UnixStream) -> Option<PeerCred> {
    let _ = stream;
    None
}

/// The rule for `app-shell`: the process answering on it must be a `yantrik-ui` binary.
///
/// `Err` is a sentence that ends in a full stop, because it is dropped into the middle of the
/// gate's own refusal ("GRANT: `id` does not authorise … — {this} Nothing was run; …").
pub fn must_be_the_shell(peer: Option<PeerCred>) -> Result<(), String> {
    let Some(peer) = peer else {
        return Err("the kernel would not say which process is answering as the shell, so it \
                    could not be checked and the grant was not offered to it."
            .to_string());
    };
    let exe = exe_of(peer.pid);
    match exe.as_deref() {
        Some(exe) if is_shell_binary(exe) => Ok(()),
        Some(exe) => Err(format!(
            "the process answering as the shell is {exe} (pid {}), not the desktop's own \
             {SHELL_BINARY}, so the grant was not offered to it.",
            peer.pid
        )),
        None => Err(format!(
            "the process answering as the shell (pid {}) could not be identified from /proc, so \
             the grant was not offered to it.",
            peer.pid
        )),
    }
}

// ── Claiming a name ─────────────────────────────────────────────────

/// How long a bind waits for whatever is on its path to answer `rpc.ping`. A live server answers
/// in well under a millisecond; this bounds a wedged one.
pub const CLAIM_PING: Duration = Duration::from_secs(1);

/// Who holds a socket path right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Holder {
    /// Nothing is there, or nothing is listening: a file a crashed process left behind.
    Nobody,
    /// Something answered `rpc.ping`. Carries what it said its service id is, when it said.
    Answers(Option<String>),
    /// Something accepted the connection and did not answer in time. Still somebody's.
    Silent,
}

/// Ask whatever is at `path` whether it is alive.
#[cfg(unix)]
pub fn who_holds(path: &Path, patience: Duration) -> Holder {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    let Ok(mut stream) = UnixStream::connect(path) else {
        // ECONNREFUSED on a socket file nobody listens on, ENOENT on no file, EACCES and the rest
        // on something that is not a socket we could use either way.
        return Holder::Nobody;
    };
    stream.set_read_timeout(Some(patience)).ok();
    stream.set_write_timeout(Some(patience)).ok();
    let ask = |stream: &mut UnixStream, method: &str| -> Option<serde_json::Value> {
        let line = format!("{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"{method}\"}}\n");
        stream.write_all(line.as_bytes()).ok()?;
        let mut reply = String::new();
        BufReader::new(stream.try_clone().ok()?).read_line(&mut reply).ok()?;
        serde_json::from_str::<serde_json::Value>(&reply).ok()
    };
    match ask(&mut stream, "rpc.ping") {
        Some(reply) if reply.get("result").is_some() || reply.get("error").is_some() => {
            let id = ask(&mut stream, "rpc.service_id")
                .and_then(|r| r.get("result").and_then(|v| v.as_str()).map(str::to_string));
            Holder::Answers(id)
        }
        _ => Holder::Silent,
    }
}

/// Make `path` free to bind, or say whose it is.
///
/// A socket that answers — or accepts and keeps quiet — belongs to a running process and is left
/// alone; the error names it. A socket nobody is listening on is removed. Anything else at the
/// path (a symlink, a regular file) is not a listener and is removed as the bind always did.
#[cfg(unix)]
pub fn claim(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::FileTypeExt;

    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return Ok(());
    };
    if !meta.file_type().is_socket() {
        return std::fs::remove_file(path);
    }
    match who_holds(path, CLAIM_PING) {
        Holder::Nobody => std::fs::remove_file(path),
        Holder::Answers(id) => Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            format!(
                "another instance owns {}: it answered rpc.ping{}. Refusing to start rather than \
                 take the name from a running process — stop that one first, or talk to it.",
                path.display(),
                id.map(|id| format!(" as `{id}`")).unwrap_or_default(),
            ),
        )),
        Holder::Silent => Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            format!(
                "something is listening on {} but did not answer rpc.ping within {}s. Refusing to \
                 start rather than take the name from a process that may only be busy — stop it \
                 first.",
                path.display(),
                CLAIM_PING.as_secs(),
            ),
        )),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::{UnixListener, UnixStream};

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("yantrik-owner-{tag}-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A server that answers every line it is sent, the way the transport answers `rpc.ping`.
    fn answering(path: &Path) {
        let listener = UnixListener::bind(path).unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                std::thread::spawn(move || {
                    let mut writer = stream.try_clone().unwrap();
                    for line in BufReader::new(stream).lines() {
                        let Ok(line) = line else { return };
                        let asked: serde_json::Value = serde_json::from_str(&line).unwrap();
                        let result = if asked["method"] == "rpc.service_id" { "app-first" } else { "pong" };
                        let reply = serde_json::json!({"jsonrpc": "2.0", "id": asked["id"], "result": result});
                        if writer.write_all(format!("{reply}\n").as_bytes()).is_err() {
                            return;
                        }
                    }
                });
            }
        });
    }

    #[test]
    fn the_shell_is_a_yantrik_ui_binary_wherever_it_was_built() {
        for exe in [
            "/opt/yantrik/bin/yantrik-ui",
            "/home/yantrik/targets/sdk-spec/release/yantrik-ui",
            "/opt/yantrik/bin/yantrik-ui (deleted)",
        ] {
            assert!(is_shell_binary(exe), "{exe}");
        }
        for exe in [
            "/usr/bin/python3.12",
            "/tmp/yantrik-ui-evil",
            "/opt/yantrik/bin/yantrik-uix",
            "/opt/yantrik/bin/yantrik-notes",
            "yantrik-ui",
            "",
            "/opt/yantrik/bin/yantrik-ui (deleted) (deleted)",
        ] {
            assert!(!is_shell_binary(exe), "{exe}");
        }
    }

    /// The kernel's account of the listener, read from the connecting side, is this very process
    /// — and a test binary is not the shell, so the rule refuses it and says what it found.
    #[test]
    fn a_peer_that_is_not_the_shell_is_named_and_refused() {
        let dir = scratch("peer");
        let path = dir.join("app-shell.sock");
        answering(&path);
        let stream = UnixStream::connect(&path).unwrap();
        let peer = peer_of(&stream).expect("SO_PEERCRED on a connected unix stream");
        assert_eq!(peer.pid as u32, std::process::id());
        let err = must_be_the_shell(Some(peer)).unwrap_err();
        assert!(err.contains(&format!("pid {}", peer.pid)) && err.contains("not the desktop's own yantrik-ui"), "{err}");
        assert!(err.ends_with('.'), "it is spliced into the gate's sentence: {err}");
        assert!(must_be_the_shell(None).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_live_name_is_not_taken_and_a_dead_one_is() {
        let dir = scratch("claim");

        let live = dir.join("app-first.sock");
        answering(&live);
        assert_eq!(who_holds(&live, CLAIM_PING), Holder::Answers(Some("app-first".into())));
        let err = claim(&live).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse);
        assert!(err.to_string().contains("another instance owns") && err.to_string().contains("app-first"), "{err}");
        assert!(UnixStream::connect(&live).is_ok(), "the running one still has its name");

        // A socket file whose process is gone: bound, then the listener dropped.
        let dead = dir.join("app-gone.sock");
        drop(UnixListener::bind(&dead).unwrap());
        assert!(dead.exists());
        assert_eq!(who_holds(&dead, CLAIM_PING), Holder::Nobody);
        claim(&dead).expect("a dead socket is removed");
        assert!(!dead.exists());

        // Nothing there at all is free, and so is a stray regular file.
        claim(&dir.join("app-never.sock")).expect("nothing to claim");
        let stray = dir.join("app-stray.sock");
        std::fs::write(&stray, b"not a socket").unwrap();
        claim(&stray).expect("a file that is not a socket is not a listener");
        assert!(!stray.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_listener_that_never_answers_still_owns_its_name() {
        let dir = scratch("silent");
        let path = dir.join("app-busy.sock");
        // Listening, never reading: connect succeeds, the ping gets no answer.
        let listener = UnixListener::bind(&path).unwrap();
        assert_eq!(who_holds(&path, Duration::from_millis(200)), Holder::Silent);
        drop(listener);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
