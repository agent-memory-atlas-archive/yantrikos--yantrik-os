//! An agent's commands, on the shell's surface: `agent_run`, `agent_job`, `agent_input`,
//! `agent_kill`.
//!
//! The terminal itself — a PTY per command, the directory that carries, the process groups, the
//! built environment, the caps — is `yantrik-agent-terminal`, tested without a shell. This module
//! is the door: it reads who is calling, turns the call's agent token into an agent, and hands the
//! work to the terminal off the UI thread. See `design/agents-workspace-2026-09-23.md`, decision 3.
//!
//! # Who the agent is
//!
//! Never an argument, and the token that says it is not one either. The token the host gave the
//! agent's harness with its first turn rides on `app.act` BESIDE `args` — `{action, args,
//! agent_token}`, the way a grant does — and reaches these handlers as `control::agent_token()`.
//! It is kept out of `args` because `args` is what gets shown and kept: the approval card draws
//! them, `record_unasked_action` writes them to `mind-audit.jsonl`, a grant is bound to them. The
//! runtime strips an `agent_token` a caller puts inside `args` anyway.
//!
//! The resolver checks the token against the kernel's account of the caller: the pid on the socket
//! has to descend from the harness that holds it. Until the host issues tokens (piece 1),
//! [`install_resolver`] has not been called and the resolver knows none, so every call is answered
//! "no agent holds this token": inert, but a real answer.
//!
//! # Off the UI thread
//!
//! A command can take minutes and its caller is owed the exit code, so each handler only reads its
//! arguments, the caller and the token on the UI thread and hands the rest — resolving the token,
//! starting, waiting, killing — to `control::answer_later`, which finishes it on the socket's side.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use serde_json::{json, Value};
use yantrik_agent_terminal::{
    AgentId, AgentResolver, JobId, JobState, Jobs, Limits, NoAgents, RunAnswer, DEFAULT_WAIT,
};
use yantrik_app_runtime::control::{self, Action, App as ControlSurface, Param};

static JOBS: OnceLock<Jobs> = OnceLock::new();
static RESOLVER: RwLock<Option<Arc<dyn AgentResolver>>> = RwLock::new(None);

/// Every agent's commands. The Agents screen draws from this (`on_output`, `with_screen`,
/// `type_input`) — the same store the actions answer from, so a card and an answer cannot disagree.
pub fn jobs() -> &'static Jobs {
    JOBS.get_or_init(|| Jobs::new(Limits::default()))
}

/// Where tokens come from. The host installs this once it issues them:
/// `install_resolver(Arc::new(Lookup(move |t: &str| host.agent_for_token(t))))`.
#[allow(dead_code)]
pub fn install_resolver(resolver: Arc<dyn AgentResolver>) {
    *RESOLVER.write().unwrap_or_else(|e| e.into_inner()) = Some(resolver);
}

fn resolver() -> Arc<dyn AgentResolver> {
    RESOLVER
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_else(|| Arc::new(NoAgents))
}

/// Kill every agent command, as the shell closes.
pub fn shutdown() {
    if let Some(jobs) = JOBS.get() {
        jobs.shutdown();
    }
}

/// What the dispatch established about one call, read on the UI thread where it is set and
/// carried to the work.
struct Call {
    /// The socket peer, as the kernel reported it — the only account of the caller the token is
    /// checked against.
    pid: Option<u32>,
    /// What rode beside `args`.
    token: Option<String>,
}

impl Call {
    fn current() -> Call {
        Call {
            pid: control::caller().and_then(|c| u32::try_from(c.pid).ok()).filter(|pid| *pid > 0),
            token: control::agent_token(),
        }
    }

    fn agent(&self) -> Result<AgentId, String> {
        resolver().resolve(self.token.as_deref().unwrap_or_default(), self.pid)
    }
}

/// Hand the work to the socket's side; run it here only when called without a socket.
fn later(work: impl FnOnce() -> Result<Value, String> + Send + 'static) -> Result<Value, String> {
    control::answer_later(work)
        .map(|()| json!({ "answering": "off the UI thread" }))
        .or_else(|work| work())
}

fn text(args: &Value, name: &str) -> String {
    args.get(name).and_then(Value::as_str).unwrap_or_default().to_string()
}

fn job_arg(args: &Value) -> Result<JobId, String> {
    let job = text(args, "job").trim().to_string();
    if job.is_empty() {
        return Err("`job` is empty: the id `agent_run` answered with.".to_string());
    }
    Ok(JobId(job))
}

