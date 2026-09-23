//! Who an agent is — never a caller's word.
//!
//! An `agent` argument would be an impersonation switch: any process that could reach the socket
//! could run commands in another agent's pane, read its jobs and kill them. So the agent comes from
//! a **token** and from the **kernel**, together (design, decision 3, "Routing, and who an agent
//! is"):
//!
//! 1. When the host hands an agent its first turn, the assignment carries an agent token — 128
//!    random bits, known to the shell and that harness. The harness passes it to the `yos-mcp` it
//!    starts for that conversation (`YANTRIK_AGENT_TOKEN`), and every act from that bridge carries
//!    it — beside `args` on `app.act`, never among them, because the arguments are what an
//!    approval card shows and an audit line keeps.
//! 2. The shell resolves the token to the agent **and** checks that the process on the socket
//!    (its pid from `SO_PEERCRED`, `yantrik_app_runtime::control::caller()`) descends from the
//!    harness process that attached — the host records that pid from the peer credentials at
//!    attach. A token presented from the wrong process tree is refused.
//!
//! # The contract the shell implements
//!
//! [`AgentResolver`] is that check. The shell's implementation is [`Lookup`] over the harness
//! host (piece 1 of the design):
//!
//! ```rust,ignore
//! Lookup(move |token: &str| host.agent_for_token(token))
//! // Host::agent_for_token(&self, token: &str) -> Option<(AgentId, Option<u32> /* harness pid */)>
//! ```
//!
//! [`Lookup`] refuses an unknown token, a harness with no recorded pid, a caller with no pid, and a
//! caller that does not descend from the harness. [`TokenTable`] is the same rule over an in-memory
//! table, for tests; [`NoAgents`] knows no tokens at all, which is the shell's resolver until the
//! host issues them.
//!
//! # The limit, stated
//!
//! Processes of the same user can read each other's environment, so a token is not a secret from
//! a hostile program running as the person. This stops confusion — a bridge acting for the wrong
//! agent — and casual impersonation. What stops a hostile program is the grade on every action.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::AgentId;

/// What a call is told when its token names no agent.
pub const NO_AGENT: &str = "no agent holds this token";

/// How far up the process tree the caller check walks. The real chain — `yos` ← `yos-mcp` ←
/// the mind's own process ← the harness — is four or five deep; this is a bound on a loop, not a
/// guess about depth.
const ANCESTRY_BOUND: usize = 64;

/// Turn a token and the kernel's account of the caller into an agent, or a sentence saying why not.
pub trait AgentResolver: Send + Sync {
    /// `caller_pid` is the socket peer's pid as the kernel reported it, never anything the caller
    /// wrote.
    fn resolve(&self, token: &str, caller_pid: Option<u32>) -> Result<AgentId, String>;
}

/// Knows no tokens. Every call is answered [`NO_AGENT`] — inert, but a real answer rather than a
/// missing action.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoAgents;

impl AgentResolver for NoAgents {
    fn resolve(&self, token: &str, caller_pid: Option<u32>) -> Result<AgentId, String> {
        verify(token, None, caller_pid)
    }
}

/// The resolver over any lookup `token → (agent, harness pid)`: the shell's, over
/// `Host::agent_for_token`.
pub struct Lookup<F>(pub F);

impl<F> AgentResolver for Lookup<F>
where
    F: Fn(&str) -> Option<(AgentId, Option<u32>)> + Send + Sync,
{
    fn resolve(&self, token: &str, caller_pid: Option<u32>) -> Result<AgentId, String> {
        let found = if token.trim().is_empty() { None } else { (self.0)(token.trim()) };
        verify(token, found, caller_pid)
    }
}

/// Tokens held in memory. For tests, and for anything that issues tokens itself.
#[derive(Default)]
pub struct TokenTable {
    entries: Mutex<HashMap<String, (AgentId, Option<u32>)>>,
}

impl TokenTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `token` belongs to `agent`, whose harness is the process `harness_pid`.
    pub fn issue(&self, token: &str, agent: AgentId, harness_pid: Option<u32>) {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(token.to_string(), (agent, harness_pid));
    }

    /// Forget a token: its agent's calls are refused from now on.
    pub fn revoke(&self, token: &str) {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).remove(token);
    }
}

impl AgentResolver for TokenTable {
    fn resolve(&self, token: &str, caller_pid: Option<u32>) -> Result<AgentId, String> {
        let found = self.entries.lock().unwrap_or_else(|e| e.into_inner()).get(token.trim()).cloned();
        verify(token, found, caller_pid)
    }
}

