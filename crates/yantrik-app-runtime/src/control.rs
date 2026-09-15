//! `app.describe` / `app.act` — how an app tells the mind what it holds, and takes instruction.
//!
//! # Why this exists
//!
//! The companion could already see the desktop, in the only way it had: `grim` takes a
//! screenshot, the PNG is base64'd and posted to a vision model, and the model reports what the
//! pixels look like. That is the right answer for a foreign application — we did not write
//! Firefox and it owes us no account of itself. It is the wrong answer for our own software.
//! Every one of the sixteen apps under `apps/` already knows exactly which note is open, which
//! track is playing, which message is selected; asking a vision model to *infer* that from a
//! photograph of our own window is expensive, slow, lossy, and needs a GPU we do not always have.
//!
//! So an app publishes its state instead. Two methods on the socket bus every app already
//! speaks:
//!
//! ```text
//! app.describe {}                                  → { app, summary, state, revision, actions }
//! app.act      { action, args, expect_revision? }  → { accepted, action_id, settled, result,
//!                                                      revision, summary, state }
//! ```
//!
//! `describe` is a few hundred bytes of exact truth, always current. `act` is the same surface
//! turned around: the actions an app already exposes to its own buttons, offered to the mind by
//! name — so driving our own software never needs a synthetic mouse click either.
//!
//! The rule this establishes: **semantic for ours, visual for theirs.**
//!
//! # Accepted is not done
//!
//! `act` never answers with a bare success, because there is no honest way to read one. Three
//! different things could be meant by "it worked":
//!
//! 1. **accepted** — the guard passed and the handler ran;
//! 2. **state changed** — the app now reports the intended result;
//! 3. **presented** — that result reached a frame someone could see.
//!
//! An action that opens a note settles all three before the handler returns. An action that
//! starts a build settles only the first: the compiler does not exist yet. A caller that cannot
//! tell those apart will report a build as finished the instant it was started — so the response
//! says `accepted`, carries `settled`, and an action that merely schedules work declares itself
//! with [`Action::defers`] rather than leaving the caller to guess.
//!
//! # Compare and act, in one turn of the event loop
//!
//! `expect_revision` is the other half. A caller that reads state, decides, and then acts has a
//! gap in between in which the person at the keyboard can type, close the document or switch
//! windows — and the action lands on a world that no longer matches the reason for it. So the
//! comparison happens *inside* the same closure as the dispatch, on the UI thread, which is the
//! app's own serialization domain: between the check and the handler, nothing else can run.
//!
//! A revision from an earlier `describe` is a hint about whether to bother. `expect_revision` is
//! the guard. Only the guard is atomic, and a caller that compares revisions itself and then
//! calls `act` has rebuilt exactly the race this removes.
//!
//! # Threading
//!
//! Both closures run on the UI thread, because that is the only thread allowed to touch a Slint
//! window. The RPC server runs on its own thread and hands work across with
//! [`slint::invoke_from_event_loop`], then blocks on a channel for the answer. That is a
//! deliberate trade: a `describe` that reads a handful of properties costs microseconds, and
//! taking the answer from the live UI is the whole point — a cached copy would be exactly the
//! stale second-hand account this module exists to replace.
//!
//! An `act` that would take real time must still not run inline; do what the app's own button
//! does and hand off to a worker.
//!
//! # Usage
//!
//! ```rust,ignore
//! use yantrik_app_runtime::control::{self, Action, Param, View};
//!
//! control::App::new("notes")
//!     .describe({
//!         let ui = app.as_weak();
//!         move || {
//!             let ui = ui.unwrap();
//!             View::new(format!("Notes — {}", ui.get_current_title()))
//!                 .with("open_note", ui.get_current_title().to_string())
//!                 .with("unsaved", ui.get_is_modified())
//!         }
//!     })
//!     .action(
//!         Action::new("open_note", "Open a note by title").arg(Param::text("title")),
//!         move |args| { /* … */ Ok(serde_json::json!({ "opened": true })) },
//!     )
//!     .serve();
//! ```

use std::cell::RefCell;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use yantrik_ipc_contracts::email::ServiceError;
use yantrik_ipc_transport::server::{RpcServer, ServiceHandler};

/// How long the RPC thread waits for the UI thread to answer.
///
/// A `describe` reading Slint properties is effectively instant. This budget exists for the case
/// where the UI thread is genuinely stuck — a modal, a long paint, a blocking call someone should
/// not have made — and the caller deserves a timeout it can report rather than a hang.
const UI_ROUNDTRIP: Duration = Duration::from_secs(3);

