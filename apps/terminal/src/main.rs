//! Yantrik Terminal — standalone app binary.
//!
//! Basic terminal emulator using std::process::Command.
//! PTY support is stubbed out (requires platform-specific libraries).

use std::cell::RefCell;
use std::rc::Rc;

use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use yantrik_app_runtime::prelude::*;

slint::include_modules!();

/// Fill the agent rail from the terminal's own state.
///
/// A shell knows exactly two things worth offering: where it is, and what just happened in it.
/// Both are already on screen and neither needs a model to establish. The one suggestion that
/// DOES need a model is offered only when the companion is reachable.
fn refresh_agent_rail(ui: &TerminalApp) {
    let cwd = ui.get_current_directory().to_string();
    let mut context: Vec<AgentContextItem> = Vec::new();
    if !cwd.is_empty() {
        context.push(AgentContextItem {
            id: "cwd".into(),
            label: cwd.clone().into(),
            detail: "working directory".into(),
            source: "file".into(),
        });
    }
    let took = ui.get_last_command_duration().to_string();
    if !took.is_empty() {
        context.push(AgentContextItem {
            id: "last".into(),
            label: format!("Last command took {took}").into(),
            detail: SharedString::new(),
            source: "linked".into(),
        });
    }
    ui.set_agent_context(ModelRc::new(VecModel::from(context)));

    let online = companion::is_online();
    let has_output = !ui.get_terminal_output().to_string().trim().is_empty();
    let mut next: Vec<AgentSuggestion> = Vec::new();
    if online && has_output {
        next.push(AgentSuggestion {
            id: "explain".into(),
            label: "Explain what just happened".into(),
            detail: "reads the last of the output".into(),
            icon: "spark".into(),
            running: ui.get_proposal_working(),
            proposes: false,
        });
    }
    ui.set_agent_suggestions(ModelRc::new(VecModel::from(next)));
    ui.set_agent_unavailable(if online || !has_output {
        SharedString::new()
    } else {
        companion::OFFLINE_HINT.into()
    });
}

fn main() {
    init_tracing("yantrik-terminal");

    // One window per app: a second launch defers to the running one (the shell focuses it).
    let Some(_instance) = instance::claim("terminal") else { return };

    let app = TerminalApp::new().unwrap();

    // Same dark/accent choice as the shell, read from the shell's settings file.
    let theme = theme::load();
    app.global::<ThemeMode>().set_dark(theme.dark);
    app.global::<AccentPreset>().set_index(theme.accent_index);

    wire(&app);
    // ── The agent layer ──
    //
    // The answer changes nothing -- it explains output that has already happened -- so the card
    // gets one button. What the model is given is the TAIL of the output, not the scrollback: a
    // shell session can run to megabytes and the question is about what just happened.
    {
        let weak = app.as_weak();
        app.on_agent_suggestion_activated(move |id| {
            let Some(ui) = weak.upgrade() else { return };
            if id != "explain" {
                return;
            }
            let out = ui.get_terminal_output().to_string();
            let tail = out
                .lines()
                .rev()
                .take(40)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n");
            let cwd = ui.get_current_directory().to_string();
            ui.set_proposal_working(true);
            ui.set_proposal(AgentProposal {
                title: "What just happened".into(),
                source: format!("from the last 40 lines in {cwd}").into(),
                ..Default::default()
            });
            let prompt = format!(
                "This is the tail of my shell session in {cwd}. In at most four short lines, say \
                 what happened and what to do next. If there is an error, name its cause. Use only \
                 what is shown.\n\n{tail}"
            );
            let back = ui.as_weak();
            std::thread::spawn(move || {
                let outcome = companion::ask(&prompt);
                let _ = back.upgrade_in_event_loop(move |ui| {
                    ui.set_proposal_working(false);
                    match outcome {
                        Ok(text) => ui.set_proposal(AgentProposal {
                            title: "What just happened".into(),
                            body: text.into(),
                            source: "from your shell output".into(),
                            verb: "Close".into(),
                            ..Default::default()
                        }),
                        Err(e) => ui.set_proposal(AgentProposal {
                            title: "The companion did not answer".into(),
                            body: format!("{e}").into(),
                            verb: "Close".into(),
                            ..Default::default()
                        }),
                    }
                });
            });
        });
    }
    {
        let weak = app.as_weak();
        app.on_proposal_dismissed(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_proposal(AgentProposal::default());
            }
        });
    }
    app.on_proposal_applied(|| {});
    app.on_agent_context_activated(|_| {});
    refresh_agent_rail(&app);

    app.run().unwrap();
}

