//! Lock screen — screen PIN and idle lock management.
//!
//! The PIN here guards the shell's own canvas and nothing else. It is a comparison against
//! `~/.yantrik/lock_pin`, which is a file on the same disk as everything it is guarding, so it
//! keeps a passer-by out of an unattended desktop and stops at exactly that. It is not what
//! protects the credential vault: that is a passphrase the vault's key is *wrapped* under, so
//! there is nothing on disk to read and nothing to compare against. See `crate::vault_unlock`,
//! which the same screen drives.
//!
//! Creates a default PIN "0000" on first use. Idle lock triggers after a configurable timeout
//! (default 5 minutes).

use std::path::PathBuf;

/// Default idle lock timeout in seconds (5 minutes).
pub const DEFAULT_IDLE_LOCK_SECS: u64 = 300;

/// Path to the PIN file.
pub fn pin_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    PathBuf::from(home).join(".yantrik/lock_pin")
}

/// Ensure the PIN file exists. Creates with default "0000" if missing.
pub fn ensure_pin_file() {
    let path = pin_path();
    if path.exists() {
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::write(&path, "0000").is_err() {
        return;
    }
    // Under the default umask this file came out 0644 — readable by every other account on the
    // machine. It is a weak secret already; there was no reason to publish it as well.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    tracing::info!("Created default lock PIN at {}", path.display());
}

/// Check if the given input matches the stored PIN.
/// Returns true if authentication succeeds.
///
/// This used to end with `Err(_) => true` — "fail-open for dev". On a shipped machine that made
/// `rm ~/.yantrik/lock_pin` the whole bypass: delete one file the locked-out person can reach from
/// any other tty and every PIN is accepted. A lock with a documented way past it is a screensaver.
///
/// A missing file is not treated as an error, because it has an obvious right answer: put the
/// default back and check against that. Anything else — unreadable, a directory, an I/O failure —
/// refuses, because at that point the machine does not know what the PIN is and "I do not know"
/// must not resolve to "come in".
pub fn check_pin(input: &str) -> bool {
    let path = pin_path();
    match std::fs::read_to_string(&path) {
        Ok(stored) => stored.trim() == input.trim(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::warn!("Lock PIN file is missing — restoring the default and checking against it");
            ensure_pin_file();
            std::fs::read_to_string(&path)
                .map(|stored| stored.trim() == input.trim())
                .unwrap_or(false)
        }
        Err(e) => {
            tracing::warn!(error = %e, "Cannot read the lock PIN file — refusing to unlock");
            false
        }
    }
}

#[cfg(test)]
mod lock_tests {
    use super::*;

    /// `HOME` is process-wide, so these take turns.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct TempHome {
        dir: PathBuf,
        previous: Option<String>,
    }

    impl TempHome {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("yantrik-lock-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let previous = std::env::var("HOME").ok();
            std::env::set_var("HOME", &dir);
            TempHome { dir, previous }
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            match &self.previous {
                Some(home) => std::env::set_var("HOME", home),
                None => std::env::remove_var("HOME"),
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn a_missing_pin_file_does_not_unlock_the_screen() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let _home = TempHome::new("missing");

        // Nothing created yet: this is the state `rm ~/.yantrik/lock_pin` produces.
        assert!(!pin_path().exists());
        assert!(
            !check_pin("whatever"),
            "deleting the PIN file must not be a way past the lock screen"
        );
        // And it put the default back, so the person is not locked out either.
        assert!(pin_path().exists());
        assert!(check_pin("0000"));
    }

    #[test]
    fn the_pin_file_is_not_readable_by_anyone_else() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let _home = TempHome::new("mode");
        ensure_pin_file();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(pin_path()).unwrap().permissions().mode() & 0o777;
            assert_eq!(
                mode, 0o600,
                "the lock PIN was written {mode:o} — every account on the machine could read it"
            );
        }
    }

    #[test]
    fn the_right_pin_unlocks_and_the_wrong_one_does_not() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let _home = TempHome::new("compare");
        ensure_pin_file();
        std::fs::write(pin_path(), "4821\n").unwrap();

        assert!(check_pin("4821"));
        assert!(check_pin(" 4821 "), "a trailing space is a typo, not a different PIN");
        assert!(!check_pin("4822"));
        assert!(!check_pin(""));
    }
}