/// `wait`, in seconds: the design's default, and never past the terminal's own bound.
fn wait_arg(args: &Value) -> Result<Duration, String> {
    let most = jobs().limits().max_wait.as_secs();
    let Some(given) = args.get("wait").filter(|v| !v.is_null()) else {
        return Ok(DEFAULT_WAIT);
    };
    let secs = given
        .as_f64()
        .or_else(|| given.as_str().and_then(|s| s.trim().parse().ok()))
        .ok_or_else(|| "`wait` is a number of seconds.".to_string())?;
    if !(0.0..=most as f64).contains(&secs) {
        return Err(format!(
            "`wait` is between 0 and {most} seconds. A command still running when it runs out \
             answers `running: true` with its job id, and `agent_job` waits on it again."
        ));
    }
    Ok(Duration::from_secs_f64(secs))
}

/// The terminal's answer, with the next step spelled out where there is one.
fn answer_json(answer: &RunAnswer) -> Value {
    let mut out = answer.to_json();
    match answer.state {
        JobState::Running { waiting_for_input: true } => {
            out["next"] = "it looks like it is waiting for input: the person can answer in its \
                           card, `agent_input` sends text (end it with \\n to press Return), \
                           `agent_kill` stops it"
                .into();
        }
        JobState::Running { .. } => {
            out["next"] = "still running: `agent_job` waits for it again, `agent_kill` stops it".into();
        }
        _ => {}
    }
    out
}

/// Said once, in every description, because it is the only documentation a mind reads.
const WHO: &str = " Acts for the agent named by the agent token your call carries beside `args` \
                   (`yos act --agent-token`, or YANTRIK_AGENT_TOKEN in yos's environment) — never \
                   an argument.";

/// The four actions as published: what `describe shell` lists and a caller is asked for.
fn specs() -> [Action; 4] {
    [
        // Sensitive, like the Terminal's own `run`: whatever the command does, it does as the
        // person. Deferred because the answer may be `running: true` — the work outlives the call
        // — and the caller has to read `running` rather than assume.
        Action::new(
            "agent_run",
            &format!(
                "Run one command line in a fresh terminal of your own, in your pane — not the \
                 person's Terminal. Answers when it exits, with `exit_code` (or `signal`), \
                 `cwd_after` and the `tail` of its output; if it is still going after `wait` it \
                 answers `running: true` with a `job` id. The directory carries to your next \
                 command; exported variables and other shell state do not. The environment is \
                 HOME, USER, PATH, LANG and TERM only.{WHO}"
            ),
        )
        .risk("sensitive")
        .defers()
        .arg(Param::text("command").describe(
            "One command line, as it would be typed. Pipes, redirection, `&&` and `cd` work; it \
             runs under bash",
        ))
        .arg(
            Param::text("cwd")
                .optional()
                .describe("Where to run it, absolute or relative to your current directory. Left out: where your last command ended"),
        )
        .arg(
            Param::number("wait")
                .optional()
                .describe("Seconds to wait for it to finish before answering `running: true`. Default 120, at most 600"),
        ),
        Action::new(
            "agent_job",
            &format!(
                "Wait for one of your commands that answered `running: true`, and say where it \
                 stands: the same answer `agent_run` gives. `wait: 0` only looks. Answers early \
                 if the command starts waiting for input.{WHO}"
            ),
        )
        .arg(Param::text("job").describe("The `job` id `agent_run` answered with"))
        .arg(
            Param::number("wait")
                .optional()
                .describe("Seconds to wait for it to finish. Default 120, at most 600"),
        ),
        // Sensitive for the Terminal `send_input`'s reason: a program at a prompt cannot tell
        // these bytes from typing, and the prompt may be `sudo`'s.
        Action::new(
            "agent_input",
            &format!(
                "Type into one of your running commands, exactly the characters given — no \
                 newline is added, so end with \\n to press Return; \\u0003 is Ctrl-C. Answers \
                 with where the command stands a moment later.{WHO}"
            ),
        )
        .risk("sensitive")
        .arg(Param::text("job").describe("The `job` id `agent_run` answered with"))
        .arg(Param::text("text").describe("The exact characters to send, up to 64 KiB")),
        Action::new(
            "agent_kill",
            &format!(
                "Stop one of your commands: its whole process group gets SIGTERM, and SIGKILL two \
                 seconds later if anything is left. Answers once it has ended, with how.{WHO}"
            ),
        )
        .arg(Param::text("job").describe("The `job` id `agent_run` answered with")),
    ]
}

