//! Agents on the shell's surface: `new_agent`, `send_to_agent`, `stop_agent`, `read_agent` — how a
//! mind hands work to another agent — and `show_agent`, which puts one on the person's screen.
//!
//! See `design/agents-workspace-2026-09-23.md`, decision 1 ("A mind can spin up agents too") and
//! the work table's row 4.
//!
//! # Who is asking
//!
//! Never an argument. The caller's own agent comes from the agent token that rides beside `args`
//! (`control::agent_token()`), checked against the kernel's account of the caller by the same
//! resolver the agent terminal uses (`control_agent_terminal::calling_agent`). A call with no
//! token is the person's own `yos act`, or a caller that runs as no agent — treated as the person,
//! as every door on this socket treats it. A call whose token is not believed is refused: a caller
//! that presented a token is not the person.
//!
//! # The rules a mind meets
//!
//! - `new_agent` is **sensitive**: in `ask` mode the person sees a card, in the asking agent's
//!   pane. An agent another agent started cannot start agents of its own (**depth one**); an agent
//!   holds at most [`MAX_CHILDREN`] live children; the host's global cap still applies. A child
//!   starts with **no grants**: nothing of its parent's goes with it (`launch::start_on`), and a
//!   grant asked for one agent is not spent by another (`control_approvals::grant_belongs`).
//! - `send_to_agent` and `stop_agent` are **standard**, and an agent may use them only on the
//!   agents it started. The person may use them on any agent.
//! - `read_agent` is **safe**. An agent may read itself and the agents it started.
//! - `show_agent` is **safe**: it puts a pane on screen, as `show_screen` does.
//!
//! # Handing work to a role
//!
//! `hand_off {role, task, context?, wait_seconds?}` starts a role from the agent catalog
//! (`agents::catalog`, design/desk-and-mind-2026-09-23.md section 5) on its first attached mind
//! that can give it a conversation of its own, with the role's brief, the task and the context as
//! its first turn. It is gated like `new_agent` — **sensitive**, the same depth-one and
//! three-children rules through [`may_start_child`], the child starts with nothing of its parent's
//! — and the role's **reach caps it further**: before its first turn is sent the agent is held to
//! the role's surfaces and ceiling on every door (`agents::reaches`), so an act outside them is
//! refused whoever the door is. An agent held to a reach cannot start a plain agent (which has
//! none) and cannot hand work to a role whose ceiling is above its own. With `wait_seconds` the
//! answer waits for the role's first turn to end, off the UI thread, and hands back what it said.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use slint::ComponentHandle;
use yantrik_app_runtime::control::{self, Action, App as ControlSurface, Param};
use yantrik_harness::Host;
use yantrik_ipc_transport::gate;

use crate::agents::catalog::{self, Catalog};
use crate::agents::model::{Item, Turn};
use crate::agents::{self, launch, reaches, AgentId, Store};
use crate::App;

/// How many live agents one agent may have started (design decision 1).
pub const MAX_CHILDREN: usize = 3;

/// The longest `hand_off` waits for a role's answer, as `agent_run` waits for a command.
pub const HAND_OFF_WAIT_MOST: u64 = 600;

/// The most `context` a hand-off carries into the role's first turn.
pub const CONTEXT_MOST_BYTES: usize = 32 * 1024;

/// The most of a role's answer `hand_off` hands back; `read_agent` has all of it.
const ANSWER_MOST_BYTES: usize = 32 * 1024;

/// How many turns `read_agent` gives when not told, and the most it gives.
const READ_TURNS: usize = 3;
const READ_TURNS_MOST: usize = 20;

/// Who a call is for.
#[derive(Clone, Debug, PartialEq)]
pub enum Caller {
    /// No agent token came with it: the person's own `yos act`, or a caller that runs as no agent.
    NoAgent,
    /// The agent its token named, checked against the caller's process.
    Agent(AgentId),
}

/// The caller of the call being dispatched. Read on the handler's thread, inside the dispatch.
fn caller() -> Result<Caller, String> {
    match crate::control_agent_terminal::calling_agent() {
        None => Ok(Caller::NoAgent),
        Some(Ok(agent)) => Ok(Caller::Agent(agent)),
        Some(Err(why)) => Err(why),
    }
}

fn host() -> Result<&'static Host, String> {
    crate::wire::harness::host().ok_or_else(|| "the harness host is not running yet".to_string())
}

fn text(args: &Value, name: &str) -> String {
    args.get(name).and_then(Value::as_str).map(str::trim).unwrap_or_default().to_string()
}

/// An agent named by a caller: `<harness>:<conversation>`, as `describe shell` lists them.
fn agent_arg(args: &Value) -> Result<AgentId, String> {
    let named = text(args, "agent");
    match named.split_once(':') {
        Some((harness, conversation)) if !harness.is_empty() && !conversation.is_empty() => Ok(AgentId(named)),
        _ => Err(format!(
            "`agent` is `{named}`; an agent is named `<mind>:<conversation>`, e.g. `pi:c-7f3a91`, \
             as new_agent answered or `describe shell` lists under `agents`."
        )),
    }
}

/// Said once, in every description, because it is the only documentation a mind reads.
const WHO: &str = " You are the agent named by the agent token your call carries beside `args` \
                   (YANTRIK_AGENT_TOKEN in yos's environment) — never an argument; with no token, \
                   the call is the person's.";