// ── Wire all callbacks ───────────────────────────────────────────────

/// The last thing this terminal ran, and whether it worked.
#[derive(Clone)]
struct LastCommand {
    command: String,
    exit_code: i32,
}

// ── The control surface ──────────────────────────────────────────────
//
// Read-only, and deliberately so. The companion already has `run_command`, which runs on a worker
// thread; this terminal runs commands with a blocking `Command::output()` on the UI thread, so an
// agent-issued command would freeze the window for as long as it took. Publishing a `run` action
// here would be a second, worse path to something we already do properly.
//
// What it *can* do that nothing else can is say what the person at the keyboard is doing — which
// directory they are in, what they last ran, and whether it failed. That is the whole point of
// the ErrorCompanion feature, and until now it had no way to find out.

/// Run one command line in the terminal and show the result, exactly as if it had been typed.
///
/// `cd` moves the working directory (a shelled-out `cd` would not persist); everything else runs
/// through the shell. The output is appended to the visible buffer and recorded as the last
/// command, and also returned, so the key handler and the `run` control action share one code
/// path and cannot disagree about what "running a command" means.
///
/// It runs the process to completion on the calling thread — the same blocking `output()` the
/// interactive path has always used, so it is no worse than a person typing the command. A caller
/// handing it something that never returns will hang the window; an agent should reach for the
/// companion's `run_command` for anything long-running and use this for the things a person types.
fn exec_command(
    cmd: &str,
    ui: &TerminalApp,
    cwd: &Rc<RefCell<String>>,
    buf: &Rc<RefCell<String>>,
    last: &Rc<RefCell<Option<LastCommand>>>,
) -> (String, i32) {
    let trimmed = cmd.trim();
    let parts: Vec<&str> = trimmed.split_whitespace().collect();

    // `cd` is the one builtin the terminal has to implement itself.
    if parts.first() == Some(&"cd") {
        let target_arg = parts.get(1).copied().unwrap_or("~");
        let target = if target_arg == "~" {
            std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .unwrap_or_else(|_| "/".to_string())
        } else {
            let current = cwd.borrow().clone();
            std::path::Path::new(&current).join(target_arg).to_string_lossy().to_string()
        };
        if std::path::Path::new(&target).is_dir() {
            *cwd.borrow_mut() = target.clone();
            ui.set_current_directory(target.into());
            let mut b = buf.borrow_mut();
            b.push_str("\n$ ");
            ui.set_terminal_output(b.clone().into());
            (String::new(), 0)
        } else {
            let msg = format!("cd: no such directory: {target}");
            let mut b = buf.borrow_mut();
            b.push_str(&format!("\n{msg}\n$ "));
            ui.set_terminal_output(b.clone().into());
            (msg, 1)
        }
    } else {
        let current_dir = cwd.borrow().clone();
        let shell = if cfg!(target_os = "windows") { "cmd" } else { "sh" };
        let flag = if cfg!(target_os = "windows") { "/C" } else { "-c" };
        let result = std::process::Command::new(shell)
            .arg(flag)
            .arg(trimmed)
            .current_dir(&current_dir)
            .output();
        let (output_text, exit) = match result {
            Ok(ref o) => {
                let mut combined = String::new();
                combined.push_str(&String::from_utf8_lossy(&o.stdout));
                combined.push_str(&String::from_utf8_lossy(&o.stderr));
                // 127 is the shell's own "could not run it": the closest honest exit when the
                // process never started.
                (combined, o.status.code().unwrap_or(-1))
            }
            Err(ref e) => (format!("Error: {e}\n"), 127),
        };
        *last.borrow_mut() = Some(LastCommand { command: trimmed.to_string(), exit_code: exit });
        let mut b = buf.borrow_mut();
        b.push('\n');
        b.push_str(&output_text);
        if !output_text.ends_with('\n') && !output_text.is_empty() {
            b.push('\n');
        }
        b.push_str("$ ");
        ui.set_terminal_output(b.clone().into());
        (output_text, exit)
    }
}

