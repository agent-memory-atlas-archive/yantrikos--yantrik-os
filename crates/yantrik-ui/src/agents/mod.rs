//! Agents — one pane per agent, with its work inside it.
//!
//! An agent is one conversation with one mind (`<harness>:<conversation>`). This module keeps every
//! agent the desktop knows of: its state, its session — the person's prompts, the mind's text and
//! thinking, one card per tool call with that call's own output inside it — and the details the
//! Agents screen counts. The screen (`wire::agents`, `agents.slint`) and every popped-out agent
//! window draw from this one store, so they cannot disagree. See
//! `design/agents-workspace-2026-09-23.md`.
//!
//! # Feeding it
//!
//! The store is fed through [`Agents`], from any thread:
//!
//! | call | who calls it |
//! |---|---|
//! | `upsert_agent(AgentMeta)` | the host, when a conversation starts |
//! | `open_turn(&id, prompt)` / `close_turn(&id, ok)` | the host, around each turn |
//! | `text(&id, delta)` | the host, for each `harness.chunk` |
//! | `event(&id, &Event, Provenance)` | the host's `Chunk::Event` (`Reported`); anything the shell saw itself (`Verified`) |
//! | `command_started / command_output / command_finished` | the agent terminal: `Jobs::on_output(agent, job, bytes)` and `Jobs::on_finish` (`Verified`, bytes as read off the PTY) |
//! | `set_state(&id, State)` | the host (`HarnessGone`), the approval card and the terminal (`WaitingForYou`) |
//! | `approval_asked / approval_answered / approval_settled` | `control_approvals`: a request carrying this agent's token, and how it came out ([`settle_approvals`]) |
//! | `remove_agent(&id)` | Close |
//!
//! Every turn the shell sends a mind passes through `feed`, which does the host's part: the
//! prompt, the text, the events, and — for a harness that writes only text — its trail lines.

pub mod feed;
pub mod launch;
pub mod model;
pub mod store;

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

pub use model::{AgentId, AgentMeta, ApprovalOutcome, CallState, Event, Provenance, State, Stream, Tab};
pub use store::Store;

/// The title every popped-out agent window starts with, so the window list can tell an agent's
/// window from anything else a task might be named after.
pub const WINDOW_TITLE_PREFIX: &str = "Agent · ";

/// Where sessions are kept: `$XDG_DATA_HOME/yantrik/agents`, or `~/.local/share/yantrik/agents`.
pub fn dir() -> PathBuf {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/")).join(".local").join("share")
        });
    data.join("yantrik").join("agents")
}

/// How often changed sessions are written out.
const SAVE_EVERY: Duration = Duration::from_secs(2);

/// The store, shared by everything in the shell that feeds it or draws it.
pub struct Agents {
    store: Mutex<Store>,
    dir: PathBuf,
    saved: Mutex<Instant>,
}

static AGENTS: OnceLock<Agents> = OnceLock::new();

/// The shell's agents, loaded from disk the first time anything asks.
///
/// Under test, a directory of the test run's own: the agent terminal feeds this store from every
/// command a test runs, and a test must neither read the person's saved sessions nor write into
/// them.
pub fn store() -> &'static Agents {
    AGENTS.get_or_init(|| {
        #[cfg(not(test))]
        let dir = dir();
        #[cfg(test)]
        let dir = std::env::temp_dir().join(format!("yantrik-agents-under-test-{}", std::process::id()));
        let store = Store::load(&dir, Box::new(model::now));
        tracing::info!(agents = store.agents().len(), dir = %dir.display(), "Agents loaded");
        Agents { store: Mutex::new(store), dir, saved: Mutex::new(Instant::now()) }
    })
}