/// The six actions as published.
fn specs() -> [Action; 6] {
    [
        // Sensitive: it starts work that runs as the person, and more of it than one call.
        Action::new(
            "new_agent",
            &format!(
                "Start another agent — a new conversation with a mind — and give it a task. It \
                 works on its own, in its own pane on the Agents screen, and its row says you \
                 started it. Answers at once with its id; `read_agent` shows how it is going, \
                 `send_to_agent` says more, `stop_agent` stops it. It starts with nothing of \
                 yours: no grants, and anything it needs allowed is asked for again, in its own \
                 pane. An agent another agent started cannot start agents; one agent holds at \
                 most {MAX_CHILDREN} running at once, and the desktop caps how many run in all.{WHO}"
            ),
        )
        .risk("sensitive")
        .arg(Param::text("mind").describe(
            "Which mind: an attached one's id as `describe shell` lists under `minds` (pi, deepseek)",
        ))
        .arg(Param::text("task").describe("What it is to do: its first prompt, in full")),
        Action::new(
            "send_to_agent",
            &format!(
                "Say more to an agent you started — its next prompt. Answers once it is sent; \
                 `read_agent` shows the answer as it comes. One turn at a time: an agent still \
                 on its last turn is not sent another.{WHO}"
            ),
        )
        .arg(Param::text("agent").describe("The agent, as new_agent answered: `<mind>:<conversation>`"))
        .arg(Param::text("text").describe("What to say to it")),
        Action::new(
            "stop_agent",
            &format!(
                "Stop an agent you started: its turn is ended, every command it is running is \
                 killed (the whole process group), any approval it is waiting on is withdrawn, \
                 and the agents it started stop with it. Its pane stays readable.{WHO}"
            ),
        )
        .arg(Param::text("agent").describe("The agent, as new_agent answered: `<mind>:<conversation>`")),
        Action::new(
            "read_agent",
            &format!(
                "Read an agent's recent turns as text: what it was asked, what it said, each \
                 call it made with how it went and the end of its output, and what the person \
                 was asked for it. You may read yourself and the agents you started.{WHO}"
            ),
        )
        .risk("safe")
        .arg(Param::text("agent").describe("The agent, as new_agent answered: `<mind>:<conversation>`"))
        .arg(
            Param::number("last")
                .optional()
                .describe("How many of its latest turns. Default 3, at most 20"),
        ),
        Action::new(
            "show_agent",
            "Put one agent's pane on the person's screen: the Agents screen, with that agent \
             selected. Changes nothing about the agent.",
        )
        .risk("safe")
        .arg(Param::text("agent").describe("The agent: `<mind>:<conversation>`")),
        // Sensitive, as `new_agent` is: it starts work that runs as the person.
        Action::new(
            "hand_off",
            &format!(
                "Hand a piece of work to a role from the agent catalog — Researcher, Planner, \
                 Coder, Reviewer, Red team, Writer, Chair, Scribe, or the person's own; `describe \
                 shell` lists them under `catalog` with what each is for, what it may touch and \
                 whether a mind it runs on is attached. It starts that role's agent on its first \
                 attached mind, in its own pane, with the role's standing instructions, your task \
                 and your context as its first prompt. It can act only within the role's reach: \
                 anything else it tries is refused. Without `wait_seconds` it answers at once with \
                 the agent's id; with it, it waits up to that long for the role's answer and hands \
                 it back. It starts with nothing of yours: no grants. An agent another agent \
                 started cannot hand off; one agent holds at most {MAX_CHILDREN} running at once, \
                 and the desktop caps how many run in all.{WHO}"
            ),
        )
        .risk("sensitive")
        .arg(Param::text("role").describe(
            "The role: its id as `describe shell` lists under `catalog` (reviewer, coder, red-team …), or its name",
        ))
        .arg(Param::text("task").describe("What it is to do, in full: its first prompt, after the role's own instructions"))
        .arg(Param::text("context").optional().describe(
            "What it should read first — the change to review, the answers to weigh. At most 32 KiB",
        ))
        .arg(Param::number("wait_seconds").optional().describe(
            "Seconds to wait for its answer. Left out, the answer is its id at once. At most 600",
        )),
    ]
}

/// The six actions, for the shell's surface.
pub fn actions(surface: ControlSurface, ui: &App) -> ControlSurface {
    let [new, send, stop, read, show, hand] = specs();
    let show_ui = ui.as_weak();
    // ── Agents catalog: this shell's own dispatch holds a role's agent to its reach from the
    // registry, in-process, as it spends grants in-process; and nothing an earlier run published
    // is held any more — those tokens are gone.
    yantrik_ipc_transport::reach::read_reach_with(reaches::lookup);
    reaches::reset();
    surface
        .action(new, |args| new_agent(host()?, &caller()?, &text(args, "mind"), &text(args, "task")))
        .action(send, |args| send_to_agent(host()?, &caller()?, &agent_arg(args)?, &text(args, "text")))
        .action(stop, |args| stop_agent(host()?, &caller()?, &agent_arg(args)?))
        .action(read, |args| read_agent(&caller()?, &agent_arg(args)?, args.get("last")))
        .action(show, move |args| {
            let agent = agent_arg(args)?;
            let ui = show_ui.upgrade().ok_or("the shell is gone")?;
            let known = agents::store().read(|s| s.agent(&agent).is_some());
            if !known {
                return Err(format!("there is no agent `{agent}` on this desktop; `describe shell` lists them under `agents`."));
            }
            ui.global::<crate::AgentsState>().invoke_show_agent(agent.0.as_str().into());
            let raised = crate::windows::raise_shell().is_ok();
            Ok(json!({ "showing": agent, "raised": raised }))
        })
        .action(hand, |args| {
            let wait = wait_arg(args)?;
            let handed = hand_off(
                host()?,
                &caller()?,
                &Catalog::load(),
                &text(args, "role"),
                &text(args, "task"),
                &text(args, "context"),
            )?;
            let Some(wait) = wait else { return Ok(handed.answer(None)) };
            // The wait is for the role's answer, which may be minutes away: off the UI thread.
            let work = move || Ok(handed.answer(Some((wait, wait_for_answer(&handed.agent, wait)))));
            control::answer_later(work).map(|()| json!({ "answering": "off the UI thread" })).or_else(|work| work())
        })
}

/// `wait_seconds`: none, or a number of seconds up to [`HAND_OFF_WAIT_MOST`]. Zero is none.
fn wait_arg(args: &Value) -> Result<Option<Duration>, String> {
    let Some(given) = args.get("wait_seconds").filter(|v| !v.is_null()) else { return Ok(None) };
    let secs = given
        .as_f64()
        .or_else(|| given.as_str().and_then(|s| s.trim().parse().ok()))
        .ok_or_else(|| "`wait_seconds` is a number of seconds.".to_string())?;
    if !(0.0..=HAND_OFF_WAIT_MOST as f64).contains(&secs) {
        return Err(format!(
            "`wait_seconds` is between 0 and {HAND_OFF_WAIT_MOST}. Left out, hand_off answers at once \
             with the agent's id, and `read_agent` shows its answer when it comes."
        ));
    }
    Ok((secs > 0.0).then(|| Duration::from_secs_f64(secs)))
}

// ── The rules ─────────────────────────────────────────────────────