fn publish_control(
    app: &TerminalApp,
    output: Rc<RefCell<String>>,
    last_command: Rc<RefCell<Option<LastCommand>>>,
    cwd: Rc<RefCell<String>>,
) {
    use yantrik_app_runtime::control::{Action, App, Param, View};

    let describe = {
        let weak = app.as_weak();
        let out = output.clone();
        let last = last_command.clone();
        move || {
            let Some(ui) = weak.upgrade() else {
                return View::new("Terminal — closing");
            };
            let cwd = ui.get_current_directory().to_string();
            let last = last.borrow().clone();

            let summary = match &last {
                Some(c) if c.exit_code != 0 => {
                    format!("Terminal — in {cwd}, `{}` failed with {}", c.command, c.exit_code)
                }
                Some(c) => format!("Terminal — in {cwd}, last ran `{}`", c.command),
                None => format!("Terminal — in {cwd}, nothing run yet"),
            };

            // The tail, not the transcript. A long session's scrollback is unbounded, and what
            // anyone wants is what just happened.
            let buffer = out.borrow();
            let tail: String = buffer
                .lines()
                .rev()
                .take(60)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n");

            View::new(summary)
                .with("directory", cwd)
                .with("alive", ui.get_is_alive())
                .with(
                    "last_command",
                    match &last {
                        Some(c) => serde_json::json!({
                            "command": c.command,
                            "exit_code": c.exit_code,
                            "failed": c.exit_code != 0,
                        }),
                        None => serde_json::Value::Null,
                    },
                )
                .with("recent_output", tail)
        }
    };

    let weak = app.as_weak();
    let run_weak = app.as_weak();
    let run_out = output.clone();
    let run_last = last_command;
    let run_cwd = cwd;
    let cleared = output;

    App::new("terminal")
        .describe(describe)
        .action(Action::new("clear", "Empty the terminal's scrollback"), move |_| {
            let ui = weak.upgrade().ok_or_else(|| "Terminal window is gone".to_string())?;
            *cleared.borrow_mut() = "$ ".to_string();
            ui.set_terminal_output("$ ".into());
            Ok(serde_json::json!({ "cleared": true }))
        })
        .action(
            // What the terminal could not do before: an agent can run a command IN the visible
            // window and read the result, so the person sees what the agent ran — the thing the
            // companion's headless run_command cannot give them. `sensitive`, because it is the
            // user's own shell and a command can do real work; the caller's ceiling decides.
            Action::new("run", "Run a command in the terminal and return its output")
                .risk("sensitive")
                .arg(Param::text("command").describe("The command line to run, e.g. `ls -la` or `cd /tmp`")),
            move |args| {
                let ui = run_weak.upgrade().ok_or_else(|| "Terminal window is gone".to_string())?;
                let command = args["command"].as_str().unwrap_or_default().trim().to_string();
                if command.is_empty() {
                    return Err("`command` is empty".into());
                }
                // Echo the command into the buffer first, so the visible terminal shows it on the
                // prompt exactly as if it had been typed, then run it through the shared path.
                {
                    let mut b = run_out.borrow_mut();
                    b.push_str(&command);
                    ui.set_terminal_output(b.clone().into());
                }
                let (output, exit) = exec_command(&command, &ui, &run_cwd, &run_out, &run_last);
                Ok(serde_json::json!({
                    "command": command,
                    "exit_code": exit,
                    "directory": run_cwd.borrow().clone(),
                    "output": output,
                }))
            },
        )
        .serve();
}