impl Agents {
    fn lock(&self) -> MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(|e| e.into_inner())
    }

    // The input API. Each is the store's own, under the lock; see `store.rs` for the rules.

    pub fn upsert_agent(&self, meta: AgentMeta) {
        self.lock().upsert_agent(meta)
    }

    pub fn remove_agent(&self, id: &AgentId) -> bool {
        self.lock().remove_agent(id)
    }

    pub fn open_turn(&self, id: &AgentId, prompt: &str) {
        self.lock().open_turn(id, prompt)
    }

    pub fn text(&self, id: &AgentId, delta: &str) {
        self.lock().text(id, delta)
    }

    pub fn event(&self, id: &AgentId, event: &Event, provenance: Provenance) {
        self.lock().event(id, event, provenance)
    }

    pub fn close_turn(&self, id: &AgentId, ok: bool) {
        self.lock().close_turn(id, ok)
    }

    pub fn set_state(&self, id: &AgentId, state: State) {
        self.lock().set_state(id, state)
    }

    pub fn command_started(&self, id: &AgentId, job: &str, command: &str, cwd: &str) {
        self.lock().command_started(id, job, command, cwd)
    }

    pub fn command_output(&self, id: &AgentId, job: &str, bytes: &[u8]) {
        self.lock().command_output(id, job, bytes)
    }

    pub fn command_finished(&self, id: &AgentId, job: &str, command: &str, exit_code: Option<i32>, killed: bool) {
        self.lock().command_finished(id, job, command, exit_code, killed)
    }

    pub fn trail_call(&self, id: &AgentId, call: &crate::trail::ToolCall) {
        self.lock().trail_call(id, call)
    }

    pub fn note(&self, id: &AgentId, text: &str) {
        self.lock().note(id, text)
    }

    pub fn replace_text(&self, id: &AgentId, text: &str) {
        self.lock().replace_text(id, text)
    }

    pub fn approval_asked(&self, id: &AgentId, request: &str, what: &str) {
        self.lock().approval_asked(id, request, what)
    }

    pub fn approval_answered(&self, id: &AgentId, request: &str, allowed: bool) {
        self.lock().approval_answered(id, request, allowed)
    }

    pub fn approval_settled(&self, id: &AgentId, request: &str, outcome: ApprovalOutcome, record: &str) {
        self.lock().approval_settled(id, request, outcome, record)
    }

    /// Read the store. Keep it short: every feeder waits on the same lock.
    pub fn read<R>(&self, f: impl FnOnce(&Store) -> R) -> R {
        f(&self.lock())
    }

    pub fn revision(&self) -> u64 {
        self.lock().revision()
    }

    /// Write out what changed, at most every [`SAVE_EVERY`]. The files are written off the lock and
    /// off the UI thread.
    pub fn save_if_due(&self) {
        {
            let mut saved = self.saved.lock().unwrap_or_else(|e| e.into_inner());
            if saved.elapsed() < SAVE_EVERY {
                return;
            }
            *saved = Instant::now();
        }
        let (writes, deletes) = self.lock().take_dirty(&self.dir);
        if writes.is_empty() && deletes.is_empty() {
            return;
        }
        let dir = self.dir.clone();
        let _ = std::thread::Builder::new().name("agents-save".into()).spawn(move || {
            if let Err(e) = store::write_all(&dir, &writes, &deletes) {
                tracing::warn!(error = %e, dir = %dir.display(), "Could not save the agents' sessions");
            }
        });
    }
}

/// Bring every agent's waiting approvals up to date with the shell's approval store — the one
/// place a request is answered, whichever card the person pressed, the Lens's or the pane's.
/// `status_of` says where one request stands: `None` while it is still waiting, or how it came
/// out and the line it leaves. `control_approvals` calls this each time it redraws the cards, so
/// an answer, an expiry and a withdrawal all reach the pane on the same tick they reach the Lens.
///
/// The store's lock is not held while `status_of` runs: that reads the approval store, and
/// nothing holds both locks at once.
pub fn settle_approvals(status_of: impl Fn(&str) -> Option<(ApprovalOutcome, String)>) {
    let waiting: Vec<(AgentId, String)> = store().read(|s| {
        s.agents()
            .iter()
            .flat_map(|a| a.pending_approvals.iter().map(move |r| (a.meta.id.clone(), r.clone())))
            .collect()
    });
    for (agent, request) in waiting {
        if let Some((outcome, record)) = status_of(&request) {
            store().approval_settled(&agent, &request, outcome, &record);
        }
    }
}