/// May `parent` start another agent? Depth one, and at most [`MAX_CHILDREN`] of its own still live
/// in the host. The global cap is the host's, met when the agent is started.
pub fn may_start_child(parent: &AgentId, store: &Store, live: &[AgentId]) -> Result<(), String> {
    if let Some(grand) = store.agent(parent).and_then(|a| a.meta.parent.clone()) {
        return Err(format!(
            "`{parent}` was started by `{grand}`, and an agent another agent started cannot start \
             agents of its own: one level only. Say in your answer what else needs doing, and \
             `{grand}` can start it."
        ));
    }
    let held: Vec<AgentId> = store.children_of(parent).into_iter().filter(|c| live.contains(c)).collect();
    if held.len() >= MAX_CHILDREN {
        return Err(format!(
            "`{parent}` already has {} agents running ({}), the most one agent may hold at once. \
             `stop_agent` one that is done, or wait for one to finish, then start another.",
            held.len(),
            held.iter().map(|a| a.0.as_str()).collect::<Vec<_>>().join(", ")
        ));
    }
    Ok(())
}

/// May `caller` send to or stop `target`? The person may, any agent; an agent, only one it started.
pub fn may_direct(caller: &Caller, target: &AgentId, store: &Store, verb: &str) -> Result<(), String> {
    match caller {
        Caller::NoAgent => Ok(()),
        Caller::Agent(me) if store.agent(target).and_then(|a| a.meta.parent.as_ref()) == Some(me) => Ok(()),
        Caller::Agent(_) => Err(format!(
            "`{target}` is not an agent you started, so you cannot {verb} it. An agent can {verb} \
             only the agents it started with new_agent; the person can {verb} any from the Agents \
             screen."
        )),
    }
}

/// May `caller` read `target`? The person may read any; an agent, itself and the agents it started.
pub fn may_read(caller: &Caller, target: &AgentId, store: &Store) -> Result<(), String> {
    match caller {
        Caller::Agent(me) if me == target => Ok(()),
        _ => may_direct(caller, target, store, "read"),
    }
}

/// The mind a caller named, as the host knows it: its id, or — for a caller that used the name a
/// person reads — the id of the one attached mind of that name.
fn mind_id(host: &Host, named: &str) -> Result<String, String> {
    let minds = host.list();
    if let Some(mind) = minds.iter().find(|m| m.id == named) {
        return Ok(mind.id.clone());
    }
    let lower = named.to_lowercase();
    if let Some(mind) = minds.iter().find(|m| m.name.to_lowercase() == lower) {
        return Ok(mind.id.clone());
    }
    let attached: Vec<&str> = minds.iter().filter(|m| !m.builtin).map(|m| m.id.as_str()).collect();
    Err(if attached.is_empty() {
        format!("no mind called `{named}` is attached, and none is: an agent needs an attached mind")
    } else {
        format!("no mind called `{named}` is attached; attached: {}", attached.join(", "))
    })
}

// ── The acts ──────────────────────────────────────────────────────

pub fn new_agent(host: &Host, caller: &Caller, mind: &str, task: &str) -> Result<Value, String> {
    if mind.trim().is_empty() {
        return Err("`mind` is empty: an attached mind's id, as `describe shell` lists under `minds`.".into());
    }
    if task.trim().is_empty() {
        return Err("`task` is empty: what the new agent is to do.".into());
    }
    let parent = match caller {
        Caller::NoAgent => None,
        Caller::Agent(me) => Some(me),
    };
    if let Some(parent) = parent {
        // A plain agent has no reach, so one held to a reach may not start one: that would be a
        // way out of its own.
        if let Some(mine) = reaches::of(parent) {
            return Err(format!(
                "`{parent}` is the {}, which works within a reach, and an agent started on a mind \
                 alone has none. Hand the work to a role from the catalog with hand_off instead.",
                mine.name
            ));
        }
        let live: Vec<AgentId> = host.agents().into_iter().map(|a| a.id).collect();
        agents::store().read(|s| may_start_child(parent, s, &live))?;
    }
    let mind = mind_id(host, mind.trim())?;
    let agent = launch::start_on(host, &mind, task, parent)?;
    tracing::info!(agent = %agent, parent = ?parent.map(|p| p.to_string()), "an agent was started from the socket");
    let said = format!(
        "Started `{agent}` on {mind}{}. It is working on it now, in its own pane. `read_agent` with \
         agent `{agent}` shows how it is going; `send_to_agent` says more; `stop_agent` stops it. \
         It has none of your grants: anything it needs allowed is asked for again, in its pane.",
        parent.map(|p| format!(", started by `{p}`")).unwrap_or_default(),
    );
    Ok(json!({ "agent": agent, "mind": mind, "parent": parent, "state": "thinking", "said": said }))
}

pub fn send_to_agent(host: &Host, caller: &Caller, target: &AgentId, text: &str) -> Result<Value, String> {
    if text.trim().is_empty() {
        return Err("`text` is empty: what to say to it.".into());
    }
    agents::store().read(|s| may_direct(caller, target, s, "send to"))?;
    launch::send_on(host, target, text)?;
    Ok(json!({
        "agent": target,
        "sent": true,
        "said": format!("Sent to `{target}`; it is working on it now. `read_agent` shows its answer as it comes."),
    }))
}

pub fn stop_agent(host: &Host, caller: &Caller, target: &AgentId) -> Result<Value, String> {
    agents::store().read(|s| may_direct(caller, target, s, "stop"))?;
    let stopped = launch::stop_on(host, target)?;
    let mut said = if stopped.stopped || stopped.commands > 0 {
        format!("Stopped `{target}`")
    } else {
        format!("`{target}` had nothing running; it is stopped all the same")
    };
    if stopped.commands > 0 {
        said.push_str(&format!(", and killed {} command{}", stopped.commands, if stopped.commands == 1 { "" } else { "s" }));
    }
    if !stopped.children.is_empty() {
        said.push_str(&format!(
            "; the agents it started stopped with it ({})",
            stopped.children.iter().map(|c| c.0.as_str()).collect::<Vec<_>>().join(", ")
        ));
    }
    said.push_str(". Its pane stays readable on the Agents screen.");
    Ok(json!({
        "agent": target,
        "stopped": stopped.stopped,
        "commands_killed": stopped.commands,
        "approvals_withdrawn": stopped.approvals,
        "children_stopped": stopped.children,
        "said": said,
    }))
}

