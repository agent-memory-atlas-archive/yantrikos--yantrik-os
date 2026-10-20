//! Intents: the commands the Mind may raise by name, each with its wire contract
//! (design/app-framework-2026-10-21.md, §3.1). Every intent is a command; not every command is
//! an intent.
//!
//! The surface's existing dispatch stays the admission authority and runs first: the state rule,
//! the argument checks against the declared parameters, the ceiling and the mode, approvals and
//! grants, the revision guard. Then, on the UI thread, the intent's handler runs: the app's own
//! state rule, the agent rule, `parse` into a typed command, the command through `update`, and
//! the reply from the committed model.
//!
//! How the Mind is answered is the intent's [`Settle`]: on return, deferred, or explicitly, when
//! the invocation reaches its one terminal outcome. For the last, the handler leaves the rest
//! of its answer with `answer_later`, and the core settles it when the app calls `fx.settle`,
//! when the invocation's last job lands, panics, is replaced or is cancelled, or when the app
//! shuts down, whichever comes first, within the deadline.

use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use serde_json::Value;
use yantrik_surface::{answer_later, LocalRegistry};

use crate::app::App;
use crate::control::{agent_is_calling, Action, View};
use crate::cx::Core;
use crate::refused::Refused;

/// One accepted call of an intent, by number. Jobs and `then` chains started under it carry it,
/// so the core knows which invocation a late result or a panic belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InvocationId(pub u64);

/// Where a command came from. Carried with it through jobs and continuations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// A button, a key, a timer: the person at this window.
    Local,
    /// The control surface: the person through `yos`, or an agent.
    Surface { agent: bool },
}

/// A command being applied under an invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Invocation {
    pub id: InvocationId,
    pub origin: Origin,
}

/// The arguments of one call, as the dispatch checked and filled them against the declared
/// parameters: present, of the declared type, defaults in place. These read them typed; they fail
/// only for what the declaration could not say, an empty required text.
pub struct Args<'a>(&'a Value);

impl<'a> Args<'a> {
    pub fn new(value: &'a Value) -> Args<'a> {
        Args(value)
    }

    /// A text argument, non-empty once trimmed.
    pub fn text(&self, name: &str) -> Result<&'a str, Refused> {
        match self.0.get(name).and_then(Value::as_str).map(str::trim) {
            Some(s) if !s.is_empty() => Ok(s),
            _ => Err(Refused::new(
                "missing_argument",
                format!("`{name}` is needed, and must not be empty."),
            )),
        }
    }

    pub fn text_opt(&self, name: &str) -> Option<&'a str> {
        self.0
            .get(name)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    pub fn integer(&self, name: &str) -> Result<i64, Refused> {
        self.0.get(name).and_then(Value::as_i64).ok_or_else(|| {
            Refused::new(
                "missing_argument",
                format!("`{name}` is needed, as a whole number."),
            )
        })
    }

    pub fn number(&self, name: &str) -> Result<f64, Refused> {
        self.0.get(name).and_then(Value::as_f64).ok_or_else(|| {
            Refused::new(
                "missing_argument",
                format!("`{name}` is needed, as a number."),
            )
        })
    }

    pub fn flag(&self, name: &str) -> Result<bool, Refused> {
        self.0.get(name).and_then(Value::as_bool).ok_or_else(|| {
            Refused::new(
                "missing_argument",
                format!("`{name}` is needed, as true or false."),
            )
        })
    }

    pub fn raw(&self) -> &'a Value {
        self.0
    }
}

/// When the Mind is answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settle {
    /// When the command commits: the work is done.
    OnReturn,
    /// When the command commits, with `settled: false`: the work has only started.
    Deferred,
    /// When the invocation reaches its terminal outcome, within the deadline.
    Explicit { within: Duration },
}

