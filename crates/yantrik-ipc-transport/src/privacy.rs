//! Private mode: while the person has it on, no agent sees or does anything on this desktop.
//!
//! The shell publishes it in `privacy.json`, beside the settings file, and it stays on until the
//! person turns it off: through restarts and reboots, because a privacy switch that quietly turns
//! itself off is a trap. Every place an agent comes in reads it on every call, as the ceiling and
//! the mode are read, so turning it on takes effect on connections already open:
//!
//! - the mind door (`server::serve_door`, and the Python SDK's door): every request from the mind
//!   account is answered with [`REFUSAL`], describe and memory checks included;
//! - each surface's dispatch (`yantrik_app_runtime::control`, the Python SDK's surface): a call
//!   that carries an agent token is refused the same way.
//!
//! The file fails closed: absent is off (a desktop nobody ever made private), but a file that is
//! there and cannot be read or understood is on. Saying the person was not private when they
//! were is the mistake that matters.
//!
//! Only the shell writes it, and only for the person: the file lives in the person's own
//! configuration directory, which the mind account cannot write.

use std::path::PathBuf;

use serde_json::json;

use crate::gate::settings_path;

/// The file the shell publishes Private mode in, beside the settings file.
pub const PRIVACY_FILE: &str = "privacy.json";

/// What an agent is told while the person is private. One sentence, the same everywhere, so an
/// agent can recognise it and stop rather than retry.
pub const REFUSAL: &str = "PRIVATE: the person has turned on Private mode. Nothing on this desktop is shown to \
                           agents or done for them until they turn it off. Nothing was run.";

/// Where the shell publishes Private mode.
pub fn privacy_path() -> PathBuf {
    settings_path().with_file_name(PRIVACY_FILE)
}

/// Whether the person is in Private mode now. Read per call.
pub fn is_private() -> bool {
    match std::fs::read_to_string(privacy_path()) {
        Ok(text) => private_in(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
    }
}

/// The file's meaning: `{"private": true|false, ...}`. Anything else reads as private.
pub fn private_in(text: &str) -> bool {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(v) => v.get("private").and_then(serde_json::Value::as_bool).unwrap_or(true),
        Err(_) => true,
    }
}

/// Publish Private mode (the shell only). Written whole to a temporary file and renamed, so a
/// reader never sees half of it (which would read as private, but should not have to).
pub fn publish(private: bool, since_unix: u64) -> std::io::Result<()> {
    let path = privacy_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    let body = json!({ "private": private, "since": since_unix }).to_string();
    std::fs::write(&tmp, body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, &path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_clear_false_is_not_private() {
        assert!(!private_in(r#"{"private": false, "since": 1}"#));
        assert!(private_in(r#"{"private": true}"#));
        for unclear in ["", "{", "[]", "{}", r#"{"private": "no"}"#, r#"{"private": 0}"#] {
            assert!(private_in(unclear), "{unclear:?} must read as private");
        }
    }
}
