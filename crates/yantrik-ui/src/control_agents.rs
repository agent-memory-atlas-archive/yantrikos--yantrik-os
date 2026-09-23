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

use serde_json::{json, Value};
use slint::ComponentHandle;
use yantrik_app_runtime::control::{Action, App as ControlSurface, Param};
use yantrik_harness::Host;

use crate::agents::{self, launch, AgentId, Store};
use crate::App;

/// How many live agents one agent may have started (design decision 1).
pub const MAX_CHILDREN: usize = 3;

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

/// The five actions as published.
fn specs() -> [Action; 5] {
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
    ]
}

/// The five actions, for the shell's surface.
pub fn actions(surface: ControlSurface, ui: &App) -> ControlSurface {
    let [new, send, stop, read, show] = specs();
    let show_ui = ui.as_weak();
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
            [grade("new_agent"), grade("send_to_agent"), grade("stop_agent"), grade("read_agent"), grade("show_agent")],
            ["sensitive", "standard", "standard", "safe", "safe"],
            "the grades the design settled"
        );
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