fn wire(app: &TerminalApp) {
    let output_buffer: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
    let last_command: Rc<RefCell<Option<LastCommand>>> = Rc::new(RefCell::new(None));
    let cwd: Rc<RefCell<String>> = Rc::new(RefCell::new(
        std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| "/".to_string()),
    ));

    // Set initial state
    app.set_is_alive(true);
    app.set_current_directory(cwd.borrow().clone().into());
    app.set_tab_count(1);
    app.set_active_tab(0);
    let initial_tab = TerminalTabData {
        title: "Terminal".into(),
        is_active: true,
        is_alive: true,
    };
    app.set_tabs(ModelRc::new(VecModel::from(vec![initial_tab])));

    // Show welcome prompt
    {
        let welcome = format!("Yantrik Terminal v0.1.0\n$ ");
        *output_buffer.borrow_mut() = welcome.clone();
        app.set_terminal_output(welcome.into());
    }

    // Key pressed — simplified: we collect input and run on Enter
    {
        let weak = app.as_weak();
        let buf = output_buffer.clone();
        let cwd_ref = cwd.clone();
        let input_line: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
        let last_run = last_command.clone();

        app.on_terminal_key_pressed(move |event| {
            let Some(ui) = weak.upgrade() else {
                return slint::private_unstable_api::re_exports::EventResult::Reject;
            };

            let text = event.text.to_string();

            // Enter key
            if text == "\n" || text == "\r" {
                let cmd_str = input_line.borrow().clone();
                *input_line.borrow_mut() = String::new();

                if cmd_str.trim().is_empty() {
                    let mut b = buf.borrow_mut();
                    b.push_str("\n$ ");
                    ui.set_terminal_output(b.clone().into());
                    return slint::private_unstable_api::re_exports::EventResult::Accept;
                }

                // `clear` and `exit` are terminal-local; everything else, including `cd`,
                // goes through the one shared executor the `run` action also uses.
                if cmd_str.trim() == "clear" {
                    *buf.borrow_mut() = "$ ".to_string();
                    ui.set_terminal_output("$ ".into());
                    return slint::private_unstable_api::re_exports::EventResult::Accept;
                }
                if cmd_str.trim() == "exit" {
                    ui.set_is_alive(false);
                    return slint::private_unstable_api::re_exports::EventResult::Accept;
                }
                exec_command(&cmd_str, &ui, &cwd_ref, &buf, &last_run);
                return slint::private_unstable_api::re_exports::EventResult::Accept;
            }

            // Backspace
            if text == "\u{8}" || text == "\u{7f}" {
                let mut line = input_line.borrow_mut();
                if !line.is_empty() {
                    line.pop();
                    let mut b = buf.borrow_mut();
                    b.pop();
                    ui.set_terminal_output(b.clone().into());
                }
                return slint::private_unstable_api::re_exports::EventResult::Accept;
            }

            // Regular character
            if !text.is_empty() && text.chars().all(|c| !c.is_control()) {
                input_line.borrow_mut().push_str(&text);
                let mut b = buf.borrow_mut();
                b.push_str(&text);
                ui.set_terminal_output(b.clone().into());
                return slint::private_unstable_api::re_exports::EventResult::Accept;
            }

            slint::private_unstable_api::re_exports::EventResult::Reject
        });
    }

    publish_control(app, output_buffer.clone(), last_command.clone(), cwd.clone());

    // Tab management stubs
    app.on_new_tab(|| { tracing::info!("New tab requested (standalone mode — single tab only)"); });
    app.on_close_tab(|_| { tracing::info!("Close tab requested (standalone mode)"); });
    app.on_switch_tab(|_| {});

    // AI stubs
    app.on_request_ai_help(|| { tracing::info!("AI help requested (standalone mode)"); });
    app.on_dismiss_suggestion(|| {});
    app.on_ai_bar_submit(|_| { tracing::info!("AI bar submit (standalone mode)"); });
    app.on_ai_run_command(|| {});
    app.on_accept_ghost(|| {});

    // Search stubs
    app.on_search_query_changed(|_| {});
    app.on_search_next(|| {});
    app.on_search_prev(|| {});

    // Split pane stubs
    app.on_terminal_split_toggle(|| { tracing::info!("Split toggle (standalone mode)"); });
    app.on_terminal_switch_pane(|_| {});
    app.on_terminal_split_input(|_| {});

    // Profile stubs
    app.on_terminal_set_profile(|_| {});

    // Other stubs
    app.on_restart_terminal(|| { tracing::info!("Restart terminal (standalone mode)"); });
    app.on_danger_proceed(|| {});
    app.on_danger_cancel(|| {});
    app.on_explain_line(|_| {});
    app.on_terminal_area_resized(|_w, _h| {});
}
