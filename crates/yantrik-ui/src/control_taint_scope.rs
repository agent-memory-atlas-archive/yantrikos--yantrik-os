//! `taint_scope` and `taint_mark`: which of the person's conversations a bridge's private reads
//! count against, for `yos-mcp` (see `taint_scope`).
//!
//! Both are `safe` and neither takes anything that names a conversation: the caller's
//! conversation is what the desktop can establish about the caller, never what the caller says.
//!
//! - A believed agent token for `<harness>:main` — the person's chat with that mind — follows that
//!   harness's scope. Any other agent (a sub-agent, a role, a recipe) answers `follows: false`:
//!   the person's + does not start that mind's conversation again, so its bridge keeps the scope
//!   it always had, its process. A token that is not believed is refused.
//! - No token (Hermes and OpenClaw start their bridge with none): the attached mind the caller's
//!   process tree resolves to, by the same walk `mind_view::requester_now` makes. No mind, or more
//!   than one by that name, is refused.
//!
//! A refusal is what the bridge reads as "the same scope": nothing it holds is cleared.
//!
//! `taint_mark` only adds. Nothing on this surface begins a scope or takes a read away: that is
//! the person's + in the Lens (`wire::chat`), and a test in `taint_scope` reads the source to keep
//! it so.

use yantrik_app_runtime::control::{Action, App as ControlSurface, Param};

use crate::agents::AgentId;
use crate::taint_scope::{self, Scope};

pub fn actions(surface: ControlSurface) -> ControlSurface {
    surface
        .action(
            Action::new(
                "taint_scope",
                "Which of the person's conversations the caller is in, for the taint rule: its id, whether the person began it with + in the Lens, and the private reads made in it. Only the person's + begins one",
            )
            .risk("safe"),
            move |_args| {
                let Some(harness) = callers_conversation()? else { return Ok(not_followed()) };
                Ok(answer(&harness, &taint_scope::current(&harness)))
            },
        )
        .action(
            Action::new(
                "taint_mark",
                "Record that the caller's conversation has read private state. It only ever adds; nothing here clears a conversation's reads",
            )
            .risk("safe")
            .arg(Param::text("read").describe("What was read, by the tool's name: os_describe, shell.read_screen")),
            move |args| {
                let Some(harness) = callers_conversation()? else { return Ok(not_followed()) };
                let read = args["read"].as_str().unwrap_or_default();
                Ok(answer(&harness, &taint_scope::mark(&harness, read)))
            },
        )
}

fn answer(harness: &str, scope: &Scope) -> serde_json::Value {
    serde_json::json!({
        "follows": true,
        "harness": harness,
        "scope": scope.id,
        "begun": scope.begun.as_str(),
        "read": scope.read,
    })
}

fn not_followed() -> serde_json::Value {
    serde_json::json!({
        "follows": false,
        "said": "This agent's conversation is its own, not one the person starts again with + in the Lens, so its taint lasts as long as its session.",
    })
}

/// The harness whose Lens conversation the caller is in; `None` for an agent whose conversation
/// the person's + does not start again; an error when the desktop cannot tell.
fn callers_conversation() -> Result<Option<String>, String> {
    match crate::control_agent_terminal::calling_agent() {
        Some(Ok(agent)) => Ok(main_harness(&agent)),
        Some(Err(why)) => Err(format!("the taint scope cannot be read for this caller: {why}")),
        None => {
            let pid = yantrik_app_runtime::control::caller()
                .and_then(|c| u32::try_from(c.pid).ok())
                .filter(|p| *p > 0 && *p != std::process::id())
                .ok_or("the taint scope cannot be read: the desktop cannot tell which process is asking")?;
            let who = crate::caller_identity::resolve(pid as i32);
            let name = who
                .attached_mind
                .ok_or("the taint scope cannot be read: the caller is not one of the minds attached here")?;
            harness_named(&name, &crate::caller_identity::attached_minds()).map(Some)
        }
    }
}

/// `<harness>:main` follows the harness's scope; every other agent keeps its own.
fn main_harness(agent: &AgentId) -> Option<String> {
    (agent.conversation() == AgentId::MAIN).then(|| agent.harness().to_string())
}

/// The one attached mind called `name`, by its harness id.
fn harness_named(name: &str, minds: &[crate::caller_identity::Mind]) -> Result<String, String> {
    let ids: Vec<&str> = minds.iter().filter(|m| m.name == name).map(|m| m.id.as_str()).collect();
    match ids.as_slice() {
        [one] => Ok((*one).to_string()),
        [] => Err(format!("the taint scope cannot be read: {name} is not attached here")),
        _ => Err(format!("the taint scope cannot be read: more than one attached mind is called {name}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caller_identity::Mind;

    fn mind(id: &str, name: &str) -> Mind {
        Mind { id: id.into(), name: name.into(), pid: None }
    }

    /// The person's chat with a mind follows the Lens; an agent the person's + does not start
    /// again keeps its own.
    #[test]
    fn only_a_main_conversation_follows_the_lens() {
        assert_eq!(main_harness(&AgentId::new("pi", AgentId::MAIN)), Some("pi".to_string()));
        assert_eq!(main_harness(&AgentId::new("pi", "c-7f3a91")), None);
        assert_eq!(main_harness(&AgentId::new("deepseek", "c-role1")), None);
    }

    /// A caller with no token is placed by the mind it resolves to, and only when that is one.
    #[test]
    fn a_caller_with_no_token_is_placed_by_its_mind_or_not_at_all() {
        let minds = [mind("builtin", "Yantrik Mind"), mind("hermes", "Hermes Agent"), mind("pi", "Pi")];
        assert_eq!(harness_named("Hermes Agent", &minds), Ok("hermes".to_string()));
        assert!(harness_named("OpenClaw", &minds).is_err());
        let twice = [mind("hermes", "Hermes Agent"), mind("hermes-2", "Hermes Agent")];
        assert!(harness_named("Hermes Agent", &twice).is_err(), "an ambiguous name is not a conversation");
    }

    /// Neither action takes anything that names a conversation, and neither is graded above `safe`.
    #[test]
    fn neither_action_takes_a_conversation_from_the_caller() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/control_taint_scope.rs");
        let whole = std::fs::read_to_string(path).unwrap();
        let code = whole.split("#[cfg(test)]").next().unwrap();
        assert_eq!(code.matches("Param::").count(), 1, "taint_mark's `read` is the only argument");
        for name in ["\"harness\")", "\"scope\")", "\"agent\")", "\"conversation\")"] {
            assert!(!code.contains(&format!("Param::text({name}")), "no action takes {name}");
        }
        assert_eq!(code.matches(".risk(\"safe\")").count(), 2);
        assert!(!code.contains("begin_by_person"), "nothing on the surface begins a scope");
    }
}
