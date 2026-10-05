//! yantrik-egress — the one way out for the mind account.
//!
//! A forward proxy on loopback that serves one account, `yantrik-mind`, and lets it reach only
//! what the person's policy allows. Tunnels only (`CONNECT`), plus plain HTTP where a rule says so;
//! no TLS is ever opened here, so it sees where the mind connects and never what it says. See
//! design/mind-egress-2026-09-29.md.
//!
//! Configured by its unit's environment:
//!
//! | variable | default | |
//! |---|---|---|
//! | `EGRESS_LISTEN` | `127.0.0.1:7450` | where the mind's `HTTPS_PROXY` points |
//! | `EGRESS_STATE` | `/var/lib/yantrik-egress` | the policy, the ledger, Private mode |
//! | `EGRESS_CONTROL` | `/run/yantrik-egress/control` | the desktop's socket |
//! | `EGRESS_SERVE_UID` | the uid of `yantrik-mind` | the one account served |
//! | `EGRESS_OWNER_UID` | — | the desktop's owner, who may use the control socket beside root |
//!
//! `yantrik-egress direct [STATE_DIR]` prints the direct set instead and exits: where the kernel
//! lets the mind account connect without this proxy (`direct`, `yantrik-update mind-egress`).
//! `yantrik-egress seed-plan` reads `<source> <url>` lines and prints which become seeded `lan`
//! rules (`seed`, `yantrik-update`'s seeding). `yantrik-egress capabilities` prints what this
//! build promises, one word a line (`policy::CAPABILITIES`). `yantrik-egress lan-hosts
//! [STATE_DIR]` prints every host a `lan` rule names, with its ports (`direct`).

mod control;
mod direct;
mod ledger;
mod local;
mod peer;
mod policy;
mod proxy;
mod ranges;
mod request;
mod seed;
mod state;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How often the ledger is written when it changed.
const FLUSH: Duration = Duration::from_secs(30);

fn env(name: &str, default: &str) -> String {
    std::env::var(name).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default.to_string())
}

#[cfg(unix)]
fn uid_of_user(name: &str) -> Option<u32> {
    let c = std::ffi::CString::new(name).ok()?;
    // SAFETY: getpwnam with a NUL-terminated name; the result is read at once, before any other
    // call that could reuse its buffer.
    let pw = unsafe { libc::getpwnam(c.as_ptr()) };
    (!pw.is_null()).then(|| unsafe { (*pw).pw_uid })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => {}
        Some("direct") if args.len() <= 2 => {
            let dir = args.get(1).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(env("EGRESS_STATE", "/var/lib/yantrik-egress")));
            match direct::export(&dir) {
                Ok(text) => print!("{text}"),
                Err(e) => {
                    eprintln!("yantrik-egress direct: {e}");
                    std::process::exit(1);
                }
            }
            return;
        }
        Some("lan-hosts") if args.len() <= 2 => {
            let dir = args.get(1).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(env("EGRESS_STATE", "/var/lib/yantrik-egress")));
            match direct::export_lan_hosts(&dir) {
                Ok(text) => print!("{text}"),
                Err(e) => {
                    eprintln!("yantrik-egress lan-hosts: {e}");
                    std::process::exit(1);
                }
            }
            return;
        }
        Some("seed-plan") if args.len() == 1 => {
            use std::io::Read;
            let mut input = String::new();
            if std::io::stdin().take(64 * 1024).read_to_string(&mut input).is_err() {
                eprintln!("yantrik-egress seed-plan: stdin is not text");
                std::process::exit(1);
            }
            print!("{}", seed::plan_text(&input));
            return;
        }
        Some("capabilities") if args.len() == 1 => {
            for c in policy::CAPABILITIES {
                println!("{c}");
            }
            return;
        }
        Some(_) => {
            eprintln!("usage: yantrik-egress            the proxy, configured by its unit's environment\n       yantrik-egress direct [DIR]   print the direct set from the policy in DIR\n       yantrik-egress lan-hosts [DIR] print every host a lan rule names, with its ports\n       yantrik-egress seed-plan      <source> <url> lines on stdin: which become seeded lan rules\n       yantrik-egress capabilities   print what this build promises, one word a line");
            std::process::exit(2);
        }
    }
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    let runtime = match tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "yantrik-egress could not start its runtime");
            std::process::exit(1);
        }
    };
    if let Err(e) = runtime.block_on(run()) {
        tracing::error!(error = %e, "yantrik-egress stopped");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let listen: SocketAddr = env("EGRESS_LISTEN", "127.0.0.1:7450").parse().map_err(|e| format!("EGRESS_LISTEN: {e}"))?;
    if !listen.ip().is_loopback() {
        return Err("EGRESS_LISTEN must be a loopback address: this proxy is for this machine's mind only".into());
    }
    let dir = PathBuf::from(env("EGRESS_STATE", "/var/lib/yantrik-egress"));
    let control_path = PathBuf::from(env("EGRESS_CONTROL", "/run/yantrik-egress/control"));
    let serve_uid = match std::env::var("EGRESS_SERVE_UID").ok().and_then(|v| v.parse().ok()) {
        Some(u) => u,
        None => uid_of_user("yantrik-mind").ok_or("no yantrik-mind account, and EGRESS_SERVE_UID is not set")?,
    };
    if serve_uid == 0 {
        return Err("the served account cannot be root".into());
    }
    let owner: Option<u32> = std::env::var("EGRESS_OWNER_UID").ok().and_then(|v| v.parse().ok()).filter(|u| *u != serve_uid);

    let state = Arc::new(Mutex::new(state::State::load(&dir)));
    {
        let s = state.lock().map_err(|_| "state poisoned")?;
        tracing::info!(mode = ?s.policy.mode, rules = s.policy.rules.len(), private = s.private, serve_uid, %listen, "yantrik-egress starting");
    }

    let _ = std::fs::remove_file(&control_path);
    let control = tokio::net::UnixListener::bind(&control_path).map_err(|e| format!("control socket {}: {e}", control_path.display()))?;
    // Anyone may connect; the kernel's word on who they are decides whether they are answered.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&control_path, std::fs::Permissions::from_mode(0o666)).map_err(|e| e.to_string())?;
    }
    tokio::spawn(control::serve(control, state.clone(), owner));

    {
        let state = state.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(FLUSH).await;
                if let Ok(mut s) = state.lock() {
                    if let Err(e) = s.save_ledger() {
                        tracing::warn!(error = %e, "the ledger could not be written");
                    }
                }
            }
        });
    }

    let listener = tokio::net::TcpListener::bind(listen).await.map_err(|e| format!("{listen}: {e}"))?;
    let local = listener.local_addr().map_err(|e| e.to_string())?;
    proxy::serve(listener, Arc::new(proxy::Proxy { state, serve_uid, local })).await;
    Ok(())
}
