//! The reach of every live agent started as a catalog role — kept here, and published for every
//! door that does not live in this process.
//!
//! `hand_off` holds an agent to its role's reach *before* its first turn is sent ([`hold`]), so
//! there is no moment in which it can act unheld. From then on:
//!
//! - the shell's own dispatch reads this registry in-process (`control_agents::actions` installs
//!   [`lookup`] with `reach::read_reach_with`, as the shell spends grants in-process);
//! - every other app and service reads the file this writes, `agent-reach.json` beside the mode
//!   file, which keeps a SHA-256 of each agent's token and never the token.
//!
//! An agent that is stopped is let go ([`release`]); its token names nothing any more anyway. The
//! file is written whole each time, to a temporary name and renamed over, so a door reads the old
//! file or the new one and never half of either.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use yantrik_harness::Host;
use yantrik_ipc_transport::reach::{self, Entry, Reach};

use super::catalog::Role;
use super::model::AgentId;

/// The most agents with a reach kept at once. The host runs six agents at most; the rest are ones
/// whose harness went without a Stop, and the oldest of those go first.
const MOST: usize = 64;

static LIVE: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

fn live() -> MutexGuard<'static, Vec<Entry>> {
    LIVE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Where the file goes. Under test, a file of the test run's own: a test must never write the
/// person's.
pub fn path() -> PathBuf {
    #[cfg(not(test))]
    return reach::reach_path();
    #[cfg(test)]
    return std::env::temp_dir().join(format!("yantrik-agent-reach-under-test-{}.json", std::process::id()));
}

/// Hold `agent` to `role`'s reach from now on, on every door. An `Err` means the reach could not
/// be published, and the agent must not be started: an agent a door cannot hold is not started.
pub fn hold(host: &Host, agent: &AgentId, role: &Role) -> Result<(), String> {
    let digest = host
        .with_agent_token(agent, reach::token_digest)
        .ok_or_else(|| format!("`{agent}` is not live, so there is no token to hold to the {}'s reach", role.name))?;
    let mut entries = live();
    entries.retain(|e| e.reach.agent != agent.0 && e.token_sha256 != digest);
    entries.push(Entry { token_sha256: digest, reach: role.reach_for(&agent.0) });
    if entries.len() > MOST {
        let extra = entries.len() - MOST;
        entries.drain(..extra);
    }
    write(&entries).map_err(|why| format!("the {}'s reach could not be published: {why}", role.name))
}

/// Let `agent` go: stopped, it acts no more.
pub fn release(agent: &AgentId) {
    let mut entries = live();
    let before = entries.len();
    entries.retain(|e| e.reach.agent != agent.0);
    if entries.len() != before {
        if let Err(why) = write(&entries) {
            tracing::warn!(agent = %agent, error = %why, "could not write the agents' reach after a stop");
        }
    }
}

/// The reach a token carries, for the shell's own dispatch.
pub fn lookup(token: &str) -> Option<Reach> {
    let digest = reach::token_digest(token);
    live().iter().find(|e| e.token_sha256 == digest).map(|e| e.reach.clone())
}

/// The reach `agent` is held to, if it was started as a role.
pub fn of(agent: &AgentId) -> Option<Reach> {
    live().iter().find(|e| e.reach.agent == agent.0).map(|e| e.reach.clone())
}

/// At the shell's start: nothing is held yet, and whatever an earlier run left in the file named
/// tokens that no longer exist.
pub fn reset() {
    let mut entries = live();
    entries.clear();
    if let Err(why) = write(&entries) {
        tracing::warn!(error = %why, "could not clear the agents' reach file");
    }
}

fn write(entries: &[Entry]) -> Result<(), String> {
    let path = path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let tmp = path.with_extension(format!("json.{}", std::process::id()));
    std::fs::write(&tmp, reach::file_text(entries)).map_err(|e| format!("{}: {e}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))
}

/// The file as a door in another process reads it, for a test to check what it would be told.
#[cfg(test)]
pub fn read_as_a_door(token: &str) -> Result<Option<Reach>, String> {
    let text = std::fs::read_to_string(path()).map_err(|e| e.to_string())?;
    reach::reach_from(&text, token)
}
