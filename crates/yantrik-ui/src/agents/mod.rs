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

pub use model::{AgentId, AgentMeta, CallState, Event, Provenance, State, Stream, Tab};
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
pub fn store() -> &'static Agents {
    AGENTS.get_or_init(|| {
        let dir = dir();
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

/// What `describe shell` says under `agents`: the counts per tab, and one entry per agent with its
/// state and what it has done.
pub fn for_describe() -> serde_json::Value {
    store().read(|s| {
        let counts = s.counts();
        let agents: Vec<serde_json::Value> = s
            .list(Tab::All, None)
            .iter()
            .filter_map(|id| s.agent(id).map(|a| (a, s.details(id).unwrap_or_default())))
            .map(|(a, d)| {
                serde_json::json!({
                    "id": a.meta.id,
                    "mind": a.meta.mind,
                    "title": a.meta.title,
                    "state": a.state.key(),
                    "since": a.since,
                    "status": a.status,
                    "parent": a.meta.parent,
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