/// Who may raise this intent.
pub enum Agents<A: App> {
    /// Anyone.
    Allowed,
    /// Not an agent, with this sentence.
    Refused(&'static str),
    /// An agent, if the rule allows this call.
    Rule(AgentRule<A>),
}

/// A rule read from the model that says whether an agent may raise an intent with these arguments.
pub type AgentRule<A> = Box<dyn Fn(&<A as App>::Model, &Args<'_>) -> Result<(), Refused>>;
/// A rule read from the model that every intent of the app is held to.
pub type StateRule<A> = Rc<dyn Fn(&<A as App>::Model) -> Result<(), Refused>>;
type Parse<A> = Box<dyn Fn(&Args<'_>) -> Result<<A as App>::Command, Refused>>;
pub(crate) type Reply<A> = Rc<dyn Fn(&<A as App>::Model) -> Value>;

/// A command with its wire contract.
pub struct Intent<A: App> {
    action: Action,
    parse: Parse<A>,
    reply: Reply<A>,
    agents: Agents<A>,
    settle: Settle,
}

impl<A: App> Intent<A> {
    /// `action` declares the name, the parameters, the grade and the description, as the surface
    /// publishes them; `parse` makes the command from the checked arguments.
    pub fn new(
        action: Action,
        parse: impl Fn(&Args<'_>) -> Result<A::Command, Refused> + 'static,
    ) -> Intent<A> {
        Intent {
            action,
            parse: Box::new(parse),
            reply: Rc::new(|_| Value::Object(Default::default())),
            agents: Agents::Allowed,
            settle: Settle::OnReturn,
        }
    }

    /// What the call answers, read from the committed model. Default: `{}`.
    pub fn reply(mut self, f: impl Fn(&A::Model) -> Value + 'static) -> Intent<A> {
        self.reply = Rc::new(f);
        self
    }

    pub fn agents(mut self, agents: Agents<A>) -> Intent<A> {
        self.agents = agents;
        self
    }

    pub fn settles(mut self, settle: Settle) -> Intent<A> {
        self.settle = settle;
        if matches!(settle, Settle::Deferred) {
            self.action = std::mem::replace(&mut self.action, Action::new("", "")).defers();
        }
        self
    }

    pub fn name(&self) -> &str {
        &self.action.name
    }
}

/// The app's intents, declared in `App::intents`.
pub struct Intents<A: App> {
    intents: Vec<Intent<A>>,
    state_rule: Option<StateRule<A>>,
}

impl<A: App> Default for Intents<A> {
    fn default() -> Self {
        Intents {
            intents: Vec::new(),
            state_rule: None,
        }
    }
}

impl<A: App> Intents<A> {
    pub fn new() -> Intents<A> {
        Intents::default()
    }

    pub fn add(&mut self, intent: Intent<A>) {
        self.intents.push(intent);
    }

    /// A rule about the app's own state that every call is held to before it is parsed, in the
    /// app's own words (the shell's is the locked desktop).
    pub fn state_rule(&mut self, rule: impl Fn(&A::Model) -> Result<(), Refused> + 'static) {
        self.state_rule = Some(Rc::new(rule));
    }

    pub fn len(&self) -> usize {
        self.intents.len()
    }

    pub fn is_empty(&self) -> bool {
        self.intents.is_empty()
    }

    pub fn names(&self) -> Vec<String> {
        self.intents.iter().map(|i| i.action.name.clone()).collect()
    }
}

/// The surface's registry for `table`, with every intent's handler bound to `core`. Serving it is
/// the runner's (`control::App::from_registry`); a test acts on it directly.
/// The most an intent may keep the Mind waiting for an explicit settlement, whatever it declares.
pub const MAX_SETTLE_WAIT: Duration = Duration::from_secs(30);

pub fn registry<A: App>(core: &Rc<Core<A>>, table: Intents<A>) -> LocalRegistry {
    let mut registry = LocalRegistry::new(A::ID);
    // An intent an agent may never raise is refused by the dispatch before it decides anything,
    // so a call that cannot run is never shown as an approval card and spends no grant. The
    // model-reading rules (`Agents::Rule`, the app's state rule) cannot run there: they are the
    // UI thread's, so they run in the handler, after a grant was spent, and fail closed.
    let never_for_agents: std::collections::HashMap<String, &'static str> = table
        .intents
        .iter()
        .filter_map(|i| match i.agents {
            Agents::Refused(sentence) => Some((i.action.name.clone(), sentence)),
            _ => None,
        })
        .collect();
    registry.set_state_rule(Box::new(move |action: &str| {
        match never_for_agents.get(action) {
            Some(sentence) if agent_is_calling() => Err((*sentence).to_string()),
            _ => Ok(()),
        }
    }));
    let weak = Rc::downgrade(core);
    let describe = weak.clone();
    registry.set_describe(Box::new(move || match describe.upgrade() {
        Some(core) => core.read(A::describe),
        None => View::new(format!("{} (closing)", A::ID)),
    }));
    for intent in table.intents {
        let Intent {
            action,
            parse,
            reply,
            agents,
            settle,
        } = intent;
        let core = weak.clone();
        let state_rule = table.state_rule.clone();
        let handler = move |args: &Value| -> Result<Value, String> {
            let core = core.upgrade().ok_or("the app is closing")?;
            let args = Args(args);
            if let Some(rule) = &state_rule {
                core.read(|m| rule(m)).map_err(|r| r.sentence)?;
            }
            let agent = agent_is_calling();
            if agent {
                match &agents {
                    Agents::Allowed => {}
                    Agents::Refused(sentence) => return Err(sentence.to_string()),
                    Agents::Rule(rule) => core.read(|m| rule(m, &args)).map_err(|r| r.sentence)?,
                }
            }
            let command = parse(&args).map_err(|r| r.sentence)?;
            let origin = Origin::Surface { agent };
            match settle {
                Settle::OnReturn | Settle::Deferred => {
                    core.run_invocation(origin, command, None)
                        .map_err(|r| r.sentence)?;
                    Ok(core.read(|m| reply(m)))
                }
                Settle::Explicit { within } => {
                    // The wait holds an RPC worker; an app's deadline is capped so two hung
                    // jobs cannot pin both workers until shutdown.
                    let within = within.min(MAX_SETTLE_WAIT);
                    let (tx, rx) = mpsc::sync_channel(1);
                    let id = core.open_settlement(tx, reply.clone());
                    if let Err(refused) = core.run_invocation(origin, command, Some(id)) {
                        core.drop_settlement(id);
                        return Err(refused.sentence);
                    }
                    let wait = move || {
                        rx.recv_timeout(within).map_err(|_| {
                            format!("the app did not settle this within {} s", within.as_secs())
                        })?
                    };
                    // The rest of the answer, off the UI thread. Handed back only when no
                    // dispatch is running (a handler called directly), and then run here.
                    match answer_later(wait) {
                        Ok(()) => Ok(Value::Null),
                        Err(wait) => wait(),
                    }
                }
            }
        };
        registry.add(action, Box::new(handler));
    }
    registry.add(crate::launch::action(), Box::new(launch_handler(weak)));
    registry
}

/// The `launch` action: a second copy's launch, applied once and answered as `present` is.
///
/// Request ids seen are remembered (the last [`crate::launch::REMEMBERED`]), so a retry of a
/// hand-over that was answered late opens nothing twice; it still brings the window forward. A
/// launch for another profile is refused: that is another instance's. An agent's call is refused
/// as `present`'s is: the desktop's hand-over is not a way to open files past the intents.
fn launch_handler<A: App>(
    core: std::rc::Weak<Core<A>>,
) -> impl Fn(&Value) -> Result<Value, String> + 'static {
    let seen: std::cell::RefCell<std::collections::VecDeque<String>> =
        std::cell::RefCell::new(std::collections::VecDeque::new());
    move |args: &Value| {
        if agent_is_calling() {
            return Err("launch is the desktop's own hand-over between two launches of an app; an \
                        agent drives the app through its other actions"
                .into());
        }
        let core = core.upgrade().ok_or("the app is closing")?;
        let launch = crate::launch::Launch::from_value(&args["launch"])?;
        let profile = core.paths().profile().to_string();
        if launch.profile != profile {
            return Err(format!(
                "that launch is for profile `{}`; this instance runs profile `{profile}`",
                launch.profile
            ));
        }
        let fresh = {
            let mut seen = seen.borrow_mut();
            if seen.contains(&launch.request) {
                false
            } else {
                if seen.len() >= crate::launch::REMEMBERED {
                    seen.pop_front();
                }
                seen.push_back(launch.request.clone());
                true
            }
        };
        if fresh {
            core.event(crate::app::Event::Launch(launch));
        } else {
            tracing::info!(app = A::ID, "a launch handed over twice; applied once");
        }
        crate::present::handle(args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_read_what_the_dispatch_filled_and_refuse_an_empty_text() {
        let v =
            serde_json::json!({"name": "  Dallas ", "n": 3, "x": 1.5, "on": true, "blank": "  "});
        let a = Args::new(&v);
        assert_eq!(a.text("name").unwrap(), "Dallas");
        assert_eq!(a.integer("n").unwrap(), 3);
        assert_eq!(a.number("x").unwrap(), 1.5);
        assert!(a.flag("on").unwrap());
        assert_eq!(a.text_opt("blank"), None);
        let e = a.text("blank").unwrap_err();
        assert_eq!(e.kind, "missing_argument");
        assert!(a.integer("name").is_err());
    }
}