/// A name for one dispatch, so anything waiting on its effects can say which one it is waiting on.
///
/// Scoped to the app and monotonic within a run. Not a UUID: it is read by people in logs and
/// compared by machines within a single session, and `app-notes#7` does both better than
/// thirty-two hex digits would.
fn next_action_id(service_id: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("{service_id}#{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

/// Service ids are prefixed so an app cannot collide with the service of the same name.
///
/// `notes` is already taken by notes-service, which stores notes; `app-notes` is the window a
/// person is looking at. They are different things and must not share a socket.
pub fn service_id_for(app_id: &str) -> String {
    format!("app-{app_id}")
}

// ── What an app reports ─────────────────────────────────────────────
//
// The vocabulary itself — `View`, `Param`, `Action`, the action JSON schema and the
// revision hash — is pure data with no tie to Slint, and a standalone service must be able
// to build the identical envelope without pulling this runtime in. So it lives in
// `yantrik-ipc-contracts::control_surface`, and this module re-exports it: every existing
// `control::View` / `control::Action` / `control::Param` caller is unchanged, and the shell
// window and a headless service now share one definition of what an app is.
pub use yantrik_ipc_contracts::control_surface::{act_json, describe_json, Action, Param, View};

// ── The registry, which lives on the UI thread ──────────────────────

type DescribeFn = Box<dyn Fn() -> View>;
type ActFn = Box<dyn Fn(&serde_json::Value) -> Result<serde_json::Value, String>>;

struct Registry {
    app_id: String,
    describe: Option<DescribeFn>,
    actions: Vec<(Action, ActFn)>,
}

/// What an app reports about itself right now: the view, and its fingerprint.
struct Snapshot {
    summary: String,
    state: serde_json::Value,
    revision: String,
}

impl Registry {
    /// Read the live view once. Every caller below goes through this, so a revision is never
    /// computed from a different read than the state it is reported beside.
    fn snapshot(&self) -> Snapshot {
        let view = match &self.describe {
            Some(f) => f(),
            None => View::new(format!("{} (no description published)", self.app_id)),
        };
        let revision = view.revision();
        Snapshot { summary: view.summary, state: view.state, revision }
    }

    fn describe(&self) -> serde_json::Value {
        let now = self.snapshot();
        let specs: Vec<Action> = self.actions.iter().map(|(a, _)| a.clone()).collect();
        let view = View { summary: now.summary, state: now.state };
        describe_json(&self.app_id, &view, &specs)
    }

    /// Check the guard, dispatch, and read what came of it — without leaving the UI thread.
    ///
    /// The three steps are one function because they have to be one turn of the event loop. Split
    /// across RPC calls, the gap between the check and the dispatch is a window in which the user
    /// can type, and the gap between the dispatch and the read is a window in which they can undo
    /// it. Here nothing runs in between, because there is no in between: this is the thread that
    /// would have to run it.
    fn act(
        &self,
        name: &str,
        args: &serde_json::Value,
        expect_revision: Option<&str>,
        action_id: &str,
    ) -> Result<serde_json::Value, String> {
        let Some((spec, run)) = self.actions.iter().find(|(a, _)| a.name == name) else {
            let known: Vec<&str> = self.actions.iter().map(|(a, _)| a.name.as_str()).collect();
            return Err(format!("unknown action `{name}`; this app offers: {}", known.join(", ")));
        };
        // Checked here rather than in every handler: a missing argument is the most common way a
        // model gets a call wrong, and the error should name the argument, not panic in the app.
        for p in spec.params.iter().filter(|p| p.required) {
            if args.get(&p.name).is_none() {
                return Err(format!("`{name}` needs argument `{}`", p.name));
            }
        }

        // The mirror of the check above, and the omission that actually bit: an argument the
        // action does not declare used to be dropped in silence. `new_note title='Handover'`
        // answered accepted:true and wrote a note called "Untitled" — the caller was told its
        // instruction had landed when nothing had read it. Refusing names the mistake and costs
        // one retry; accepting it hides the mistake and costs the whole task.
        if let Some(given) = args.as_object() {
            for key in given.keys() {
                if spec.params.iter().any(|p| &p.name == key) {
                    continue;
                }
                let known: Vec<&str> = spec.params.iter().map(|p| p.name.as_str()).collect();
                return Err(if known.is_empty() {
                    format!("`{name}` takes no arguments, but `{key}` was given")
                } else {
                    format!("`{name}` has no argument `{key}`; it takes: {}", known.join(", "))
                });
            }
        }

        // The guard. A caller that read state, decided, and asked for this action gets to say what
        // it was looking at; if the app has moved on, the action does not happen. Refusing is
        // cheap and correctable — acting on a stale premise is neither.
        if let Some(expected) = expect_revision {
            let before = self.snapshot();
            if before.revision != expected {
                return Err(format!(
                    "STALE: this app is at revision {} and you acted on {expected}. \
                     It now reports: {}. Read it again before deciding.",
                    before.revision, before.summary
                ));
            }
        }

        let result = run(args)?;

        // Read back through the same path a `describe` would take, so a caller never has to make
        // a second round trip to find out what its own action did.
        let after = self.snapshot();
        // Through the same helper a service uses, so a window action and a service action are
        // indistinguishable by shape. `accepted` says the handler ran; `settled` (from the
        // action's own `deferred`) says whether the work finished — never `ok`, never `done`.
        let view = View { summary: after.summary, state: after.state };
        Ok(act_json(&self.app_id, action_id, !spec.deferred, result, &view))
    }
}

thread_local! {
    /// Installed by [`App::serve`] on the thread that owns the window.
    static REGISTRY: RefCell<Option<Registry>> = const { RefCell::new(None) };
}

/// Ask the UI thread to run `job` and wait for its answer.
///
/// Returns `Err` when the event loop is not running or is too busy to answer inside
/// [`UI_ROUNDTRIP`] — both of which the caller should see as an error rather than a hang.
fn on_ui_thread<T, F>(job: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&Registry) -> T + Send + 'static,
{
    let (tx, rx) = mpsc::sync_channel::<Result<T, String>>(1);
    slint::invoke_from_event_loop(move || {
        let answer = REGISTRY.with(|cell| match cell.borrow().as_ref() {
            Some(reg) => Ok(job(reg)),
            None => Err("this app published no control surface".to_string()),
        });
        // The receiver is gone only if we already timed out; dropping the answer is correct.
        let _ = tx.send(answer);
    })
    .map_err(|e| format!("app is not accepting requests: {e}"))?;

    rx.recv_timeout(UI_ROUNDTRIP)
        .map_err(|_| format!("app did not answer within {}s", UI_ROUNDTRIP.as_secs()))?
}

// ── The RPC surface ─────────────────────────────────────────────────

struct ControlRpc {
    service_id: String,
}

impl ServiceHandler for ControlRpc {
    fn service_id(&self) -> &str {
        &self.service_id
    }

    fn handle(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, ServiceError> {
        match method {
            "app.describe" => on_ui_thread(|reg| reg.describe())
                .map_err(|m| ServiceError { code: -32000, message: m }),

            "app.act" => {
                let action = params
                    .get("action")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if action.is_empty() {
                    return Err(ServiceError {
                        code: -32602,
                        message: "act needs a non-empty `action`".into(),
                    });
                }
                let args = params.get("args").cloned().unwrap_or(serde_json::json!({}));
                // Optional, and deliberately so: a caller acting on its own initiative has nothing
                // to compare against, and demanding a revision it never read would only teach it
                // to send back whatever it last saw.
                let expect = params
                    .get("expect_revision")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let action_id = next_action_id(&self.service_id);

                tracing::info!(action = %action, id = %action_id, "app.act");
                let id = action_id.clone();
                let outcome =
                    on_ui_thread(move |reg| reg.act(&action, &args, expect.as_deref(), &id))
                        .map_err(|m| ServiceError { code: -32000, message: m })?;

                match outcome {
                    Ok(answer) => Ok(answer),
                    // An action that legitimately refuses is an application error, not a
                    // transport failure: -32602 keeps it out of the client's circuit breaker.
                    Err(message) => Err(ServiceError { code: -32602, message }),
                }
            }

            other => Err(ServiceError {
                code: -32601,
                message: format!("unknown method `{other}`; this app serves app.describe, app.act"),
            }),
        }
    }
}

// ── Building one ────────────────────────────────────────────────────

/// An app's control surface, under construction.
///
/// Build it on the UI thread and finish with [`App::serve`].
pub struct App {
    registry: Registry,
}

impl App {
    pub fn new(app_id: &str) -> Self {
        Self { registry: Registry { app_id: app_id.into(), describe: None, actions: Vec::new() } }
    }

    /// What this app reports when asked. Runs on the UI thread; keep it cheap.
    pub fn describe(mut self, f: impl Fn() -> View + 'static) -> Self {
        self.registry.describe = Some(Box::new(f));
        self
    }

    /// One thing this app can be asked to do. Runs on the UI thread.
    pub fn action(
        mut self,
        spec: Action,
        f: impl Fn(&serde_json::Value) -> Result<serde_json::Value, String> + 'static,
    ) -> Self {
        self.registry.actions.push((spec, Box::new(f)));
        self
    }

    /// Publish this surface on the socket bus.
    ///
    /// Must be called from the thread that owns the window, before `run()`. Failing to serve is
    /// not fatal: an app whose socket cannot be bound is still a working app, it is only invisible
    /// to the mind, and taking the window down over that would be the worse outcome.
    pub fn serve(self) {
        let app_id = self.registry.app_id.clone();
        let action_count = self.registry.actions.len();
        REGISTRY.with(|cell| *cell.borrow_mut() = Some(self.registry));

        let service_id = service_id_for(&app_id);
        std::thread::Builder::new()
            .name(format!("{service_id}-rpc"))
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        tracing::warn!(error = %e, "No runtime; app is not describable");
                        return;
                    }
                };
                runtime.block_on(async {
                    let address = RpcServer::default_address(&service_id);
                    tracing::info!(
                        address = %address,
                        actions = action_count,
                        "Control surface listening (app.describe / app.act)"
                    );
                    if let Err(e) = RpcServer::new(&address)
                        .serve(Arc::new(ControlRpc { service_id: service_id.clone() }))
                        .await
                    {
                        tracing::warn!(error = %e, "Control surface stopped");
                    }
                });
            })
            .ok();
    }
}