/// The four actions, for the shell's surface. Each reads the call on the UI thread and hands
/// everything else to the function of the same name.
pub fn actions(surface: ControlSurface) -> ControlSurface {
    let [run, job, input, kill] = specs();
    surface
        .action(run, |args| agent_run(args, Call::current()))
        .action(job, |args| agent_job(args, Call::current()))
        .action(input, |args| agent_input(args, Call::current()))
        .action(kill, |args| agent_kill(args, Call::current()))
}

fn agent_run(args: &Value, call: Call) -> Result<Value, String> {
    let command = text(args, "command");
    let cwd = args
        .get("cwd")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(PathBuf::from);
    let wait = wait_arg(args)?;
    later(move || {
        let agent = call.agent()?;
        Ok(answer_json(&jobs().run(&agent, &command, cwd, wait)?))
    })
}

fn agent_job(args: &Value, call: Call) -> Result<Value, String> {
    let job = job_arg(args)?;
    let wait = wait_arg(args)?;
    later(move || {
        let agent = call.agent()?;
        Ok(answer_json(&jobs().job(&agent, &job, wait)?))
    })
}

fn agent_input(args: &Value, call: Call) -> Result<Value, String> {
    let job = job_arg(args)?;
    let typed = text(args, "text");
    if typed.is_empty() {
        return Err("`text` is empty: the exact characters to send.".to_string());
    }
    later(move || {
        let agent = call.agent()?;
        jobs().input(&agent, &job, &typed)?;
        // A moment for the command to react, so the tail shows what it did with it.
        let mut out = answer_json(&jobs().job(&agent, &job, Duration::from_millis(400))?);
        out["sent_bytes"] = typed.len().into();
        Ok(out)
    })
}

fn agent_kill(args: &Value, call: Call) -> Result<Value, String> {
    let job = job_arg(args)?;
    later(move || {
        let agent = call.agent()?;
        let stopped = jobs().kill(&agent, &job)?;
        let settle = jobs().limits().kill_grace + Duration::from_millis(500);
        let mut out = answer_json(&jobs().job(&agent, &job, settle)?);
        out["stopped"] = stopped.into();
        if !stopped {
            out["note"] = "it had already ended; nothing was signalled".into();
        }
        Ok(out)
    })
}

/// How much of a command line `describe` repeats.
const COMMAND_CLIP: usize = 160;