/// What `describe shell` says under `agents`: the counts per tab, and one entry per agent with its
/// state, whether it is waiting on the person, the commands the shell is running for it now, when
/// it last did anything, and what it has done.
pub fn for_describe() -> serde_json::Value {
    // Read before the store's lock is taken: the terminal has its own.
    let running = crate::control_agent_terminal::running_jobs();
    let now = model::now();
    store().read(|s| {
        let counts = s.counts();
        let agents: Vec<serde_json::Value> = s
            .list(Tab::All, None)
            .iter()
            .filter_map(|id| s.agent(id).map(|a| (a, s.details(id).unwrap_or_default())))
            .map(|(a, d)| {
                let jobs: Vec<&serde_json::Value> =
                    running.iter().filter(|(agent, _)| agent == &a.meta.id).map(|(_, job)| job).collect();
                let waiting_for_input = jobs.iter().any(|j| j["waiting_for_input"] == true);
                serde_json::json!({
                    "id": a.meta.id,
                    "mind": a.meta.mind,
                    "title": a.meta.title,
                    "state": a.state.key(),
                    "since": a.since,
                    "status": a.status,
                    // Waiting on the person: an approval card up for it, or one of its commands
                    // sitting at a prompt only the person can answer.
                    "needs_you": a.state == State::WaitingForYou || !a.pending_approvals.is_empty() || waiting_for_input,
                    "pending_approvals": a.pending_approvals,
                    // The commands the shell is running for it right now — its own terminal's.
                    "running_jobs": jobs,
                    // When anything last happened to it, and how long ago.
                    "last_activity": a.touched,
                    "last_activity_secs_ago": now.saturating_sub(a.touched),
                    "parent": a.meta.parent,
                    "children": s.children_of(&a.meta.id),
                    "turns": d.turns,
                    "calls": d.calls,
                    "failed_calls": d.failed_calls,
                    // What the shell itself ran, never what a harness says it ran.
                    "commands": d.commands.iter().map(|(line, exit, state)| serde_json::json!({
                        "command": line, "exit_code": exit, "state": state.key(),
                    })).collect::<Vec<_>>(),
                    "files": d.files,
                    "approvals": { "asked": d.approvals_asked, "answered": d.approvals_answered },
                    "one_conversation": !a.meta.conversations,
                })
            })
            .collect();
        serde_json::json!({
            "active": counts[0],
            "needs_you": counts[1],
            "complete": counts[2],
            "all": counts[3],
            "agents": agents,
        })
    })
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use serde_json::json;

    /// `describe shell` → `agents`: each agent's id, mind, title and state, whether it needs the
    /// person, the commands the shell is running for it now, when it last did anything, and the
    /// agents it started — and never a token.
    #[test]
    fn describe_says_who_needs_you_what_runs_and_when_it_last_did_anything() {
        let pi = AgentId::new("pi", "c-describe");
        let kid = AgentId::new("pi", "c-describe-kid");
        store().open_turn(&pi, "tidy the photos folder");
        let mut meta = AgentMeta::new(kid.clone(), "pi");
        meta.parent = Some(pi.clone());
        store().upsert_agent(meta);
        store().approval_asked(&pi, "appr-describe", "files.move");
        let jobs = crate::control_agent_terminal::jobs();
        let job = jobs.start(&pi, "sleep 30", None).unwrap();

        let described = for_describe();
        let entry = described["agents"]
            .as_array()
            .and_then(|all| all.iter().find(|a| a["id"] == "pi:c-describe"))
            .unwrap_or_else(|| panic!("pi is not listed: {described}"))
            .clone();
        assert_eq!((entry["mind"].clone(), entry["title"].clone()), (json!("pi"), json!("tidy the photos folder")));
        assert_eq!(entry["state"], "waiting_for_you", "{entry}");
        assert_eq!(entry["needs_you"], true, "{entry}");
        assert_eq!(entry["pending_approvals"], json!(["appr-describe"]));
        assert!(
            entry["running_jobs"].as_array().unwrap().iter().any(|j| j["job"] == job.0.as_str() && j["command"] == "sleep 30"),
            "{entry}"
        );
        assert!(entry["last_activity"].as_u64().is_some_and(|t| t > 1_700_000_000), "{entry}");
        assert!(entry["last_activity_secs_ago"].as_u64().is_some_and(|s| s < 60), "{entry}");
        assert_eq!(entry["children"], json!(["pi:c-describe-kid"]));
        let kid_entry = described["agents"].as_array().unwrap().iter().find(|a| a["id"] == "pi:c-describe-kid").unwrap();
        assert_eq!((kid_entry["parent"].clone(), kid_entry["needs_you"].clone()), (json!("pi:c-describe"), json!(false)));
        assert!(!described.to_string().contains("agent_token"), "{described}");
        jobs.kill(&pi, &job).unwrap();
    }
}
