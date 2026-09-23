//! Starting an agent, saying more to one, and stopping one — through the harness host.
//!
//! The host issues the conversation (`Host::start_agent`), addresses each turn to it
//! (`Host::send_to`) and stops it (`Host::stop_agent`); this file records what it did in the store
//! and hands the answer to `feed`, which reads it into the agent's session. A harness that holds
//! one conversation has the one agent `<harness>:main` — the same conversation the Lens has with
//! it — and a New agent on it continues that one rather than pretending to open a second. The
//! built-in companion answers in the Lens and holds no agents; the host refuses it, and so does
//! this.
//!
//! Each act comes in two forms: the one the screen calls, on the shell's own host, and `…_on`,
//! which takes the host — what `shell.new_agent` and friends call, and what their tests drive
//! with a host of their own.

use yantrik_harness::{Host, Turn};

use super::model::{title_of, AgentId, State};

fn host() -> Result<&'static Host, String> {
    crate::wire::harness::host().ok_or_else(|| "the harness host is not running".to_string())
}

fn builtin(harness: &str) -> bool {
    harness == crate::wire::harness::BUILTIN_ID
}

fn turn(text: &str) -> Turn {
    Turn::new(text.to_string()).with_context(crate::wire::chat::desktop_context(&crate::wire::settings::place()))
}

/// Start an agent: a mind and its first prompt. Answers with the agent it became.
pub fn start(mind: &str, prompt: &str) -> Result<AgentId, String> {
    start_on(host()?, mind, prompt, None)
}

/// Start an agent on `host`. `parent` is the agent that asked for it (`shell.new_agent`), which
/// makes it a child: its row says so, Stop on the parent stops it, and it is always a new
/// conversation — never a one-conversation harness's `main`, which is the person's own
/// conversation with that mind in the Lens.
///
/// A child starts with nothing of its parent's: its first turn is `prompt` and the desktop's
/// context, as any agent's is. No grant, request id, note or token of the parent's goes with it,
/// and the host mints its own token for it (design decision 1: "a child starts with no grants").
pub fn start_on(host: &Host, mind: &str, prompt: &str, parent: Option<&AgentId>) -> Result<AgentId, String> {
    let prompt = prompt.trim();
    if prompt.is_empty() {
        return Err("Say what the agent is to do.".into());
    }
    if builtin(mind) {
        return Err("The built-in companion answers in the Lens; it cannot be started as an agent yet.".into());
    }
    let agent = match host.start_agent(mind) {
        Ok(agent) => agent,
        // One conversation, already open: the agent is that conversation — for the person, who
        // is continuing their own. Anything else — the cap, a harness that is gone, or a parent
        // asking for a child on a harness that cannot give it one — is said as the host said it.
        Err(why) => {
            let main = AgentId::new(mind, AgentId::MAIN);
            let continues = host.agents().iter().any(|a| a.id == main && !a.conversations);
            if !continues || parent.is_some() {
                return Err(why);
            }
            main
        }
    };
    let busy = super::store().read(|s| s.agent(&agent).is_some_and(|a| a.open_turn().is_some()));
    if busy {
        return Err(format!("`{agent}` is still on its last turn; one turn at a time."));
    }
    let answer = match host.send_to(&agent, turn(prompt)) {
        Ok(answer) => answer,
        Err(why) => {
            if parent.is_some() {
                // Nothing was asked of it: let the conversation go rather than hold a place
                // under the cap for a child that never started.
                host.stop_agent(&agent);
            }
            return Err(why);
        }
    };
    let mut meta = super::feed::meta_for(&agent);
    meta.title = title_of(prompt);
    meta.parent = parent.cloned();
    super::store().upsert_agent(meta);
    super::store().open_turn(&agent, prompt);
    super::feed::record(agent.clone(), answer, false);
    Ok(agent)
}

/// Say something more to an agent.
pub fn send(agent: &AgentId, text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Ok(());
    }
    if builtin(agent.harness()) {
        return Err("The built-in companion answers in the Lens.".into());
    }
    send_on(host()?, agent, text)
}

