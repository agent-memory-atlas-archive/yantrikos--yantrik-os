//! Starting an agent, saying more to one, and stopping one — through the harness host.
//!
//! The host issues the conversation (`Host::start_agent`), addresses each turn to it
//! (`Host::send_to`) and stops it (`Host::stop_agent`); this file records what it did in the store
//! and hands the answer to `feed`, which reads it into the agent's session. A harness that holds
//! one conversation has the one agent `<harness>:main` — the same conversation the Lens has with
//! it — and a New agent on it continues that one rather than pretending to open a second. The
//! built-in companion answers in the Lens and holds no agents; the host refuses it, and so does
//! this.

use yantrik_harness::Turn;

use super::model::{title_of, AgentId, State};

fn host() -> Result<&'static yantrik_harness::Host, String> {
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
    let prompt = prompt.trim();
    if prompt.is_empty() {
        return Err("Say what the agent is to do.".into());
    }
    if builtin(mind) {
        return Err("The built-in companion answers in the Lens; it cannot be started as an agent yet.".into());
    }
    let host = host()?;
    let agent = match host.start_agent(mind) {
        Ok(agent) => agent,
        // One conversation, already open: the agent is that conversation. Anything else — the
        // cap, a harness that is gone — is said as the host said it.
        Err(why) => {
            let main = AgentId::new(mind, AgentId::MAIN);
            let continues = host.agents().iter().any(|a| a.id == main && !a.conversations);
            if !continues {
                return Err(why);
            }
            main
        }
    };
    let busy = super::store().read(|s| s.agent(&agent).is_some_and(|a| a.open_turn().is_some()));
    if busy {
        return Err(format!("`{agent}` is still on its last turn; one turn at a time."));
    }
    let answer = host.send_to(&agent, turn(prompt))?;
    let mut meta = super::feed::meta_for(&agent);
    meta.title = title_of(prompt);
    super::store().upsert_agent(meta);
    super::store().open_turn(&agent, prompt);
    super::feed::record(agent.clone(), answer, false);
    Ok(agent)
}

/// Say something more to an agent.
pub fn send(agent: &AgentId, text: &str) -> Result<(), String> {
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
    let answer = host()?.send_to(agent, turn(text))?;
    super::store().upsert_agent(super::feed::meta_for(agent));
    super::store().open_turn(agent, text);
    super::feed::record(agent.clone(), answer, false);
    Ok(())
}

/// Stop an agent's work: the host fails the turns waiting for it, settles the one in flight and
/// tells the harness to stop; and every command the shell is running for it is killed, each one's
/// whole process group (`Jobs::kill_agent`).
pub fn stop(agent: &AgentId) -> Result<(), String> {
    if builtin(agent.harness()) {
        return Err("The built-in companion answers in the Lens and cannot be stopped from here.".into());
    }
    let killed = crate::control_agent_terminal::jobs().kill_agent(agent);
    let stopped = host()?.stop_agent(agent);
    let note = match (stopped, killed.len()) {
        (_, 0) if !stopped => "Stop asked; nothing was running.".to_string(),
        (_, 0) => "Stop asked.".to_string(),
        (_, 1) => "Stop asked, and its one running command killed.".to_string(),
        (_, n) => format!("Stop asked, and its {n} running commands killed."),
    };
    super::store().note(agent, &note);
    Ok(())
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
