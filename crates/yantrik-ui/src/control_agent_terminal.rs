//! An agent's commands, on the shell's surface: `agent_run`, `agent_job`, `agent_input`,
//! `agent_kill`.
//!
//! The terminal itself — a PTY per command, the directory that carries, the process groups, the
//! built environment, the caps — is `yantrik-agent-terminal`, tested without a shell. This module
//! is the door: it reads who is calling, turns the caller's `agent_token` into an agent, and hands
//! the work to the terminal off the UI thread. See `design/agents-workspace-2026-09-23.md`,
//! decision 3.
//!
//! # Who the agent is
//!
//! Never an argument. Every action takes `agent_token` — the token the host gave the agent's
//! harness with its first turn — and the resolver checks it against the kernel's account of the
//! caller: the pid on the socket has to descend from the harness that holds the token. Until the
//! host issues tokens (piece 1), [`install_resolver`] has not been called and the resolver knows
//! none, so every call is answered "no agent holds this token": inert, but a real answer.
//!
//! # Off the UI thread
//!
//! A command can take minutes and its caller is owed the exit code, so each handler only reads its
//! arguments and the caller on the UI thread and hands the rest — resolving the token, starting,
//! waiting, killing — to `control::answer_later`, which finishes it on the socket's side.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use serde_json::{json, Value};
use yantrik_agent_terminal::{AgentResolver, JobId, JobState, Jobs, Limits, NoAgents, RunAnswer, DEFAULT_WAIT};
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

/// The socket peer, as the kernel reported it — the only account of the caller the token is
/// checked against.
fn caller_pid() -> Option<u32> {
    control::caller().and_then(|c| u32::try_from(c.pid).ok()).filter(|pid| *pid > 0)
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

fn token_param() -> Param {
    Param::text("agent_token").describe(
        "The agent token your harness was given (YANTRIK_AGENT_TOKEN). It says which agent you \
         are; there is no `agent` argument, and a token only works from the process tree it was \
         issued to",
    )
}

/// The four actions, for the shell's surface.
pub fn actions(surface: ControlSurface) -> ControlSurface {
    surface
        .action(
            // Sensitive, like the Terminal's own `run`: whatever the command does, it does as the
            // person. Deferred because the answer may be `running: true` — the work outlives the
            // call — and the caller has to read `running` rather than assume.
            Action::new(
                "agent_run",
                "Run one command line in a fresh terminal of your own, in your pane — not the \
                 person's Terminal. Answers when it exits, with `exit_code` (or `signal`), \
                 `cwd_after` and the `tail` of its output; if it is still going after `wait` it \
                 answers `running: true` with a `job` id. The directory carries to your next \
                 command; exported variables and other shell state do not. The environment is \
                 HOME, USER, PATH, LANG and TERM only.",
            )
            .risk("sensitive")
            .defers()
            .arg(token_param())
            .arg(Param::text("command").describe(
                "One command line, as it would be typed. Pipes, redirection, `&&` and `cd` work; \
                 it runs under bash",
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
            move |args| {
                let token = text(args, "agent_token");
                let command = text(args, "command");
                let cwd = args
                    .get("cwd")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|c| !c.is_empty())
                    .map(PathBuf::from);
                let wait = wait_arg(args)?;
                let pid = caller_pid();
                later(move || {
                    let agent = resolver().resolve(&token, pid)?;
                    Ok(answer_json(&jobs().run(&agent, &command, cwd, wait)?))
                })
            },
        )
        .action(
            Action::new(
                "agent_job",
                "Wait for one of your commands that answered `running: true`, and say where it \
                 stands: the same answer `agent_run` gives. `wait: 0` only looks. Answers early \
                 if the command starts waiting for input.",
            )
            .arg(token_param())
            .arg(Param::text("job").describe("The `job` id `agent_run` answered with"))
            .arg(
                Param::number("wait")
                    .optional()
                    .describe("Seconds to wait for it to finish. Default 120, at most 600"),
            ),
            move |args| {
                let token = text(args, "agent_token");
                let job = job_arg(args)?;
                let wait = wait_arg(args)?;
                let pid = caller_pid();
                later(move || {
                    let agent = resolver().resolve(&token, pid)?;
                    Ok(answer_json(&jobs().job(&agent, &job, wait)?))
                })
            },
        )
        .action(
            // Sensitive for the Terminal `send_input`'s reason: a program at a prompt cannot tell
            // these bytes from typing, and the prompt may be `sudo`'s.
            Action::new(
                "agent_input",
                "Type into one of your running commands, exactly the characters given — no \
                 newline is added, so end with \\n to press Return; \\u0003 is Ctrl-C. Answers \
                 with where the command stands a moment later.",
            )
            .risk("sensitive")
            .arg(token_param())
            .arg(Param::text("job").describe("The `job` id `agent_run` answered with"))
            .arg(Param::text("text").describe("The exact characters to send, up to 64 KiB")),
            move |args| {
                let token = text(args, "agent_token");
                let job = job_arg(args)?;
                let typed = text(args, "text");
                if typed.is_empty() {
                    return Err("`text` is empty: the exact characters to send.".to_string());
                }
                let pid = caller_pid();
                later(move || {
                    let agent = resolver().resolve(&token, pid)?;
                    jobs().input(&agent, &job, &typed)?;
                    // A moment for the command to react, so the tail shows what it did with it.
                    let mut out = answer_json(&jobs().job(&agent, &job, Duration::from_millis(400))?);
                    out["sent_bytes"] = typed.len().into();
                    Ok(out)
                })
            },
        )
        .action(
            Action::new(
                "agent_kill",
                "Stop one of your commands: its whole process group gets SIGTERM, and SIGKILL two \
                 seconds later if anything is left. Answers once it has ended, with how.",
            )
            .arg(token_param())
            .arg(Param::text("job").describe("The `job` id `agent_run` answered with")),
            move |args| {
                let token = text(args, "agent_token");
                let job = job_arg(args)?;
                let pid = caller_pid();
                later(move || {
                    let agent = resolver().resolve(&token, pid)?;
                    let stopped = jobs().kill(&agent, &job)?;
                    let settle = jobs().limits().kill_grace + Duration::from_millis(500);
                    let mut out = answer_json(&jobs().job(&agent, &job, settle)?);
                    out["stopped"] = stopped.into();
                    if !stopped {
                        out["note"] = "it had already ended; nothing was signalled".into();
                    }
                    Ok(out)
                })
            },
        )
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