/// The one rule every resolver applies to what its lookup found.
fn verify(
    token: &str,
    found: Option<(AgentId, Option<u32>)>,
    caller_pid: Option<u32>,
) -> Result<AgentId, String> {
    if token.trim().is_empty() {
        return Err("no agent token came with this call, so it belongs to no agent. An agent's \
                    calls carry the token its harness was given with the agent's first turn, \
                    beside `args` on app.act (`agent_token`; `yos act --agent-token`, or \
                    YANTRIK_AGENT_TOKEN in yos's environment) — never among the arguments."
            .to_string());
    }
    let Some((agent, harness)) = found else {
        return Err(format!(
            "{NO_AGENT}. Tokens are issued by this desktop when an agent starts, and a token from \
             an earlier session or another machine names nothing here."
        ));
    };
    let Some(harness) = harness else {
        return Err(format!(
            "the harness holding this token for {agent} attached without a process this machine \
             could see, so the token cannot be checked against the caller; refused."
        ));
    };
    let Some(caller) = caller_pid.filter(|pid| *pid > 0) else {
        return Err("this call arrived without a process the kernel could name, so its token \
                    cannot be checked against it; refused."
            .to_string());
    };
    if !descends_from(caller, harness) {
        return Err(format!(
            "this token was not issued to the process that sent it: pid {caller} does not descend \
             from the harness the token belongs to. A token works only from the process tree its \
             harness started; refused."
        ));
    }
    Ok(agent)
}

/// Whether `pid` is `ancestor` or runs under it, walking up `/proc` with
/// [`yantrik_ipc_transport::peer_identity::parse_stat`].
///
/// Cycle-safe and bounded. Any `/proc` read that fails ends the walk with `false`: a process that
/// cannot be traced to the harness is not the harness's.
pub fn descends_from(pid: u32, ancestor: u32) -> bool {
    use yantrik_ipc_transport::peer_identity::parse_stat;

    let target = ancestor as i32;
    let mut at = pid as i32;
    let mut seen = std::collections::HashSet::new();
    for _ in 0..ANCESTRY_BOUND {
        if at == target {
            return true;
        }
        if at <= 1 || !seen.insert(at) {
            return false;
        }
        let Some(stat) = std::fs::read_to_string(format!("/proc/{at}/stat"))
            .ok()
            .and_then(|text| parse_stat(&text))
        else {
            return false;
        };
        at = stat.ppid;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent() -> AgentId {
        AgentId::new("pi", "c-7f3a91")
    }

    #[test]
    fn a_desktop_that_has_issued_no_tokens_answers_every_call_the_same_way() {
        let err = NoAgents.resolve("0123456789abcdef", Some(std::process::id())).unwrap_err();
        assert!(err.starts_with(NO_AGENT), "{err}");
        let err = NoAgents.resolve("  ", Some(std::process::id())).unwrap_err();
        assert!(err.contains("no agent token came with this call"), "{err}");
        assert!(err.contains("beside `args`"), "and it says where the token goes: {err}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_token_resolves_only_from_under_the_harness_it_was_issued_to() {
        // This test process stands in for the harness.
        let harness = std::process::id();
        let table = TokenTable::new();
        table.issue("tok-pi", agent(), Some(harness));

        assert_eq!(table.resolve("tok-pi", Some(harness)), Ok(agent()), "the harness itself");

        // A child of the harness — what yos-mcp is — resolves too.
        let mut child = std::process::Command::new("sleep").arg("5").spawn().unwrap();
        assert_eq!(table.resolve("tok-pi", Some(child.id())), Ok(agent()));
        let _ = child.kill();
        let _ = child.wait();

        // pid 1 is nobody's child: the same token from there is refused, with a sentence.
        let err = table.resolve("tok-pi", Some(1)).unwrap_err();
        assert!(err.contains("not issued to the process that sent it"), "{err}");

        // No pid from the kernel, no check, no agent.
        let err = table.resolve("tok-pi", None).unwrap_err();
        assert!(err.contains("without a process the kernel could name"), "{err}");

        // A token nobody issued, and one taken back.
        assert!(table.resolve("tok-other", Some(harness)).unwrap_err().starts_with(NO_AGENT));
        table.revoke("tok-pi");
        assert!(table.resolve("tok-pi", Some(harness)).unwrap_err().starts_with(NO_AGENT));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_harness_with_no_recorded_process_cannot_vouch_for_anyone() {
        let lookup = Lookup(|token: &str| (token == "t").then(|| (agent(), None::<u32>)));
        let err = lookup.resolve("t", Some(std::process::id())).unwrap_err();
        assert!(err.contains("attached without a process"), "{err}");
        assert!(lookup.resolve("u", Some(1)).unwrap_err().starts_with(NO_AGENT));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn descent_is_read_from_proc_and_a_stranger_is_not_a_descendant() {
        let me = std::process::id();
        assert!(descends_from(me, me));
        assert!(descends_from(me, 1), "everything descends from init");
        assert!(!descends_from(1, me));
        assert!(!descends_from(u32::MAX / 2, me), "a pid that does not exist descends from nothing");
    }
}