pub fn read_agent(caller: &Caller, target: &AgentId, last: Option<&Value>) -> Result<Value, String> {
    let last = match last.filter(|v| !v.is_null()) {
        None => READ_TURNS,
        Some(given) => {
            let n = given
                .as_f64()
                .or_else(|| given.as_str().and_then(|s| s.trim().parse().ok()))
                .ok_or_else(|| "`last` is a number of turns.".to_string())?;
            if !(1.0..=READ_TURNS_MOST as f64).contains(&n) {
                return Err(format!("`last` is between 1 and {READ_TURNS_MOST} turns."));
            }
            n as usize
        }
    };
    agents::store().read(|s| {
        may_read(caller, target, s)?;
        let agent = s
            .agent(target)
            .ok_or_else(|| format!("there is no agent `{target}` on this desktop; `describe shell` lists them under `agents`."))?;
        let transcript = s.transcript(target, last).unwrap_or_default();
        Ok(json!({
            "agent": target,
            "mind": agent.meta.mind,
            "state": agent.state.key(),
            "needs_you": !agent.pending_approvals.is_empty() || agent.state == agents::State::WaitingForYou,
            "turns": agent.turns.len(),
            "text": transcript,
        }))
    })
}

// ── Handing work to a role ────────────────────────────────────────

/// What `hand_off` started.
#[derive(Clone, Debug)]
pub struct Handed {
    pub agent: AgentId,
    pub role: catalog::Role,
    pub mind: String,
}

/// How a role's first turn came out, as far as a wait saw.
#[derive(Clone, Debug, PartialEq)]
pub struct Answered {
    /// Its first turn ended within the wait.
    pub done: bool,
    /// And ended well — not failed, not stopped.
    pub ok: bool,
    /// What it said in that turn.
    pub text: String,
}

/// Start `role` on `task`, for `caller`: the rules of `new_agent`, then the role's own — its first
/// attached mind that can give it a conversation of its own, and its reach held on every door
/// before its first turn is sent.
pub fn hand_off(host: &Host, caller: &Caller, catalog: &Catalog, role: &str, task: &str, context: &str) -> Result<Handed, String> {
    if role.trim().is_empty() {
        return Err(format!("`role` is empty: a role from the catalog — {}.", catalog.listing()));
    }
    let Some(role) = catalog.find(role) else {
        return Err(format!(
            "There is no role `{}` in the catalog; it has {}. `describe shell` lists them under `catalog`.",
            role.trim(),
            catalog.listing()
        ));
    };
    if task.trim().is_empty() {
        return Err(format!("`task` is empty: what the {} is to do.", role.name));
    }
    if context.len() > CONTEXT_MOST_BYTES {
        return Err(format!(
            "`context` is {} KiB; at most {} KiB goes into a first turn. Put the rest in a file and \
             name it in the task.",
            context.len() / 1024,
            CONTEXT_MOST_BYTES / 1024
        ));
    }
    let parent = match caller {
        Caller::NoAgent => None,
        Caller::Agent(me) => Some(me),
    };
    if let Some(parent) = parent {
        let live: Vec<AgentId> = host.agents().into_iter().map(|a| a.id).collect();
        agents::store().read(|s| may_start_child(parent, s, &live))?;
        // A reach caps the agents it hands work to as well: never above its own ceiling.
        if let Some(mine) = reaches::of(parent) {
            if gate::grade(&role.reach.ceiling) > gate::grade(&mine.ceiling) {
                return Err(format!(
                    "`{parent}` is the {}, at most `{}`, and cannot hand work to the {}, whose reach \
                     goes up to `{}`: a role never hands work above its own ceiling.",
                    mine.name, mine.ceiling, role.name, role.reach.ceiling
                ));
            }
        }
    }
    let mind = role.pick_mind(&catalog::minds_now(host))?;
    let hold = |agent: &AgentId| reaches::hold(host, agent, role);
    let how = launch::Start { title: Some(task.trim()), role: Some(role.meta()), before_first_turn: Some(&hold) };
    let agent = launch::start_with(host, &mind, &role.first_turn(task, context), parent, how)?;
    watch_budget(host.clone(), agent.clone(), role.name.clone(), role.budget.minutes);
    tracing::info!(agent = %agent, role = %role.id, mind = %mind, parent = ?parent.map(|p| p.to_string()), "work was handed to a catalog role");
    Ok(Handed { agent, role: role.clone(), mind })
}

/// The Agents screen's New agent → from the catalog: the person hands `task` to `role`.
pub fn hand_off_from_screen(role: &str, task: &str) -> Result<AgentId, String> {
    hand_off(host()?, &Caller::NoAgent, &Catalog::load(), role, task, "").map(|handed| handed.agent)
}

/// A role's budget in minutes, held: when it runs out the agent is stopped — its conversation let
/// go, its place under the desktop's cap freed — and its pane says why. Ends early once it is
/// stopped some other way.
fn watch_budget(host: Host, agent: AgentId, name: String, minutes: u32) {
    let budget = Duration::from_secs(u64::from(minutes) * 60);
    let spawned = std::thread::Builder::new().name("agent-budget".into()).spawn(move || {
        let started = Instant::now();
        let live = |host: &Host| host.agents().iter().any(|a| a.id == agent);
        loop {
            let left = budget.saturating_sub(started.elapsed());
            if left.is_zero() {
                break;
            }
            std::thread::sleep(left.min(Duration::from_secs(5)));
            if !live(&host) {
                return;
            }
        }
        if live(&host) {
            let _ = launch::stop_on(&host, &agent);
            agents::store().note(&agent, &format!("Its budget of {minutes} minutes as the {name} ran out, so it was stopped."));
            tracing::info!(agent = %agent, minutes, "a role's budget ran out; it was stopped");
        }
    });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "could not watch a role's budget");
    }
}