/// Say something more to an agent, on `host`.
pub fn send_on(host: &Host, agent: &AgentId, text: &str) -> Result<(), String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(());
    }
    if builtin(agent.harness()) {
        return Err("The built-in companion answers in the Lens.".into());
    }
    let (busy, gone) = super::store().read(|s| {
        s.agent(agent)
            .map(|a| (a.open_turn().is_some(), a.state == State::HarnessGone))
            .unwrap_or((false, false))
    });
    if busy {
        return Err("It is still on its last turn; one turn at a time.".into());
    }
    if gone {
        return Err("Its harness is gone. Start it again, then ask.".into());
    }
    let answer = host.send_to(agent, turn(text))?;
    super::store().upsert_agent(super::feed::meta_for(agent));
    super::store().open_turn(agent, text);
    super::feed::record(agent.clone(), answer, false);
    Ok(())
}

/// What a Stop came to.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stopped {
    /// Whether the host had a turn, a queued turn or a live conversation to stop.
    pub stopped: bool,
    /// The agent's commands killed, each one's whole process group.
    pub commands: usize,
    /// Approval cards taken back: nobody is waiting on their answers any more.
    pub approvals: usize,
    /// Its children, stopped with it.
    pub children: Vec<AgentId>,
}

/// Stop an agent's work: the host fails the turns waiting for it, settles the one in flight and
/// tells the harness to stop; and every command the shell is running for it is killed, each one's
/// whole process group (`Jobs::kill_agent`).
pub fn stop(agent: &AgentId) -> Result<(), String> {
    if builtin(agent.harness()) {
        return Err("The built-in companion answers in the Lens and cannot be stopped from here.".into());
    }
    stop_on(host()?, agent).map(|_| ())
}

/// Stop an agent on `host`, and the agents it started: Stop on a parent stops its children
/// (design decision 1). One level, because a child cannot start agents of its own.
pub fn stop_on(host: &Host, agent: &AgentId) -> Result<Stopped, String> {
    if builtin(agent.harness()) {
        return Err("The built-in companion answers in the Lens and cannot be stopped from here.".into());
    }
    let mut stopped = stop_one(host, agent);
    let children = super::store().read(|s| s.children_of(agent));
    let live: Vec<AgentId> = host.agents().into_iter().map(|a| a.id).collect();
    for child in children {
        let working = super::store().read(|s| s.agent(&child).is_some_and(|a| a.busy()));
        if !working && !live.contains(&child) {
            continue;
        }
        let theirs = stop_one(host, &child);
        super::store().note(&child, &format!("Stopped with `{agent}`, the agent that started it."));
        stopped.commands += theirs.commands;
        stopped.approvals += theirs.approvals;
        stopped.children.push(child);
    }
    Ok(stopped)
}

fn stop_one(host: &Host, agent: &AgentId) -> Stopped {
    let killed = crate::control_agent_terminal::jobs().kill_agent(agent);
    let stopped = host.stop_agent(agent);
    // A card for work that is no longer happening is refused, never granted. The approval store's
    // own tick redraws the Lens, and the pane with it.
    let approvals = crate::approvals::withdraw_for_agent(&agent.0).len();
    let note = match (stopped, killed.len()) {
        (_, 0) if !stopped => "Stop asked; nothing was running.".to_string(),
        (_, 0) => "Stop asked.".to_string(),
        (_, 1) => "Stop asked, and its one running command killed.".to_string(),
        (_, n) => format!("Stop asked, and its {n} running commands killed."),
    };
    super::store().note(agent, &note);
    Stopped { stopped, commands: killed.len(), approvals, children: Vec::new() }
}

/// Whether an agent can still be spoken to: its harness attached, and — for a harness with
/// conversations — the conversation still live. `<harness>:main` is always there while its harness
/// is attached.
pub fn reachable(agent: &AgentId, live: &[AgentId], attached: &[String]) -> bool {
    let harness = agent.harness();
    !builtin(harness)
        && attached.iter().any(|a| a == harness)
        && (agent.conversation() == AgentId::MAIN || live.contains(agent))
}