/// `describe shell` → `agent_jobs`: per agent, the commands running now.
pub fn for_describe() -> Value {
    // Nothing has ever run: say so without starting the store for it.
    let Some(jobs) = JOBS.get() else { return json!([]) };
    let mut by_agent: Vec<(String, Vec<Value>)> = Vec::new();
    for summary in jobs.list() {
        let JobState::Running { waiting_for_input } = summary.state else { continue };
        let command: String = summary.command.chars().take(COMMAND_CLIP).collect();
        let entry = json!({
            "job": summary.job,
            "command": if summary.command.chars().count() > COMMAND_CLIP { format!("{command}…") } else { command },
            "elapsed_secs": summary.elapsed.as_secs(),
            "waiting_for_input": waiting_for_input,
        });
        let agent = summary.agent.to_string();
        match by_agent.iter_mut().find(|(a, _)| *a == agent) {
            Some((_, list)) => list.push(entry),
            None => by_agent.push((agent, vec![entry])),
        }
    }
    Value::Array(
        by_agent
            .into_iter()
            .map(|(agent, running)| json!({ "agent": agent, "running": running }))
            .collect(),
    )
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use yantrik_agent_terminal::{TokenTable, NO_AGENT};

    fn call(token: &str) -> Call {
        Call { pid: Some(std::process::id()), token: Some(token.to_string()) }
    }

    /// The token is not an argument of any of the four, so nothing that shows or keeps
    /// arguments — `describe shell`'s action list, an approval card, an audit line — is ever
    /// handed one by these actions.
    #[test]
    fn no_agent_action_takes_the_token_as_an_argument() {
        for spec in specs() {
            let schema = spec.schema();
            let params = &schema["parameters"]["properties"];
            assert!(params.get("agent_token").is_none(), "{} takes the token as an argument: {schema}", spec.name);
            assert!(params.get("agent").is_none(), "{} takes the agent as an argument: {schema}", spec.name);
            assert!(
                schema["description"].as_str().is_some_and(|d| d.contains("beside `args`")),
                "{} does not say where the token goes: {schema}",
                spec.name
            );
        }
    }

    /// The four actions end to end, minus only the socket: with no dispatch in progress the work
    /// runs inline, so this drives the real token check, the real terminal and `describe`.
    ///
    /// One test, because the resolver is the shell's one global: "no tokens yet" has to be seen
    /// before any are installed.
    #[test]
    fn the_actions_answer_for_the_agent_the_token_names_and_nobody_else() {
        let me = Some(std::process::id());

        // Before the host issues tokens, every call is inert — and says so.
        let err = agent_run(&json!({"command": "echo hi"}), call("t-pi")).unwrap_err();
        assert!(err.starts_with(NO_AGENT), "{err}");
        let err = agent_run(&json!({"command": "echo hi"}), Call { pid: me, token: None }).unwrap_err();
        assert!(err.contains("no agent token came with this call"), "{err}");

        // This test process stands in for the harness that holds both tokens.
        let table = Arc::new(TokenTable::new());
        table.issue("t-pi", AgentId::new("pi", "c-shell"), me);
        table.issue("t-ds", AgentId::new("deepseek", "c-shell"), me);
        install_resolver(table);

        let done = agent_run(&json!({"command": "cd /tmp && echo hi", "wait": 10}), call("t-pi")).unwrap();
        assert_eq!(done["exit_code"], 0, "{done}");
        assert_eq!(done["cwd_after"], "/tmp", "{done}");
        assert_eq!(done["tail"], "hi", "{done}");
        assert_eq!(done["agent"], "pi:c-shell", "the agent is the token's");
        assert!(!done.to_string().contains("t-pi"), "the answer does not repeat the token: {done}");

        let slow = agent_run(&json!({"command": "sleep 30", "wait": 0.5}), call("t-pi")).unwrap();
        assert_eq!(slow["running"], true, "{slow}");
        assert!(slow["next"].as_str().is_some_and(|n| n.contains("agent_job")), "{slow}");
        let job = slow["job"].clone();

        // `describe shell` lists it under its agent, and nowhere names the token.
        let described = for_describe();
        let pi = described
            .as_array()
            .and_then(|agents| agents.iter().find(|a| a["agent"] == "pi:c-shell"))
            .unwrap_or_else(|| panic!("pi is not listed: {described}"));
        assert!(pi["running"].as_array().unwrap().iter().any(|j| j["job"] == job && j["command"] == "sleep 30"));
        assert!(!described.to_string().contains("t-pi"), "{described}");

        // Another agent's token cannot touch it; nor can pi's token from outside pi's harness.
        let err = agent_kill(&json!({"job": job}), call("t-ds")).unwrap_err();
        assert!(err.contains("belongs to another agent"), "{err}");
        let outsider = Call { pid: Some(1), token: Some("t-pi".into()) };
        let err = agent_job(&json!({"job": job, "wait": 0}), outsider).unwrap_err();
        assert!(err.contains("not issued to the process"), "{err}");

        let looked = agent_job(&json!({"job": job, "wait": 0}), call("t-pi")).unwrap();
        assert_eq!(looked["running"], true);

        let killed = agent_kill(&json!({"job": job}), call("t-pi")).unwrap();
        assert_eq!((killed["stopped"].clone(), killed["signal_name"].clone()), (json!(true), json!("SIGTERM")), "{killed}");

        let answered = agent_run(&json!({"command": "read -r x; echo \"[$x]\"", "wait": 0.3}), call("t-pi")).unwrap();
        let typed = agent_input(&json!({"job": answered["job"], "text": "yes\n"}), call("t-pi")).unwrap();
        assert_eq!(typed["sent_bytes"], 4);
        let finished = agent_job(&json!({"job": answered["job"], "wait": 10}), call("t-pi")).unwrap();
        assert_eq!(finished["tail"], "yes\n[yes]", "the echo of what was typed, then the answer: {finished}");

        // The bounds on `wait`, refused before anything is started.
        let err = agent_run(&json!({"command": "true", "wait": 601}), call("t-pi")).unwrap_err();
        assert!(err.contains("between 0 and 600"), "{err}");
    }
}