/// Wait up to `wait` for a role's first turn to end, and read what it said.
pub fn wait_for_answer(agent: &AgentId, wait: Duration) -> Answered {
    let deadline = Instant::now() + wait;
    loop {
        let seen = agents::store().read(|s| {
            let first = s.agent(agent)?.turns.iter().find(|t| !t.prompt.is_empty())?;
            first.ended.map(|_| (first.ok == Some(true), turn_text(first)))
        });
        if let Some((ok, text)) = seen {
            return Answered { done: true, ok, text };
        }
        if Instant::now() >= deadline {
            return Answered { done: false, ok: false, text: String::new() };
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// What a turn said, in words: its text, and — for a turn that did not end well — the shell's
/// notes on why. Cut at [`ANSWER_MOST_BYTES`], saying so.
fn turn_text(turn: &Turn) -> String {
    let mut text = String::new();
    for item in &turn.items {
        match item {
            Item::Text(t) => text.push_str(&t.text()),
            Item::Note(note) if turn.ok != Some(true) => text.push_str(&format!("\n[{note}]\n")),
            _ => {}
        }
    }
    let text = text.trim();
    if text.len() <= ANSWER_MOST_BYTES {
        return text.to_string();
    }
    let mut cut = ANSWER_MOST_BYTES;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}\n… {} more bytes; `read_agent` has the rest.", &text[..cut], text.len() - cut)
}

impl Handed {
    /// What `hand_off` answers: the agent, the role, where it runs and what it may touch — and,
    /// after a wait, what it said or that it is still working.
    pub fn answer(&self, waited: Option<(Duration, Answered)>) -> Value {
        let role = &self.role;
        let who = format!("the {} (`{}`, on {})", role.name, self.agent, self.mind);
        let mut out = json!({
            "agent": self.agent,
            "role": role.id,
            "role_name": role.name,
            "mind": self.mind,
            "reach": role.reach.text(),
            "budget": { "turns": role.budget.turns, "minutes": role.budget.minutes },
        });
        let follow = format!(
            "`read_agent` with agent `{agent}` shows how it is going; `send_to_agent` says more; \
             `stop_agent` stops it.",
            agent = self.agent
        );
        let said = match waited {
            None => format!(
                "Handed to {who}. It works on its own, in its own pane, within its reach ({reach}), \
                 for up to {turns} turns and {minutes} minutes, and it has none of your grants. {follow}",
                reach = role.reach.text(),
                turns = role.budget.turns,
                minutes = role.budget.minutes,
            ),
            Some((_, answered)) if answered.done => {
                out["done"] = true.into();
                out["ok"] = answered.ok.into();
                out["answer"] = answered.text.clone().into();
                let how = if answered.ok { "answered" } else { "could not finish; what it said" };
                let text = if answered.text.is_empty() { "(it said nothing)".to_string() } else { answered.text };
                format!("{} {how}:\n\n{text}", capitalised(&who))
            }
            Some((wait, _)) => {
                out["done"] = false.into();
                format!(
                    "Handed to {who}; it is still working after {} s. {follow}",
                    wait.as_secs()
                )
            }
        };
        out["said"] = said.into();
        out
    }
}

/// Whether `agent` may be shown asking for `app.action`, graded `grade`: an agent held to a
/// role's reach is refused, in the reach's words, before a card for an act its reach refuses
/// would reach the person — the act itself would be refused on its door whatever they pressed.
pub fn within_reach(agent: &AgentId, app: &str, action: &str, grade: &str) -> Result<(), String> {
    match reaches::of(agent) {
        Some(reach) => yantrik_ipc_transport::reach::within(&reach, app, action, grade),
        None => Ok(()),
    }
}

fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{AgentMeta, Store};
    use yantrik_harness::protocol;

    fn id(s: &str) -> AgentId {
        AgentId(s.to_string())
    }

    /// Nothing that shows or keeps arguments — `describe shell`, a card, an audit line — is handed a
    /// token by these actions: none takes one, and none takes the caller's own agent either. The
    /// one `agent` argument some of them take is the agent acted ON.
    #[test]
    fn no_agents_action_takes_the_token_or_the_callers_own_agent() {
        for spec in specs() {
            let schema = spec.schema();
            let params = schema["parameters"]["properties"].as_object().unwrap();
            for banned in ["agent_token", "token", "parent", "caller", "grant"] {
                assert!(!params.contains_key(banned), "{} takes `{banned}`: {schema}", spec.name);
            }
        }
        let grade = |name: &str| specs().into_iter().find(|s| s.name == name).unwrap().permission;
        assert_eq!(
            [grade("new_agent"), grade("send_to_agent"), grade("stop_agent"), grade("read_agent"), grade("show_agent"), grade("hand_off")],
            ["sensitive", "standard", "standard", "safe", "safe", "sensitive"],
            "the grades the design settled: hand_off is gated like new_agent"
        );
    }

    // ── Handing work to a role ──

    fn attach(host: &Host, id: &str, conversations: bool) -> String {
        let attach = json!({ "id": id, "name": id, "conversations": conversations });
        host.handle_from(protocol::ATTACH, &attach, Some(std::process::id())).unwrap()["session"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn shipped() -> Catalog {
        Catalog::from_layers(&catalog::SHIPPED, &[])
    }

    /// Every turn waiting for a harness, by conversation.
    fn handed_out(host: &Host, session: &str) -> std::collections::HashMap<String, Value> {
        let mut out = std::collections::HashMap::new();
        for _ in 0..16 {
            let turn = host.handle(protocol::POLL, &json!({ "session": session })).unwrap();
            let Some(conversation) = turn["conversation"].as_str() else { break };
            out.insert(conversation.to_string(), turn.clone());
        }
        out
    }

    fn settled(agent: &AgentId) -> bool {
        (0..100).any(|_| {
            let done = agents::store().read(|s| s.agent(agent).is_some_and(|a| a.open_turn().is_none()));
            if !done {
                std::thread::sleep(Duration::from_millis(50));
            }
            done
        })
    }

    /// The whole of hand_off against a real host: the role's preferred attached mind, a first turn
    /// of brief, task and context with a token of its own, a row named for the task with its role
    /// — and the role's reach held before that first turn, on the shell's registry and in the file
    /// a door in another process reads, which never holds the token. Then in reach, off its
    /// surfaces and above its ceiling, as every door decides them; and a stop lets it go.
    #[test]
    fn hand_off_starts_the_role_on_its_first_attached_mind_held_to_its_reach() {
        let host = Host::new(vec![]);
        let pi = attach(&host, "pi", true);
        let deepseek = attach(&host, "deepseek", true);
        let handed = hand_off(&host, &Caller::NoAgent, &shipped(), "Reviewer", "review the change in ~/src/app", "diff --git a/x b/x").unwrap();
        assert_eq!((handed.mind.as_str(), handed.agent.harness()), ("deepseek", "deepseek"), "the Reviewer runs on deepseek first");

        let first = host.handle(protocol::POLL, &json!({ "session": deepseek })).unwrap();
        let text = first["text"].as_str().unwrap();
        for says in ["You are the Reviewer", "Find what is wrong with a change", "The task:\nreview the change in ~/src/app", "Read this first:\ndiff --git a/x b/x"] {
            assert!(text.contains(says), "{says:?} missing:\n{text}");
        }
        let token = first["agent_token"].as_str().unwrap().to_string();
        assert!(host.handle(protocol::POLL, &json!({ "session": pi })).unwrap()["turn_id"].is_null(), "nothing went to pi");

        let (title, role) = agents::store().read(|s| s.agent(&handed.agent).map(|a| (a.meta.title.clone(), a.meta.role.clone()))).unwrap();
        assert_eq!(title, "review the change in ~/src/app", "its row is named for the task, not the brief");
        assert_eq!(role.map(|r| (r.name, r.reach)), Some(("Reviewer".to_string(), "editor, documents and notes · at most safe".to_string())));

        let held = reaches::lookup(&token).expect("the shell's own dispatch holds it");
        assert_eq!(held.agent, handed.agent.0);
        assert_eq!(reaches::read_as_a_door(&token).unwrap(), Some(held.clone()), "and so does every other door");
        assert!(!std::fs::read_to_string(reaches::path()).unwrap().contains(&token), "the file keeps a digest, never the token");

        use yantrik_ipc_transport::reach::within;
        assert!(within(&held, "notes", "list_notes", "safe").is_ok(), "in reach");
        let err = within(&held, "files", "move", "safe").unwrap_err();
        assert!(err.starts_with("REACH: files.move is outside the Reviewer's reach") && err.contains(&handed.agent.0), "{err}");
        let err = within(&held, "notes", "new_note", "standard").unwrap_err();
        assert!(err.contains("above the Reviewer's `safe` ceiling"), "{err}");
        let err = within(&held, "shell", "agent_run", "sensitive").unwrap_err();
        assert!(err.starts_with("REACH: shell.agent_run is outside"), "a reviewer runs no commands: {err}");
        // Asking the person about an act its reach refuses is refused too, in the same words.
        let err = within_reach(&handed.agent, "files", "move", "sensitive").unwrap_err();
        assert!(err.starts_with("REACH:"), "{err}");
        assert!(within_reach(&AgentId("pi:c-noreach".into()), "files", "move", "sensitive").is_ok(), "an agent with no role has no reach");

        let answer = handed.answer(None);
        let said = answer["said"].as_str().unwrap();
        assert!(said.starts_with(&format!("Handed to the Reviewer (`{}`, on deepseek)", handed.agent)), "{said}");
        assert!(said.contains("editor, documents and notes · at most safe") && said.contains("4 turns and 15 minutes"), "{said}");
        assert!(!answer.to_string().contains(&token), "{answer}");

        stop_agent(&host, &Caller::NoAgent, &handed.agent).unwrap();
        assert_eq!(reaches::lookup(&token), None, "stopped, it is let go");
        assert_eq!(reaches::read_as_a_door(&token).unwrap(), None);
    }

    /// Down its list to the first mind attached that can give it a conversation of its own; a
    /// one-conversation mind is never used, and none at all is said plainly with nothing started.
    #[test]
    fn a_role_falls_back_down_its_list_and_is_refused_plainly_when_none_of_its_minds_is_attached() {
        let host = Host::new(vec![]);
        attach(&host, "hermes", false);
        let err = hand_off(&host, &Caller::NoAgent, &shipped(), "reviewer", "review it", "").unwrap_err();
        assert!(err.starts_with("No mind the Reviewer runs on is attached: it runs on deepseek, pi, openclaw"), "{err}");
        assert!(err.contains("hermes holds one conversation at a time — the person's own"), "{err}");
        assert!(host.agents().is_empty(), "nothing was started, and the person's own conversation with hermes is untouched");

        attach(&host, "pi", true);
        let handed = hand_off(&host, &Caller::NoAgent, &shipped(), "reviewer", "review it", "").unwrap();
        assert_eq!(handed.mind, "pi", "past deepseek, which is not attached, to pi");

        let err = hand_off(&host, &Caller::NoAgent, &shipped(), "janitor", "x", "").unwrap_err();
        assert!(err.contains("no role `janitor`") && err.contains("reviewer (Reviewer)"), "{err}");
        assert!(hand_off(&host, &Caller::NoAgent, &shipped(), "", "x", "").unwrap_err().contains("`role` is empty"));
        assert!(hand_off(&host, &Caller::NoAgent, &shipped(), "reviewer", "  ", "").unwrap_err().contains("`task` is empty"));
        let err = hand_off(&host, &Caller::NoAgent, &shipped(), "reviewer", "x", &"a".repeat(40 * 1024)).unwrap_err();
        assert!(err.contains("at most 32 KiB"), "{err}");
        stop_agent(&host, &Caller::NoAgent, &handed.agent).unwrap();
    }

    /// new_agent's rules, word for word — depth one, three children, nothing of the parent's — and
    /// the reach capping further: an agent held to a reach cannot hand work above its own ceiling,
    /// nor start a plain agent that would have no reach at all.
    #[test]
    fn hand_off_meets_new_agents_rules_and_a_reach_caps_what_it_hands_on() {
        let host = Host::new(vec![]);
        let session = attach(&host, "pi", true);
        let started = new_agent(&host, &Caller::NoAgent, "pi", "plan the release").unwrap();
        let parent = AgentId(started["agent"].as_str().unwrap().to_string());
        let parent_token = handed_out(&host, &session)[parent.conversation()]["agent_token"].as_str().unwrap().to_string();
        let as_parent = Caller::Agent(parent.clone());

        let kids: Vec<Handed> = ["researcher", "writer", "scribe"]
            .iter()
            .map(|role| hand_off(&host, &as_parent, &shipped(), role, &format!("{role}'s part"), "").unwrap())
            .collect();
        let err = hand_off(&host, &as_parent, &shipped(), "planner", "a fourth", "").unwrap_err();
        assert!(err.contains("already has 3 agents running"), "{err}");
        let err = hand_off(&host, &Caller::Agent(kids[0].agent.clone()), &shipped(), "chair", "weigh them", "").unwrap_err();
        assert!(err.contains("one level only"), "{err}");

        let turns = handed_out(&host, &session);
        for kid in &kids {
            let turn = &turns[kid.agent.conversation()];
            assert!(!turn.to_string().contains(&parent_token), "nothing of the parent's token");
            assert_ne!(turn["agent_token"].as_str().unwrap(), parent_token, "a token of its own");
            let parent_of = agents::store().read(|s| s.agent(&kid.agent).and_then(|a| a.meta.parent.clone()));
            assert_eq!(parent_of, Some(parent.clone()), "its row says who handed it the work");
        }
        let stopped = stop_agent(&host, &Caller::NoAgent, &parent).unwrap();
        assert_eq!(stopped["children_stopped"].as_array().map(Vec::len), Some(3), "{stopped}");

        // A Reviewer (safe) the person started: it may hand work to the Red team (safe), not to the
        // Coder (sensitive), and it may not start a plain agent.
        let reviewer = hand_off(&host, &Caller::NoAgent, &shipped(), "reviewer", "review it", "").unwrap();
        let as_reviewer = Caller::Agent(reviewer.agent.clone());
        let err = hand_off(&host, &as_reviewer, &shipped(), "coder", "fix it", "").unwrap_err();
        assert!(err.contains("is the Reviewer, at most `safe`, and cannot hand work to the Coder"), "{err}");
        let err = new_agent(&host, &as_reviewer, "pi", "do anything").unwrap_err();
        assert!(err.contains("works within a reach"), "{err}");
        let red = hand_off(&host, &as_reviewer, &shipped(), "red-team", "attack it", "").unwrap();
        assert_eq!(red.role.id, "red-team");
        stop_agent(&host, &Caller::NoAgent, &reviewer.agent).unwrap();
    }

    /// With a wait, hand_off hands back what the role said once its first turn ends; a wait that
    /// runs out says it is still working. And a role's turns are its budget.
    #[test]
    fn hand_off_with_a_wait_hands_back_the_roles_answer_and_its_turns_are_a_budget() {
        let host = Host::new(vec![]);
        let session = attach(&host, "pi", true);
        let handed = hand_off(&host, &Caller::NoAgent, &shipped(), "chair", "weigh the three answers", "A says ship; B says wait").unwrap();
        let harness = {
            let (host, session) = (host.clone(), session.clone());
            std::thread::spawn(move || {
                let turn = host.handle(protocol::POLL, &json!({ "session": session })).unwrap();
                let id = turn["turn_id"].clone();
                host.handle(protocol::CHUNK, &json!({ "session": session, "turn_id": id, "delta": "Verdict — ship on Friday." })).unwrap();
                host.handle(protocol::COMPLETE, &json!({ "session": session, "turn_id": id })).unwrap();
            })
        };
        let answered = wait_for_answer(&handed.agent, Duration::from_secs(10));
        harness.join().unwrap();
        assert_eq!(answered, Answered { done: true, ok: true, text: "Verdict — ship on Friday.".into() });
        let answer = handed.answer(Some((Duration::from_secs(10), answered)));
        assert_eq!((answer["done"].clone(), answer["answer"].clone()), (json!(true), json!("Verdict — ship on Friday.")));
        let said = answer["said"].as_str().unwrap();
        assert!(said.starts_with(&format!("The Chair (`{}`, on pi) answered:\n\nVerdict", handed.agent)), "{said}");

        // The Chair has two turns: a second is sent, a third is refused.
        assert!(settled(&handed.agent));
        send_to_agent(&host, &Caller::NoAgent, &handed.agent, "and C says never").unwrap();
        let turn = host.handle(protocol::POLL, &json!({ "session": session })).unwrap();
        host.handle(protocol::COMPLETE, &json!({ "session": session, "turn_id": turn["turn_id"] })).unwrap();
        assert!(settled(&handed.agent));
        let err = send_to_agent(&host, &Caller::NoAgent, &handed.agent, "one more").unwrap_err();
        assert!(err.contains("is the Chair, whose budget is 2 turns, and it has had them all"), "{err}");

        // A wait that runs out.
        let other = hand_off(&host, &Caller::NoAgent, &shipped(), "scribe", "summarise it", "").unwrap();
        let answered = wait_for_answer(&other.agent, Duration::from_millis(300));
        assert!(!answered.done);
        let said = other.answer(Some((Duration::from_millis(300), answered)))["said"].as_str().unwrap().to_string();
        assert!(said.contains("it is still working after 0 s") && said.contains("`read_agent` with agent"), "{said}");

        assert_eq!(wait_arg(&json!({})).unwrap(), None);
        assert_eq!(wait_arg(&json!({ "wait_seconds": 0 })).unwrap(), None, "zero is not waiting");
        assert_eq!(wait_arg(&json!({ "wait_seconds": 90 })).unwrap(), Some(Duration::from_secs(90)));
        assert!(wait_arg(&json!({ "wait_seconds": 601 })).is_err());
        assert!(wait_arg(&json!({ "wait_seconds": "soon" })).is_err());
        for agent in [&handed.agent, &other.agent] {
            stop_agent(&host, &Caller::NoAgent, agent).unwrap();
        }
    }

    /// Depth one, three children, and the host's own cap — checked without a host.
    #[test]
    fn a_child_cannot_start_agents_and_a_parent_holds_at_most_three() {
        let mut s = Store::new();
        let parent = id("pi:c-par001");
        s.upsert_agent(AgentMeta::new(parent.clone(), "pi"));
        let mut live = vec![parent.clone()];
        for n in 0..3 {
            let child = id(&format!("pi:c-kid00{n}"));
            let mut meta = AgentMeta::new(child.clone(), "pi");
            meta.parent = Some(parent.clone());
            s.upsert_agent(meta);
            live.push(child);
        }
        let err = may_start_child(&parent, &s, &live).unwrap_err();
        assert!(err.contains("already has 3 agents running"), "{err}");
        // One of them stopped: its place is free again.
        live.retain(|a| a.0 != "pi:c-kid000");
        assert!(may_start_child(&parent, &s, &live).is_ok());
        // A child asking for a child of its own.
        let err = may_start_child(&id("pi:c-kid001"), &s, &live).unwrap_err();
        assert!(err.contains("one level only") && err.contains("pi:c-par001"), "{err}");
    }

    #[test]
    fn an_agent_directs_only_the_agents_it_started_and_the_person_directs_any() {
        let mut s = Store::new();
        let mut meta = AgentMeta::new(id("pi:c-kid001"), "pi");
        meta.parent = Some(id("pi:c-par001"));
        s.upsert_agent(meta);
        s.upsert_agent(AgentMeta::new(id("deepseek:c-other1"), "deepseek"));
        let parent = Caller::Agent(id("pi:c-par001"));
        assert!(may_direct(&parent, &id("pi:c-kid001"), &s, "stop").is_ok());
        let err = may_direct(&parent, &id("deepseek:c-other1"), &s, "send to").unwrap_err();
        assert!(err.contains("not an agent you started"), "{err}");
        assert!(may_direct(&Caller::Agent(id("pi:c-kid001")), &id("pi:c-par001"), &s, "stop").is_err(), "a child does not direct its parent");
        assert!(may_direct(&Caller::NoAgent, &id("deepseek:c-other1"), &s, "stop").is_ok(), "the person directs any");
        // Reading: itself and its children, never a stranger.
        assert!(may_read(&parent, &id("pi:c-par001"), &s).is_ok());
        assert!(may_read(&parent, &id("pi:c-kid001"), &s).is_ok());
        assert!(may_read(&parent, &id("deepseek:c-other1"), &s).is_err());
    }

    /// The whole of `new_agent` against a real host: depth, children, the global cap, and the
    /// child's first turn — which carries its task and nothing of its parent's.
    #[test]
    fn new_agent_starts_a_child_with_nothing_of_its_parents_and_meets_every_cap() {
        let host = Host::new(vec![]);
        let attach = json!({ "id": "glue", "name": "Glue", "conversations": true });
        let session = host.handle_from(protocol::ATTACH, &attach, Some(std::process::id())).unwrap()["session"]
            .as_str()
            .unwrap()
            .to_string();
        let poll = || host.handle(protocol::POLL, &json!({ "session": session })).unwrap();
        let finish = |handed: &Value| {
            host.handle(protocol::COMPLETE, &json!({ "session": session, "turn_id": handed["turn_id"] })).unwrap();
        };

        // The parent: an agent of its own, holding a token, with a note waiting for its next turn
        // — the kind of thing a child must not be handed.
        let parent = host.start_agent("glue").unwrap();
        let _answer = host.send_to(&parent, yantrik_harness::Turn::new("plan the release")).unwrap();
        let handed = poll();
        let parent_token = handed["agent_token"].as_str().unwrap().to_string();
        finish(&handed);
        assert!(host.note_for(&parent, "your command `make` finished: exit code 0".into()));
        agents::store().upsert_agent(AgentMeta::new(parent.clone(), "Glue"));

        let caller = Caller::Agent(parent.clone());
        let started = new_agent(&host, &caller, "glue", "write the changelog").unwrap();
        let child = AgentId(started["agent"].as_str().unwrap().to_string());
        assert_eq!(started["parent"], json!(parent.0), "{started}");
        assert!(started["said"].as_str().unwrap().contains("none of your grants"), "{started}");
        let recorded = agents::store().read(|s| s.agent(&child).map(|a| (a.meta.parent.clone(), a.meta.title.clone())));
        assert_eq!(recorded, Some((Some(parent.clone()), "write the changelog".to_string())), "its row says who started it");

        // What the child's harness is handed: its task, the desktop's context and its own token.
        let first = poll();
        assert_eq!(first["conversation"], json!(child.conversation()));
        assert_eq!(first["text"], "write the changelog");
        let child_token = first["agent_token"].as_str().unwrap();
        assert_ne!(child_token, parent_token, "a token of its own");
        let whole = first.to_string();
        assert!(!whole.contains(&parent_token), "nothing of the parent's token");
        assert!(!whole.contains("finished: exit code"), "nor the parent's notes");
        assert!(first["context"].as_str().map_or(true, |c| !c.contains("notes")), "{first}");
        finish(&first);

        // The name a person reads works too; a mind that is not attached is said plainly.
        assert!(new_agent(&host, &caller, "Glue", "second").is_ok());
        let err = new_agent(&host, &caller, "hermes", "x").unwrap_err();
        assert!(err.contains("no mind called `hermes`") && err.contains("glue"), "{err}");
        // A third, then a fourth refused.
        assert!(new_agent(&host, &caller, "glue", "third").is_ok());
        let err = new_agent(&host, &caller, "glue", "fourth").unwrap_err();
        assert!(err.contains("already has 3 agents running"), "{err}");
        // A child cannot start one.
        let err = new_agent(&host, &Caller::Agent(child.clone()), "glue", "grandchild").unwrap_err();
        assert!(err.contains("one level only"), "{err}");

        // The host's own cap: parent + 3 children live, then the person starts two more.
        assert!(new_agent(&host, &Caller::NoAgent, "glue", "fifth").is_ok());
        assert!(new_agent(&host, &Caller::NoAgent, "glue", "sixth").is_ok());
        let err = new_agent(&host, &Caller::NoAgent, "glue", "seventh").unwrap_err();
        assert!(err.contains(&format!("the most is {}", protocol::MAX_LIVE_AGENTS)), "{err}");

        // Stop on the parent stops its children, and frees their places.
        let stopped = stop_agent(&host, &Caller::NoAgent, &parent).unwrap();
        assert_eq!(stopped["children_stopped"].as_array().map(Vec::len), Some(3), "{stopped}");
        let live: Vec<AgentId> = host.agents().into_iter().map(|a| a.id).collect();
        assert!(!live.contains(&parent) && !live.contains(&child), "{live:?}");

        // Nothing anyone was answered carried either token.
        for answer in [started.to_string(), stopped.to_string()] {
            assert!(!answer.contains(&parent_token) && !answer.contains(child_token), "{answer}");
        }
    }

    /// `read_agent` answers with the session as text — a verified card with its exit code, what
    /// the person was asked — and refuses a stranger's session to an agent.
    #[test]
    fn read_agent_gives_the_session_as_text_and_only_to_those_who_may_read_it() {
        let me = id("pi:c-read01");
        let store = agents::store();
        store.upsert_agent(AgentMeta::new(me.clone(), "pi"));
        store.open_turn(&me, "count the photos");
        store.text(&me, "Counting them now.");
        store.command_started(&me, "job-read01", "ls ~/Pictures | wc -l", "/home/me");
        store.command_output(&me, "job-read01", b"4127\r\n");
        store.command_finished(&me, "job-read01", "ls ~/Pictures | wc -l", Some(0), false);
        store.approval_asked(&me, "appr-read01", "files.move");
        store.close_turn(&me, true);

        let read = read_agent(&Caller::Agent(me.clone()), &me, None).unwrap();
        let text = read["text"].as_str().unwrap();
        for said in ["count the photos", "Counting them now.", "[verified call]", "exit 0", "4127", "[asked the person] files.move"] {
            assert!(text.contains(said), "{said:?} missing:\n{text}");
        }
        assert_eq!(read["needs_you"], true, "{read}");
        let err = read_agent(&Caller::Agent(id("deepseek:c-other2")), &me, None).unwrap_err();
        assert!(err.contains("not an agent you started"), "{err}");
        assert!(read_agent(&Caller::NoAgent, &me, Some(&json!(21))).unwrap_err().contains("between 1 and 20"));
        assert!(read_agent(&Caller::NoAgent, &id("pi:c-nobody"), None).unwrap_err().contains("no agent"));
    }

    #[test]
    fn an_agent_is_named_the_way_describe_lists_it() {
        assert_eq!(agent_arg(&json!({"agent": " pi:c-7f3a91 "})).unwrap(), id("pi:c-7f3a91"));
        for bad in ["pi", ":c-1", "pi:", ""] {
            assert!(agent_arg(&json!({"agent": bad})).is_err(), "{bad}");
        }
    }
}
