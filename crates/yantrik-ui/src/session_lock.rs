//! The lock, held by the compositor (#313).
//!
//! The lock screen was screen 3 inside the shell's own window, an ordinary toplevel: an app
//! window in front when the desktop locked stayed in front and usable, and the compositor would
//! raise any window over the "lock" on request (VM 520: Weather and Calendar over it, working,
//! with `describe` saying `locked: true`). Now locking also starts `yantrik-lock`, which takes the
//! session lock (`ext-session-lock-v1`): the compositor shows only its surfaces and sends input
//! only to them.
//!
//! It does not know the secret: it asks this process, one line at a time, and this answers with
//! `lock::check_unlock` — the same check the shell's own screen makes, in one place: the login
//! password, or the PIN on an account without one (#414). On `ok` it unlocks the session and the
//! shell runs its own unlock (back to the desktop, the vault offered the same secret). A compositor without the protocol (exit 3) leaves the shell's own screen as
//! the lock, as before, and says so. A lock client that dies while the session is locked is
//! started again, so the person can always get back in.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::App;

/// One lock client at a time, however many ways the desktop was asked to lock.
static RUNNING: AtomicBool = AtomicBool::new(false);

/// The compositor has no session lock.
const EXIT_UNSUPPORTED: i32 = 3;
/// A lock client that keeps dying is not restarted forever.
const MOST_STARTS: u32 = 5;

/// What to answer a line from the lock client, and the secret when the answer unlocks.
pub fn answer(line: &str, check: impl Fn(&str) -> bool) -> Option<(&'static str, Option<String>)> {
    let secret = line.strip_prefix("secret ")?;
    if check(secret) {
        Some(("ok", Some(secret.to_string())))
    } else {
        Some(("no", None))
    }
}

fn lock_bin() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("yantrik-lock")))
        .unwrap_or_else(|| std::path::PathBuf::from("/opt/yantrik/bin/yantrik-lock"))
}

/// Take the session lock, if it is not held already. `on_unlock` runs on the UI thread with the
/// secret that unlocked it.
pub fn engage(
    ui: slint::Weak<App>,
    greeting: String,
    secret: crate::lock::Secret,
    on_unlock: impl Fn(&App, &str) + Send + Clone + 'static,
) {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    let bin = lock_bin();
    if !bin.exists() {
        tracing::warn!(path = %bin.display(), "No session-lock client; the shell's own screen is the lock");
        RUNNING.store(false, Ordering::SeqCst);
        return;
    }
    std::thread::spawn(move || {
        let mut starts = 0;
        loop {
            starts += 1;
            match run_once(&bin, &greeting, secret) {
                Outcome::Unlocked(pin) => {
                    let (ui, on_unlock) = (ui.clone(), on_unlock.clone());
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = ui.upgrade() {
                            on_unlock(&ui, &pin);
                        }
                    });
                    break;
                }
                Outcome::Unsupported => {
                    tracing::warn!("This compositor has no session lock; the shell's own screen is the lock (#313)");
                    break;
                }
                Outcome::Died(why) if starts < MOST_STARTS => {
                    // The session may be locked with nobody to unlock it: start another.
                    tracing::warn!(%why, starts, "The session-lock client ended without unlocking; starting it again");
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
                Outcome::Died(why) => {
                    tracing::error!(%why, "The session-lock client kept failing; the shell's own screen is the lock");
                    break;
                }
            }
        }
        RUNNING.store(false, Ordering::SeqCst);
    });
}

enum Outcome {
    Unlocked(String),
    Unsupported,
    Died(String),
}

fn run_once(bin: &std::path::Path, greeting: &str, secret: crate::lock::Secret) -> Outcome {
    let mut child = match Command::new(bin)
        .args(["--greeting", greeting, "--ask", secret.arg()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return Outcome::Died(format!("could not start {}: {e}", bin.display())),
    };
    let (Some(mut to), Some(from)) = (child.stdin.take(), child.stdout.take()) else {
        let _ = child.kill();
        return Outcome::Died("no pipes to the lock client".into());
    };
    let mut unlocked_with = None;
    for line in BufReader::new(from).lines() {
        let Ok(line) = line else { break };
        if line == "locked" {
            tracing::info!("The compositor locked the session");
            continue;
        }
        if let Some((reply, pin)) = answer(&line, |given| crate::lock::check_unlock(secret, given)) {
            if writeln!(to, "{reply}").and_then(|_| to.flush()).is_err() {
                break;
            }
            if pin.is_some() {
                unlocked_with = pin;
            }
        }
    }
    match child.wait().map(|s| s.code()) {
        Ok(Some(0)) if unlocked_with.is_some() => Outcome::Unlocked(unlocked_with.unwrap_or_default()),
        Ok(Some(EXIT_UNSUPPORTED)) => Outcome::Unsupported,
        Ok(code) => Outcome::Died(format!("exited {code:?}")),
        Err(e) => Outcome::Died(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_line_is_answered_by_the_shells_own_check_and_nothing_else_is() {
        let check = |p: &str| p == "correct horse";
        assert_eq!(answer("secret correct horse", check), Some(("ok", Some("correct horse".to_string()))));
        assert_eq!(answer("secret 0000", check), Some(("no", None)));
        assert_eq!(answer("secret ", check), Some(("no", None)), "an empty entry is a wrong one");
        assert_eq!(answer("pin correct horse", check), None, "the old protocol is not a question");
        assert_eq!(answer("locked", check), None, "not a question");
        assert_eq!(answer("ok", check), None, "the client cannot answer itself");
    }

    #[test]
    fn the_lock_client_sits_beside_the_shell() {
        assert!(lock_bin().ends_with("yantrik-lock"));
    }
}