// ── Finding the others ──────────────────────────────────────────────

/// The app ids that currently have a control socket in this session.
///
/// A socket file outlives a crashed process, so this is a list of candidates, not of live apps —
/// callers should treat a failed `app.describe` as "gone" rather than as an error worth reporting.
#[cfg(unix)]
pub fn running_apps() -> Vec<String> {
    let dir = yantrik_ipc_transport::server::socket_dir();
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut ids: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|name| {
            name.strip_suffix(".sock")
                .and_then(|stem| stem.strip_prefix("app-"))
                .map(|id| id.to_string())
        })
        .collect();
    ids.sort();
    ids
}

#[cfg(not(unix))]
pub fn running_apps() -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_ids_do_not_collide_with_services() {
        // notes-service owns `notes`; the Notes window must not bind the same socket.
        assert_eq!(service_id_for("notes"), "app-notes");
        assert_ne!(service_id_for("notes"), "notes");
    }

    #[test]
    fn a_view_builds_an_object() {
        let v = View::new("Notes — 3 open").with("count", 3).with("unsaved", true);
        assert_eq!(v.summary, "Notes — 3 open");
        assert_eq!(v.state["count"], 3);
        assert_eq!(v.state["unsaved"], true);
    }

    /// A registry with one action over a state the test can move underneath it.
    fn notes_at(title: &'static str) -> Registry {
        Registry {
            app_id: "notes".into(),
            describe: Some(Box::new(move || {
                View::new(format!("Notes \u{2014} {title}")).with("open_note", title)
            })),
            actions: vec![(
                Action::new("rename", "Rename the open note").arg(Param::text("to")),
                Box::new(|args| Ok(serde_json::json!({ "renamed_to": args["to"].clone() }))),
            )],
        }
    }

    #[test]
    fn an_action_becomes_json_schema() {
        let schema = Action::new("open_note", "Open a note by title")
            .arg(Param::text("title").describe("The note's title"))
            .arg(Param::flag("focus").optional())
            .schema();

        assert_eq!(schema["name"], "open_note");
        // Unstated risk is `standard`: steering someone's window is never free.
        assert_eq!(schema["permission"], "standard");
        assert_eq!(schema["parameters"]["properties"]["title"]["type"], "string");
        assert_eq!(schema["parameters"]["properties"]["focus"]["type"], "boolean");
        // Only the required argument is listed as required.
        assert_eq!(schema["parameters"]["required"], serde_json::json!(["title"]));
    }

    #[test]
    fn an_action_can_declare_itself_dangerous() {
        let schema = Action::new("kill_process", "End a process").risk("dangerous").schema();
        assert_eq!(schema["permission"], "dangerous");
    }

    #[test]
    fn a_missing_argument_is_named_not_guessed() {
        let reg = Registry {
            app_id: "notes".into(),
            describe: None,
            actions: vec![(
                Action::new("open_note", "Open a note").arg(Param::text("title")),
                Box::new(|_| Ok(serde_json::json!("never reached"))),
            )],
        };

        let err = reg.act("open_note", &serde_json::json!({}), None, "t#1").unwrap_err();
        assert!(err.contains("title"), "the error must name the missing argument: {err}");
    }

    #[test]
    fn an_argument_the_action_never_declared_is_refused_not_dropped() {
        let reg = Registry {
            app_id: "notes".into(),
            describe: None,
            actions: vec![(
                Action::new("open_note", "Open a note").arg(Param::text("title")),
                Box::new(|_| Ok(serde_json::json!("ran"))),
            )],
        };

        // The real call that exposed this: a title was passed to an action that does not take
        // one, the argument was dropped, and the caller was told the action succeeded.
        let err = reg
            .act("open_note", &serde_json::json!({"title": "a", "colour": "red"}), None, "t#1")
            .unwrap_err();
        assert!(err.contains("colour"), "the error must name the argument it did not know: {err}");
        assert!(err.contains("title"), "and list what it does take: {err}");
    }

    #[test]
    fn an_action_that_takes_nothing_says_so_rather_than_ignoring_you() {
        let reg = Registry {
            app_id: "notes".into(),
            describe: None,
            actions: vec![(
                Action::new("new_note", "Start a new note"),
                Box::new(|_| Ok(serde_json::json!({"title": "Untitled"}))),
            )],
        };

        // This is verbatim the call made on the deployed VM. It used to answer accepted:true
        // and write a note called "Untitled".
        let err = reg
            .act("new_note", &serde_json::json!({"title": "Handover"}), None, "t#1")
            .unwrap_err();
        assert!(err.contains("takes no arguments"), "{err}");
        assert!(err.contains("title"), "{err}");

        // And the no-argument call it was always meant to accept still works.
        assert!(reg.act("new_note", &serde_json::json!({}), None, "t#2").is_ok());
    }

    #[test]
    fn an_unknown_action_lists_the_real_ones() {
        let reg = Registry {
            app_id: "notes".into(),
            describe: None,
            actions: vec![(
                Action::new("open_note", "Open a note"),
                Box::new(|_| Ok(serde_json::Value::Null)),
            )],
        };

        let err = reg.act("nope", &serde_json::json!({}), None, "t#1").unwrap_err();
        assert!(err.contains("open_note"), "a wrong guess should be correctable: {err}");
    }

    #[test]
    fn describe_falls_back_when_the_app_published_nothing() {
        let reg = Registry { app_id: "notes".into(), describe: None, actions: Vec::new() };
        let out = reg.describe();
        assert_eq!(out["app"], "notes");
        assert_eq!(out["actions"], serde_json::json!([]));
    }

    // ── The fingerprint ──

    #[test]
    fn the_same_view_fingerprints_the_same_and_a_changed_one_does_not() {
        let a = View::new("Notes \u{2014} Kernel asks").with("words", 412).with("unsaved", true);
        let same = View::new("Notes \u{2014} Kernel asks").with("words", 412).with("unsaved", true);
        assert_eq!(a.revision(), same.revision());

        // One word typed is a different state, and has to be a different revision — otherwise a
        // guard built on it would wave through an action decided before the typing.
        let typed = View::new("Notes \u{2014} Kernel asks").with("words", 413).with("unsaved", true);
        assert_ne!(a.revision(), typed.revision());

        // And so is a different summary over identical state.
        let renamed = View::new("Notes \u{2014} Kernel answers").with("words", 412).with("unsaved", true);
        assert_ne!(a.revision(), renamed.revision());
    }

    #[test]
    fn the_order_fields_were_added_in_does_not_change_the_revision() {
        // Two `describe` implementations of the same state must agree, or a guard would fire on a
        // refactor that changed nothing a person could see.
        let one = View::new("Notes").with("words", 412).with("unsaved", true);
        let other = View::new("Notes").with("unsaved", true).with("words", 412);
        assert_eq!(one.revision(), other.revision());
    }

    // ── Accepted is not done ──

    #[test]
    fn acting_never_answers_with_a_bare_success() {
        let answer = notes_at("Kernel asks")
            .act("rename", &serde_json::json!({ "to": "Kernel answers" }), None, "notes#1")
            .unwrap();

        // The three things a caller has to be able to tell apart.
        assert_eq!(answer["accepted"], true);
        assert_eq!(answer["action_id"], "notes#1");
        assert_eq!(answer["settled"], true);
        assert_eq!(answer["result"]["renamed_to"], "Kernel answers");
        // And the state afterwards, so nobody has to make a second call to find out what they did.
        assert!(answer["summary"].as_str().unwrap().contains("Kernel asks"));
        assert!(answer["revision"].as_str().is_some_and(|r| r.len() == 16));
    }

    #[test]
    fn an_action_that_only_starts_the_work_says_so() {
        // The build case. Returning `accepted: true` with nothing else would let a caller report a
        // compile as finished the instant it was started.
        let reg = Registry {
            app_id: "builder".into(),
            describe: None,
            actions: vec![(
                Action::new("build", "Start a build").defers(),
                Box::new(|_| Ok(serde_json::json!({ "job": 83 }))),
            )],
        };

        let answer = reg.act("build", &serde_json::json!({}), None, "builder#1").unwrap();
        assert_eq!(answer["accepted"], true);
        assert_eq!(answer["settled"], false, "a dispatched build has not built anything yet");

        // And the schema says it in advance, so a caller can plan to watch rather than discover
        // afterwards that it has to.
        let schema = Action::new("build", "Start a build").defers().schema();
        assert_eq!(schema["settles"], "later");
        assert_eq!(Action::new("open_note", "Open").schema()["settles"], "on return");
    }

    // ── The guard ──

    #[test]
    fn an_action_decided_on_a_state_the_app_has_left_is_refused() {
        // The race this exists for. A caller reads "Kernel asks", decides to rename it, and by the
        // time the call lands the user has opened something else. Renaming now renames the wrong
        // note, and the caller would report success.
        let stale = View::new("Notes \u{2014} Kernel asks").with("open_note", "Kernel asks").revision();

        let err = notes_at("Shopping list")
            .act("rename", &serde_json::json!({ "to": "x" }), Some(&stale), "notes#1")
            .unwrap_err();

        assert!(err.starts_with("STALE:"), "a caller has to be able to branch on this: {err}");
        assert!(err.contains(&stale), "the refusal names what was expected: {err}");
        assert!(
            err.contains("Shopping list"),
            "and what is actually there, so the next read is not blind: {err}"
        );
    }

    #[test]
    fn a_guard_that_matches_lets_the_action_through() {
        let current = View::new("Notes \u{2014} Kernel asks").with("open_note", "Kernel asks").revision();

        let answer = notes_at("Kernel asks")
            .act("rename", &serde_json::json!({ "to": "ok" }), Some(&current), "notes#1")
            .unwrap();
        assert_eq!(answer["accepted"], true);
        assert_eq!(answer["result"]["renamed_to"], "ok");
    }

    #[test]
    fn an_action_with_no_guard_still_runs() {
        // Most calls are the caller's own initiative and have nothing to compare against.
        // Requiring a revision would only teach callers to echo back whatever they last saw,
        // which is a guard that always passes.
        let answer = notes_at("Kernel asks")
            .act("rename", &serde_json::json!({ "to": "ok" }), None, "notes#1")
            .unwrap();
        assert_eq!(answer["accepted"], true);
    }

    #[test]
    fn the_guard_is_checked_before_the_arguments_are_used() {
        // Ordering that matters: a stale guard must refuse without the handler having run. If the
        // rename happened and *then* we noticed the state had moved, the refusal would be a lie.
        let ran = std::rc::Rc::new(std::cell::Cell::new(false));
        let reg = Registry {
            app_id: "notes".into(),
            describe: Some(Box::new(|| View::new("Notes \u{2014} now"))),
            actions: vec![(Action::new("go", "Go"), {
                let ran = ran.clone();
                Box::new(move |_| {
                    ran.set(true);
                    Ok(serde_json::Value::Null)
                })
            })],
        };

        let err = reg.act("go", &serde_json::json!({}), Some("0000000000000000"), "n#1");
        assert!(err.is_err());
        assert!(!ran.get(), "the handler must not have run");
    }

    #[test]
    fn every_dispatch_gets_its_own_name() {
        let first = next_action_id("app-notes");
        let second = next_action_id("app-notes");
        assert_ne!(first, second, "two waits must not key on the same id");
        assert!(first.starts_with("app-notes#"), "{first}");
    }
}
