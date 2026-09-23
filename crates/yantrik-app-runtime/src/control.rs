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
//! # The ceiling
//!
//! Every action carries a grade — `safe < standard < sensitive < dangerous` — and the machine has
//! a ceiling for programmatic callers, `tool_permission` in the shell's `settings.yaml`. The
//! comparison used to happen only in the MCP bridge, which meant the OS's one real boundary was
//! enforced by one of its callers: `os_act` refused a `dangerous` action while `yos act` — and
//! every mind's own shell tool, and anything else that could open the socket — ran the same
//! action untouched. The check lives here now, in the dispatch every `app.act` crosses regardless
//! of who sent it, and the bridge keeps its copy as defence in depth and for the better message.
//!
//! A person at the keyboard is deliberately not a "programmatic caller". The shell's own buttons
//! invoke the same callbacks the action handlers invoke, but they never pass through this module —
//! there is no socket, no `app.act`, no dispatch. The ceiling binds the door minds come in by,
//! not the window the person is sitting at, and no caller-identity scheme is needed to say so,
//! because the two paths do not meet. (Who exactly *is* on the socket is issue #43; until then
//! every socket caller gets the one machine-wide ceiling.)
//!
//! # The mode, and the grant
//!
//! Under the ceiling the person has a *mode* — plan, ask, auto or bypass — that says what may run
//! without asking them. For a while that lived only in the MCP bridge: it read the mode, raised
//! the approval card when the mode said to, and acted once the person pressed Allow. `yos act`
//! and a raw client on the socket ran the same `sensitive` action in `ask` mode with no card and
//! no record (issues #49 and #116). Now the dispatch reads the mode the way it reads the ceiling
//! — the shell publishes it beside `settings.yaml` — and a call above what the mode allows must
//! carry a **grant**: the `request_id` that `request_approval` minted and a person's Allow turned
//! into one, which the dispatch spends through the shell before the handler runs — once the
//! ceiling has passed, so an Allow is never used up on an act the ceiling then refuses (#154).
//! Every door meets the same question; `yos act` and the bridge ask for the card on the caller's
//! behalf. `describe` needs nothing, and the ceiling stays above every mode and every grant.
//!
//! The rule itself is `yantrik_ipc_transport::gate`, re-exported below. Three services answer
//! `app.act` in their own handlers rather than through this dispatch — System Monitor, whose
//! `kill_process` is `dangerous`, Notifications and Weather — and until #153 they met none of it.
//! They call the same `gate::permit` now, with the grades from the tables they publish, and
//! refuse in the same words.
//!
//! One line the dispatch does not draw: plan mode's refusal of `standard`. The desktop's own
//! processes cross this socket with `standard` calls — an app asking the shell to `start_service`
//! the service it needs, a second launch handing its file to the open window — and until callers
//! carry an identity (#43) the dispatch cannot tell those from a mind. So every mode runs
//! `standard` here ([`SOCKET_FLOOR`]); plan's refusal of it stays the bridge's, as it always was,
//! and plan still refuses everything above it on every door, because the shell raises no card in
//! plan and so no grant can exist.
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
//! When the caller is owed the *result* of that time — a command's exit code, not "started" — the
//! handler hands the rest of its answer to [`answer_later`]: the handler returns at once and the UI
//! thread moves on, the work runs on the RPC side, and the caller's reply is its result. The RPC
//! side is a multi-threaded runtime that steps the waiting call out of the way
//! (`block_in_place`), so one caller waiting two minutes does not hold up every other caller of the
//! same socket.
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
use yantrik_ipc_transport::server::{PeerCred, RpcServer, ServiceHandler};

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

// ── The names one app answers to ─────────────────────────────────────

/// Every app that publishes a control surface, and the other names it is known by.
///
/// An app has up to three names and they are not always the same word: the id it publishes here
/// (`containers`), the program in `/opt/yantrik/bin` (`yantrik-container-manager`), and the
/// launcher's word for its tile (`containers`, but `sysmonitor` for System Monitor). Which of
/// them a caller happens to be holding decided whether it could describe the app at all:
///
/// ```text
/// yos ls                          → app-containers
/// yos describe container-manager  → "no socket for 'container-manager'"
/// ```
///
/// The app is `container-manager` in `/opt/yantrik/bin`, in the launcher's route table and in
/// `open_app`; only its socket was `containers`. A mind that found the app by the name everything
/// else calls it, and then asked it to describe itself, was refused — and had no way to learn
/// better from the refusal.
///
/// So the other names are written down once, here, and [`App::serve`] links each of them at the
/// socket the app binds. The ids are the apps' own and are not changed by this: the id an app
/// publishes is still what `describe` reports and what `yos ls` lists. What changes is that the
/// other names reach it.
///
/// Hyphens, because that is how an app publishes its own id (`download-manager`). [`fold`] makes
/// `container_manager`, `Container Manager` and `container-manager` one question, so a caller's
/// punctuation is not part of the name.
///
/// An app with no other name still belongs in this table: it is what makes the table a complete
/// answer to "is this a surface of this desktop", which is what the shell's route table is
/// checked against (`every_launchable_name_reaches_a_surface` in `wire::dock`). Adding a route
/// spelling without a name here fails that test rather than shipping another refusal.
const SURFACES: &[(&str, &[&str])] = &[
    ("arcade", &[]),
    // Not one of ours: Blender is a program this desktop opens, and its surface is served by a
    // Python addon inside it (apps/blender/addon), a port of this module's dispatch rather than
    // a user of this runtime. It belongs in the table for the same reason as any other row —
    // the table is the complete answer to "is this a surface of this desktop".
    ("blender", &[]),
    ("calendar", &[]),
    ("containers", &["container-manager"]),
    ("documents", &["document-editor"]),
    ("download-manager", &["downloads"]),
    ("editor", &["text-editor"]),
    ("email", &[]),
    // Both, because the launcher now opens it under `image` and a caller holding the older
    // `images` must still reach the same surface.
    ("image-viewer", &["images", "image"]),
    ("network", &["network-manager"]),
    ("notes", &[]),
    ("presentation", &["slides"]),
    // Nothing "opens" the desktop, so it is in no launcher table; it does publish a surface, and
    // its own notifications' buttons and approval cards have to reach it.
    ("shell", &[]),
    ("snippets", &["snippet-manager"]),
    // No other name: "images" is the image viewer's alias, and inventing a second one for the app
    // that makes them would be a guess about how people will ask.
    ("studio", &[]),
    ("system-monitor", &["sysmonitor"]),
    ("terminal", &[]),
    ("weather", &[]),
];

/// One spelling of a name, so the separator a caller arrived with is not part of the question.
fn fold(name: &str) -> String {
    name.trim().to_lowercase().replace([' ', '_'], "-")
}

/// The id whose control surface answers to `name`, whichever of the app's names that is.
///
/// `surface_id("container-manager")`, `surface_id("Container Manager")` and
/// `surface_id("containers")` are all `containers` — the id the socket is bound under. `None`
/// means no app of this desktop answers to that name at all, which is a different thing from an
/// app that is closed.
pub fn surface_id(name: &str) -> Option<&'static str> {
    let key = fold(name);
    SURFACES.iter().find_map(|(id, others)| {
        (*id == key || others.contains(&key.as_str())).then_some(*id)
    })
}

/// The other names `app_id`'s surface answers to. Empty for an app with one name, and for a name
/// that is not this desktop's.
pub fn other_names(app_id: &str) -> &'static [&'static str] {
    let key = fold(app_id);
    SURFACES
        .iter()
        .find(|(id, _)| *id == key)
        .map(|(_, others)| *others)
        .unwrap_or(&[])
}

// ── The ceiling, the mode and the grant ─────────────────────────────
//
// Every action carries a grade; the machine has a ceiling (`tool_permission`), the person has a
// mode, and a call above what the mode runs unasked must carry a grant — a person's Allow, spent
// through the shell. That rule lives in `yantrik_ipc_transport::gate` since issue #153: three
// services answer `app.act` in their own handlers, never crossed this dispatch, and so never met
// it — System Monitor's `dangerous` `kill_process` ran on any call to its socket. A service must
// not link Slint to be told no, so the rule moved below this crate, beside the socket client it
// needs for spending a grant, and is re-exported here unchanged: `control::configured_ceiling`,
// `control::mode_from`, `control::spend_grants_with` and the rest are what they were.
//
// What stays here is the half only a window has. Its grades live on the UI thread and file and
// socket IO does not, so the RPC thread reads the files and spends any grant (see
// `ControlRpc::dispatch`), and `Registry::act` decides with `gate::decide` inside the same turn
// of the event loop as the handler.
pub use yantrik_ipc_transport::gate::{
    configured_ceiling, configured_mode, decide, grant_of, mode_from, mode_path, permit,
    spend_grants_with, Authority, Mode, AGENT_TOKEN, DEFAULT_MODE, LADDER, MODES, MODE_FILE,
    SOCKET_FLOOR,
};
use yantrik_ipc_transport::gate::{agent_token_of, grade};
#[cfg(test)]
use yantrik_ipc_transport::gate::{ceiling_from, DEFAULT_CEILING};

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
        let specs: Vec<Action> = self
            .actions
            .iter()
            .map(|(a, _)| {
                let mut spec = a.clone();
                spec.permission = effective_grade(&spec.name, spec.permission);
                spec
            })
            .collect();
        let view = View { summary: now.summary, state: now.state };
        describe_json(&self.app_id, &view, &specs)
    }

    /// The grade this surface publishes for `name` right now — regrades included — or the
    /// refusal an action it does not have gets.
    ///
    /// The RPC thread asks for this before it spends a grant, so a grant is only ever spent on an
    /// act whose grade the ceiling allows (#154). `act` reads the grade the same way.
    fn grade_of(&self, name: &str) -> Result<&'static str, String> {
        self.actions
            .iter()
            .find(|(a, _)| a.name == name)
            .map(|(a, _)| effective_grade(&a.name, a.permission))
            .ok_or_else(|| self.unknown(name))
    }

    fn unknown(&self, name: &str) -> String {
        let known: Vec<&str> = self.actions.iter().map(|(a, _)| a.name.as_str()).collect();
        format!("unknown action `{name}`; this app offers: {}", known.join(", "))
    }

    /// Check the ceiling and the mode, check the guard, dispatch, and read what came of it —
    /// without leaving the UI thread.
    ///
    /// These steps are one function because they have to be one turn of the event loop. Split
    /// across RPC calls, the gap between the check and the dispatch is a window in which the user
    /// can type, and the gap between the dispatch and the read is a window in which they can undo
    /// it. Here nothing runs in between, because there is no in between: this is the thread that
    /// would have to run it.
    ///
    /// `authority` arrives as an argument — the ceiling and the mode already read from their
    /// files by the RPC thread, and any grant already spent there (see `ControlRpc::dispatch`) —
    /// so the boundary is enforced in the dispatch itself, the one function every `app.act` to a
    /// window crosses, whoever sent it, while the IO stays off the UI thread and tests can pin the
    /// ceiling and the mode instead of inheriting the developer's.
    fn act(
        &self,
        name: &str,
        args: &serde_json::Value,
        expect_revision: Option<&str>,
        action_id: &str,
        authority: &Authority,
    ) -> Result<serde_json::Value, String> {
        let Some((spec, run)) = self.actions.iter().find(|(a, _)| a.name == name) else {
            return Err(self.unknown(name));
        };

        // The ceiling and then the mode, before anything else about this call is even looked at
        // — before the arguments are checked, before the revision guard, and long before the
        // handler — because "may this caller use this action at all" is a question about the
        // action. `gate::decide` is the rule a service answering `app.act` itself meets too, in
        // the same words. `effective_grade`, not `spec.permission`: an app may have moved its own
        // grade since the surface was published (see `regrade`), and the check has to read the
        // grade that `describe` is currently showing or the two disagree.
        let published = effective_grade(name, spec.permission);
        decide(authority, &self.app_id, name, published)?;

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

    /// Grades [`regrade`] has moved since the surface was published, by action name.
    ///
    /// A separate cell, and that is the whole point. The dispatch runs a handler from *inside*
    /// `REGISTRY.borrow()` (see `on_ui_thread`), so a handler that reached for `borrow_mut` on
    /// the same cell panicked with "RefCell already borrowed" and took the app down with it —
    /// which is exactly what `set_backend` did, since calling `regrade` from a handler is the
    /// only way this function is ever meant to be used. Writing the override here means the
    /// registry stays immutably borrowed and nothing re-enters it.
    static OVERRIDES: RefCell<Vec<(String, &'static str)>> = const { RefCell::new(Vec::new()) };
}

/// The grade an action is published at right now: what it was declared with, unless
/// [`regrade`] has moved it.
///
/// Every reader goes through here — the ceiling check in [`Registry::act`], [`published_grade`],
/// and the specs [`Registry::describe`] hands out — so the card a person is shown and the
/// dispatch that enforces it can never be reading two different numbers.
fn effective_grade(action: &str, declared: &'static str) -> &'static str {
    OVERRIDES.with(|cell| {
        cell.borrow()
            .iter()
            .find(|(name, _)| name == action)
            .map(|(_, grade)| *grade)
            .unwrap_or(declared)
    })
}

// ── Who is calling ──────────────────────────────────────────────────
//
// An action handler used to have no way to find out. Everything it could see about its caller
// arrived inside the request, which means the caller wrote it — and the shell was printing one
// of those strings on an approval card under the words "asking to use this machine". Anything
// that could open the socket could put any name there (issue #43).
//
// The kernel knows better and says so for free: `SO_PEERCRED` on an accepted unix socket gives
// the peer's pid, uid and gid, filled in at `connect` time from the peer's own process. The
// transport reads it at accept (see `yantrik_ipc_transport::server::PeerCred`); this module's
// job is to get it to the place the handler actually runs.
//
// That last part is the whole difficulty, and it is why this is a thread-local rather than a
// global. The socket is served on its own thread; handlers run on the UI thread, reached by
// posting a closure to the Slint event loop. A "current caller" stored anywhere shared would be
// read by a handler that belongs to a different request, because two connections can be in
// flight at once. So the caller travels WITH the closure, and is installed on the UI thread for
// exactly the duration of that one dispatch.
//
// The handler signature is untouched: fourteen apps build `|args| { ... }` closures and none of
// them has to change. A handler that cares reads `control::caller()`; every other one never
// learns this exists.

/// Who opened the socket this request came in on, as the kernel reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caller {
    /// The peer process at `connect` time. It may well have exited by now — `yos` runs one call
    /// and stops — so anything that wants `/proc` facts about it must read them promptly.
    pub pid: i32,
    pub uid: u32,
    pub gid: u32,
}

thread_local! {
    /// The caller of the dispatch currently running on THIS thread, or `None`.
    static CURRENT_CALLER: RefCell<Option<Caller>> = const { RefCell::new(None) };
}

/// Who is calling, inside an action or describe handler. `None` when nothing could be
/// established — a TCP dev connection, a peer that vanished, or a handler invoked directly.
///
/// Nothing in this crate refuses anything on the strength of it. Deciding what an identity is
/// worth is the shell's business (`crates/yantrik-ui/src/caller_identity.rs`); the runtime's job
/// is only to make the fact available where it can be read honestly.
pub fn caller() -> Option<Caller> {
    CURRENT_CALLER.with(|cell| *cell.borrow())
}

/// The grade THIS app publishes for one of its own actions.
///
/// Reads the registry installed by [`App::serve`], so it answers only on the thread that owns
/// the window — which is where handlers run, and is the only place it is wanted. Over the socket
/// the same fact arrives as `permission` in `app.describe`; this is the local shortcut, and the
/// shell needs it because asking *itself* over its own socket from its own UI thread is a call
/// that cannot be answered until the call returns.
///
/// `None` means "this app has no action by that name", which a caller must not read as "it is
/// harmless": an unknown action has no grade, and the honest answer to a question about one is
/// a refusal, not a default.
pub fn published_grade(action: &str) -> Option<&'static str> {
    REGISTRY.with(|cell| cell.borrow().as_ref().and_then(|reg| reg.grade_of(action).ok()))
}

/// Re-declare the grade THIS app publishes for one of its own actions, while it is running.
///
/// An app whose actions cost different amounts depending on how it is configured needs this.
/// Studio's `generate` sends a prompt to whatever backend the person chose: to a ComfyUI on their
/// own LAN that is `standard`, to a hosted service it is `sensitive`, because the words leave the
/// machine and may cost money doing it. The grade is fixed when [`App::serve`] runs, and the
/// configuration can change afterwards from an action on this same surface — so without a way to
/// move it, a caller could point the app at a hosted service and generate in the same breath, and
/// the prompt would leave under the grade that applied when it was still local. That is the one
/// direction a grade must never be wrong in, and it is why an app may raise its own grade at
/// runtime rather than publish the cautious one forever.
///
/// Returns the grade now published, so a handler can say what the next call will be asked for.
/// `Err` leaves the published grade untouched: a typo must not quietly un-grade an action.
///
/// Like [`published_grade`], this reads the registry installed by [`App::serve`], so it answers
/// only on the thread that owns the window — which is where handlers run, and the only place a
/// grade can be changed without racing the dispatch that reads it.
///
/// It takes only a SHARED borrow of the registry, and writes the new grade into a separate cell.
/// That is not tidiness: the dispatch runs a handler from inside `REGISTRY.borrow()`, so the
/// first version of this — which took `borrow_mut` — panicked with "RefCell already borrowed"
/// the first time an action called it, killing the app. Calling this from a handler is the only
/// way it is ever meant to be used, so that was every use of it.
pub fn regrade(action: &str, permission: &'static str) -> Result<&'static str, String> {
    if grade(permission).is_none() {
        return Err(format!(
            "`{permission}` is not a level this OS defines ({}), so `{action}` kept the grade it had",
            LADDER.join(" < ")
        ));
    }
    REGISTRY.with(|cell| {
        let installed = cell.borrow();
        let Some(registry) = installed.as_ref() else {
            return Err("this app published no control surface, so there is no grade to change".to_string());
        };
        if !registry.actions.iter().any(|(a, _)| a.name == action) {
            let known: Vec<&str> = registry.actions.iter().map(|(a, _)| a.name.as_str()).collect();
            return Err(format!("this app has no action `{action}`; it offers: {}", known.join(", ")));
        }
        Ok(())
    })?;
    OVERRIDES.with(|cell| {
        let mut set = cell.borrow_mut();
        match set.iter_mut().find(|(name, _)| name == action) {
            Some(entry) => entry.1 = permission,
            None => set.push((action.to_string(), permission)),
        }
    });
    Ok(permission)
}

/// Installs `who` for the duration of `job` and takes it back afterwards.
///
/// A guard rather than a set-then-clear pair, so a handler that panics cannot leave the next
/// dispatch on this thread reading the previous caller's pid. It restores the *previous* value
/// rather than clearing, which costs nothing and keeps a nested call honest.
struct CallerScope(Option<Caller>);

impl CallerScope {
    fn enter(who: Option<Caller>) -> CallerScope {
        let previous = CURRENT_CALLER.with(|cell| cell.replace(who));
        CallerScope(previous)
    }
}

impl Drop for CallerScope {
    fn drop(&mut self) {
        CURRENT_CALLER.with(|cell| *cell.borrow_mut() = self.0);
    }
}

// ── Which agent a call is for ───────────────────────────────────────
//
// A mind running as one of the person's agents carries a token its harness was given (design
// `agents-workspace-2026-09-23.md`, decision 3). It travels BESIDE `args` on `app.act`, the way a
// grant does, and never inside them — because `args` is what gets shown and kept: the approval
// card draws it, `record_unasked_action` writes it to `mind-audit.jsonl`, a grant is bound to it.
// A token in any of those is a token anyone reading the screen or the log can replay.
//
// So the dispatch lifts the token off the call, strips any copy a caller put inside `args`, and
// hands it to the handler the way it hands over the caller: for the duration of the one dispatch,
// on the thread the handler runs on. What the token is worth is the handler's business — the
// shell resolves it against the kernel's account of the caller; here it is only carried.
//
// The lifting itself — `AGENT_TOKEN` and `agent_token_of` — is `yantrik_ipc_transport::gate`'s,
// beside `grant_of`, so a service answering `app.act` in its own handler takes the token out of
// `args` the same way before its grant is spent against them.

thread_local! {
    /// The agent token of the dispatch currently running on THIS thread, or `None`.
    static CURRENT_AGENT_TOKEN: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// The agent token the call being handled carried beside its `args`, inside an action handler.
/// `None` when it carried none, or outside a dispatch.
///
/// Like [`caller`], it is a fact about the call and not a verdict: nothing here checks it.
pub fn agent_token() -> Option<String> {
    CURRENT_AGENT_TOKEN.with(|cell| cell.borrow().clone())
}

/// Installs a dispatch's token for its duration and puts back what was there, panic or not.
struct AgentTokenScope(Option<String>);

impl AgentTokenScope {
    fn enter(token: Option<String>) -> AgentTokenScope {
        AgentTokenScope(CURRENT_AGENT_TOKEN.with(|cell| cell.replace(token)))
    }
}

impl Drop for AgentTokenScope {
    fn drop(&mut self) {
        let previous = self.0.take();
        CURRENT_AGENT_TOKEN.with(|cell| *cell.borrow_mut() = previous);
    }
}

// ── Answers that take time ──────────────────────────────────────────
//
// A handler has the three seconds of `UI_ROUNDTRIP`, on the thread that paints the window. Some
// acts are worth waiting for anyway: the shell's `agent_run` starts a command and owes its caller
// the exit code, which may be two minutes away. Deferring (`settled: false`, "go and look later")
// is the right answer for work whose result lands on screen; it is the wrong one for work whose
// result IS the answer.
//
// So a handler can say "the rest of my answer is this closure". It returns at once — the window
// never waits — and the dispatch runs the closure on the RPC side, where the only thing waiting is
// the one caller who asked. The closure travels from the UI thread back to the RPC thread with the
// reply, the same way the caller travelled out with the request.

/// The rest of an answer, finished off the UI thread.
type Later = Box<dyn FnOnce() -> Result<serde_json::Value, String> + Send>;

thread_local! {
    /// On the UI thread, during one dispatch: where [`answer_later`] leaves the rest of the
    /// answer. `None` outside a dispatch, which is how `answer_later` knows nothing will run it.
    static LATER_SLOT: RefCell<Option<Option<Later>>> = const { RefCell::new(None) };

    /// On the RPC thread: the rest of the answer the dispatch that just came back handed over.
    static LATER_HANDED: RefCell<Option<Later>> = const { RefCell::new(None) };
}

/// Finish this action's answer off the UI thread: `work` runs after the handler has returned, on
/// the socket's side, and what it returns is the caller's `result` (an `Err` is the caller's
/// refusal, exactly as if the handler had returned it).
///
/// Call it from inside a handler, as its last act, and return anything — the value is replaced.
/// `work` must carry everything it needs: it does not run on the UI thread, so it cannot touch the
/// window, and [`caller`] is not set there (read it in the handler and move it in).
///
/// `Err(work)` hands the work back when nothing will run it — the handler was called directly, not
/// through the socket — so the handler can run it itself:
/// `answer_later(work).map(|()| placeholder).or_else(|work| work())`.
///
/// If the UI thread answered too late for the caller (see `UI_ROUNDTRIP`), `work` is dropped
/// without running: do the whole of the act inside it and a late reply starts nothing.
pub fn answer_later<F>(work: F) -> Result<(), F>
where
    F: FnOnce() -> Result<serde_json::Value, String> + Send + 'static,
{
    LATER_SLOT.with(|cell| match cell.borrow_mut().as_mut() {
        Some(slot) => {
            *slot = Some(Box::new(work));
            Ok(())
        }
        None => Err(work),
    })
}

/// Opens the slot for one dispatch on the UI thread and closes it afterwards, even on a panic, so
/// one handler's work can never be run as another's answer.
struct LaterScope(Option<Option<Later>>);

impl LaterScope {
    fn enter() -> LaterScope {
        LaterScope(LATER_SLOT.with(|cell| cell.replace(Some(None))))
    }

    fn take(&self) -> Option<Later> {
        LATER_SLOT.with(|cell| cell.borrow_mut().as_mut().and_then(Option::take))
    }
}

impl Drop for LaterScope {
    fn drop(&mut self) {
        let previous = self.0.take();
        LATER_SLOT.with(|cell| *cell.borrow_mut() = previous);
    }
}

/// Run `work` without holding up the socket's other callers: on the multi-threaded runtime the
/// control surface serves on, this worker steps aside and another takes its connections.
fn off_the_reactor<T>(work: impl FnOnce() -> T) -> T {
    use tokio::runtime::{Handle, RuntimeFlavor};
    match Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(work)
        }
        _ => work(),
    }
}

/// Hand one closure to the thread that owns the window.
///
/// Boxed rather than generic so that the test stand-in below can take it back unrun when no
/// stand-in is installed; the box costs one allocation per RPC call, which is nothing beside
/// the round trip it is part of.
fn post_to_ui(job: Box<dyn FnOnce() + Send>) -> Result<(), String> {
    // In tests there is no Slint event loop and no window. The stand-in is a plain worker
    // thread fed by a channel — the same shape as the real hop (the closure crosses a thread
    // boundary, and the caller has to cross with it), which is the property under test.
    #[cfg(test)]
    let job = match test_ui_thread::post(job) {
        Ok(()) => return Ok(()),
        Err(unrun) => unrun,
    };

    slint::invoke_from_event_loop(job).map_err(|e| format!("app is not accepting requests: {e}"))
}

/// A stand-in for the thread that owns the window, for the one test that needs a real socket.
///
/// The property worth testing is that the caller crosses the thread hop with its own request,
/// and that cannot be tested through a handler called directly — `Registry::act` never sees a
/// socket. It also cannot be tested through the real hop, because `slint::invoke_from_event_loop`
/// needs a running event loop, which needs a window, which needs a display the test machine does
/// not have. So the hop is a channel to a worker thread: same shape, same thread boundary, same
/// thread-local, no compositor.
///
/// One stand-in per test binary, because [`REGISTRY`] is a thread-local and the stand-in is the
/// thread that holds it.
#[cfg(test)]
mod test_ui_thread {
    use std::sync::mpsc::{self, Sender};
    use std::sync::{Mutex, OnceLock};

    type Job = Box<dyn FnOnce() + Send>;

    static STANDIN: OnceLock<Mutex<Sender<Job>>> = OnceLock::new();

    /// Start the stand-in and build the registry ON it.
    ///
    /// `build` rather than a `Registry`: a registry holds the app's own closures, which are not
    /// `Send` (they capture Slint handles in a real app), so it has to be made on the thread
    /// that will keep it. Returns only once the registry is in place, so a request that arrives
    /// immediately cannot find an empty one.
    pub(super) fn start(build: Box<dyn FnOnce() -> super::Registry + Send>) {
        let (tx, rx) = mpsc::channel::<Job>();
        let (ready, is_ready) = mpsc::channel::<()>();
        std::thread::Builder::new()
            .name("control-test-ui".into())
            .spawn(move || {
                super::REGISTRY.with(|cell| *cell.borrow_mut() = Some(build()));
                let _ = ready.send(());
                while let Ok(job) = rx.recv() {
                    job();
                }
            })
            .expect("stand-in UI thread");
        is_ready.recv().expect("the stand-in installed its registry");
        STANDIN
            .set(Mutex::new(tx))
            .map_err(|_| ())
            .expect("only one stand-in per test binary");
    }

    /// Post to the stand-in, or hand the job straight back when there is none — which is every
    /// test but the one, so nothing else in this module changes behaviour under `cfg(test)`.
    pub(super) fn post(job: Job) -> Result<(), Job> {
        let Some(tx) = STANDIN.get() else { return Err(job) };
        let tx = tx.lock().unwrap_or_else(|e| e.into_inner());
        tx.send(job).map_err(|e| e.0)
    }
}

/// Ask the UI thread to run `job` and wait for its answer.
///
/// Returns `Err` when the event loop is not running or is too busy to answer inside
/// [`UI_ROUNDTRIP`] — both of which the caller should see as an error rather than a hang.
///
/// `who` rides along to the far side. It is installed there, not here: the handler runs on the
/// UI thread, so the UI thread is the only place a thread-local can be read by it.
fn on_ui_thread<T, F>(who: Option<Caller>, job: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&Registry) -> T + Send + 'static,
{
    let (tx, rx) = mpsc::sync_channel::<(Result<T, String>, Option<Later>)>(1);
    post_to_ui(Box::new(move || {
        let _scope = CallerScope::enter(who);
        let later = LaterScope::enter();
        let answer = REGISTRY.with(|cell| match cell.borrow().as_ref() {
            Some(reg) => Ok(job(reg)),
            None => Err("this app published no control surface".to_string()),
        });
        // The receiver is gone only if we already timed out; dropping the answer is correct.
        let _ = tx.send((answer, later.take()));
    }))?;

    let (answer, later) = rx
        .recv_timeout(UI_ROUNDTRIP)
        .map_err(|_| format!("app did not answer within {}s", UI_ROUNDTRIP.as_secs()))?;
    // For `ControlRpc::handle_from`, on this same thread, which finishes it. See `answer_later`.
    LATER_HANDED.with(|cell| *cell.borrow_mut() = later);
    answer
}

// ── The RPC surface ─────────────────────────────────────────────────

struct ControlRpc {
    service_id: String,
    /// The id the registry publishes under — what a grant is bound to, which is not the
    /// socket's `app-` name.
    app_id: String,
}

impl ServiceHandler for ControlRpc {
    fn service_id(&self) -> &str {
        &self.service_id
    }

    /// The transport's older entry point. Kept so the trait is satisfied for any caller that
    /// still uses it; it means "nobody said who was calling", which is exactly what `None` is.
    fn handle(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, ServiceError> {
        self.handle_from(method, params, None)
    }

    fn handle_from(
        &self,
        method: &str,
        params: serde_json::Value,
        peer: Option<PeerCred>,
    ) -> Result<serde_json::Value, ServiceError> {
        let who = peer.map(|p| Caller { pid: p.pid, uid: p.uid, gid: p.gid });
        // Nothing left over from an earlier call on this thread can be taken for this one's.
        LATER_HANDED.with(|cell| cell.borrow_mut().take());
        let answer = self.dispatch(method, params, who);
        let later = LATER_HANDED.with(|cell| cell.borrow_mut().take());
        match (answer, later) {
            (Ok(envelope), Some(later)) if method == "app.act" => finish_later(envelope, later, who),
            (answer, _) => answer,
        }
    }
}

/// Run the rest of an answer a handler left with [`answer_later`], and put its result in the
/// envelope — with the view read again afterwards, so the state beside the result is the state
/// the result came from rather than the state before the wait.
fn finish_later(
    mut envelope: serde_json::Value,
    later: Later,
    who: Option<Caller>,
) -> Result<serde_json::Value, ServiceError> {
    let result = off_the_reactor(later).map_err(|message| ServiceError { code: -32602, message })?;
    envelope["result"] = result;
    let after = on_ui_thread(who, |reg| {
        let now = reg.snapshot();
        (now.summary, now.state, now.revision)
    });
    // A UI thread too busy to answer now does not undo what the work did: the result stands and
    // the view is the one from when the handler ran.
    if let Ok((summary, state, revision)) = after {
        envelope["summary"] = summary.into();
        envelope["state"] = state;
        envelope["revision"] = revision.into();
    }
    Ok(envelope)
}

impl ControlRpc {
    fn dispatch(
        &self,
        method: &str,
        params: serde_json::Value,
        who: Option<Caller>,
    ) -> Result<serde_json::Value, ServiceError> {
        match method {
            "app.describe" => on_ui_thread(who, |reg| reg.describe())
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
                let mut args = params.get("args").cloned().unwrap_or(serde_json::json!({}));
                // Lifted off before anything reads `args` — the grant below is bound to them —
                // and out of `args` if a caller put it there. See `agent_token`.
                let token = agent_token_of(&params, &mut args);
                // Optional, and deliberately so: a caller acting on its own initiative has nothing
                // to compare against, and demanding a revision it never read would only teach it
                // to send back whatever it last saw.
                let expect = params
                    .get("expect_revision")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                // A grant, when the caller holds one: the `request_id` the shell answered
                // `request_approval` with, once a person has pressed Allow. Optional for the
                // same reason `expect_revision` is — most calls need none.
                let grant = grant_of(&params);
                let action_id = next_action_id(&self.service_id);

                // Read on this thread, enforced on the UI one: the settings and mode files are
                // IO, spending a grant is a round trip, and the dispatch closure is a turn of
                // the event loop.
                let mut authority = Authority::now();
                if let Some(id) = grant.as_deref() {
                    // A grant is spent only once the ceiling has passed on the action's grade,
                    // or a person's Allow is used up on an act that is then refused and never
                    // runs (#154). The grade lives on the UI thread, so ask it first — one extra
                    // hop, only for a call that carries a grant, which is one a person has just
                    // answered a card for. An action this app does not have is answered as that
                    // here, and nothing is spent on it. Should the app regrade the action between
                    // this read and the dispatch, the dispatch still decides on the grade it
                    // publishes then; the most that race can cost is the grant.
                    let name = action.clone();
                    let graded = on_ui_thread(who, move |reg| reg.grade_of(&name))
                        .map_err(|m| ServiceError { code: -32000, message: m })?
                        .map_err(|m| ServiceError { code: -32602, message: m })?;
                    authority
                        .spend(id, &self.app_id, &action, graded, &args)
                        .map_err(|m| ServiceError { code: -32602, message: m })?;
                }
                tracing::info!(
                    action = %action,
                    id = %action_id,
                    ceiling = %authority.ceiling,
                    mode = %authority.mode.name,
                    granted = authority.granted,
                    // Logged as a pair so a line in the journal says who as well as what. The
                    // audit log is the shell's job; this is the runtime's own record.
                    caller_pid = who.map(|c| c.pid).unwrap_or(0),
                    caller_uid = who.map(|c| c.uid).unwrap_or(0),
                    // Whether one came, never the token itself.
                    agent_token = token.is_some(),
                    "app.act"
                );
                let id = action_id.clone();
                let outcome = on_ui_thread(who, move |reg| {
                    let _agent = AgentTokenScope::enter(token);
                    reg.act(&action, &args, expect.as_deref(), &id, &authority)
                })
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
        serve_rpc(&app_id, action_count);
    }
}

/// Bind the socket and answer on it, on a thread of its own.
///
/// Split out of [`App::serve`] so the test below can put the registry on a stand-in UI thread
/// and still bind exactly the same server. `serve` itself is byte-for-byte what it always did.
fn serve_rpc(app_id: &str, action_count: usize) {
    {
        let service_id = service_id_for(app_id);
        let app_id = app_id.to_string();
        link_other_names(&app_id);
        std::thread::Builder::new()
            .name(format!("{service_id}-rpc"))
            .spawn(move || {
                // Multi-threaded, and small: a caller waiting on `answer_later` steps its worker
                // out of the way and the other keeps serving everyone else.
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
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
                        .serve(Arc::new(ControlRpc { service_id: service_id.clone(), app_id }))
                        .await
                    {
                        tracing::warn!(error = %e, "Control surface stopped");
                    }
                });
            })
            .ok();
    }
}

/// Put every other name this app answers to beside the socket it is about to bind.
///
/// A symlink, not a second listener: it is one surface, so a caller that follows
/// `app-container-manager.sock` has to land in the same process and read the same revision. Two
/// listeners would be two answers to the same question, and a `describe`/`act` pair split across
/// them is the race `expect_revision` exists to close.
///
/// Made before the bind rather than after it, because nothing here can be told when the bind
/// happened and polling for the file would be a second way to be wrong. A symlink to a socket
/// that does not exist yet is invisible to a caller — `os.path.exists` is false and `connect`
/// gets ENOENT — and starts working the moment the socket lands, which is the same fall-through
/// every caller already has for the stale socket of a closed window.
///
/// Nothing here is fatal. An app whose other names cannot be linked is still a working app,
/// reachable by the id it publishes, which is what it was before.
#[cfg(unix)]
fn link_other_names(app_id: &str) {
    let others = other_names(app_id);
    if others.is_empty() {
        return;
    }
    let dir = yantrik_ipc_transport::server::socket_dir();
    for name in link_names(&dir, app_id, others) {
        tracing::info!(name = %name, app = app_id, "Control surface also answers to this name");
    }
}

#[cfg(not(unix))]
fn link_other_names(_app_id: &str) {}

/// Link `others` at `app_id`'s socket inside `dir`, and report the names that now reach it.
///
/// Separated from the directory lookup so it can be tested in a directory of its own. Relative
/// link targets on purpose: the socket directory is moved by nothing, and a relative target
/// survives being read from a different mount view of the same runtime dir.
#[cfg(unix)]
fn link_names(dir: &std::path::Path, app_id: &str, others: &[&str]) -> Vec<String> {
    let target = format!("{}.sock", service_id_for(app_id));
    let mut linked = Vec::new();
    for name in others {
        let link = dir.join(format!("{}.sock", service_id_for(name)));
        // What is there already is from an earlier run of this app, or from an earlier release
        // that bound this name for real. Either way it is not something to connect to now, and
        // leaving it would leave the other name pointing at nothing.
        match std::fs::read_link(&link) {
            Ok(existing) if existing == std::path::Path::new(&target) => {
                linked.push((*name).to_string());
                continue;
            }
            Ok(_) => {
                let _ = std::fs::remove_file(&link);
            }
            Err(_) if link.symlink_metadata().is_ok() => {
                let _ = std::fs::remove_file(&link);
            }
            Err(_) => {}
        }
        match std::os::unix::fs::symlink(&target, &link) {
            Ok(()) => linked.push((*name).to_string()),
            Err(e) => tracing::warn!(
                name = *name,
                app = app_id,
                error = %e,
                "Could not link one of this app's other names; callers holding it cannot reach it"
            ),
        }
    }
    linked
}

// ── Finding the others ──────────────────────────────────────────────

/// The app ids that currently have a control socket in this session.
///
/// A socket file outlives a crashed process, so this is a list of candidates, not of live apps —
/// callers should treat a failed `app.describe` as "gone" rather than as an error worth reporting.
///
/// One app, one entry: the other names an app answers to are symlinks to its socket (see
/// `link_names`), and listing those would report one open window twice under two names.
#[cfg(unix)]
pub fn running_apps() -> Vec<String> {
    let dir = yantrik_ipc_transport::server::socket_dir();
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut ids: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| !e.file_type().is_ok_and(|kind| kind.is_symlink()))
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

    /// A ceiling that binds nothing, for the tests that are about everything *except* the
    /// boundary. The boundary has its own tests below, with the ceiling and the mode pinned per
    /// case rather than inherited from whatever files the machine running them happens to have.
    const OPEN: &str = "dangerous";

    /// Authority that binds nothing: the ceiling and the mode both at the top of the ladder.
    fn open() -> Authority {
        Authority { ceiling: OPEN.into(), mode: Mode::named("bypass"), granted: false }
    }

    /// A machine at `ceiling`, in a mode that asks about nothing under it: the ceiling tests.
    fn under(ceiling: &str) -> Authority {
        Authority { ceiling: ceiling.into(), mode: Mode::named("bypass"), granted: false }
    }

    /// An open ceiling and the mode under test, with or without a grant spent for the call.
    fn in_mode(mode: &str, granted: bool) -> Authority {
        Authority { ceiling: OPEN.into(), mode: Mode::named(mode), granted }
    }

    #[test]
    fn service_ids_do_not_collide_with_services() {
        // notes-service owns `notes`; the Notes window must not bind the same socket.
        assert_eq!(service_id_for("notes"), "app-notes");
        assert_ne!(service_id_for("notes"), "notes");
    }

    /// The name the container manager is called everywhere else reaches the id it publishes.
    ///
    /// This is the refusal the table was written for: `yos ls` said `app-containers`, the app was
    /// `container-manager` in `/opt/yantrik/bin`, in the launcher and in `open_app`, and
    /// `describe container-manager` answered "no socket for 'container-manager'".
    #[test]
    fn every_name_an_app_is_known_by_reaches_the_id_it_publishes() {
        for spelling in [
            "container-manager", "container_manager", "Container Manager", "CONTAINER-MANAGER",
            "  containers  ", "containers",
        ] {
            assert_eq!(surface_id(spelling), Some("containers"), "{spelling}");
        }
        assert_eq!(surface_id("sysmonitor"), Some("system-monitor"));
        assert_eq!(surface_id("downloads"), Some("download-manager"));
        assert_eq!(surface_id("text-editor"), Some("editor"));
        assert_eq!(surface_id("slides"), Some("presentation"));
        // An app with one name answers to it, and nothing answers for an app this desktop does
        // not have. "No such app" and "that app is closed" are different answers and a caller
        // acts differently on them.
        assert_eq!(surface_id("notes"), Some("notes"));
        assert_eq!(surface_id("no-such-app"), None);
        assert_eq!(surface_id(""), None);
    }

    /// No name is claimed twice, and every name is written the way the folding leaves it.
    ///
    /// A second claim on one name would be resolved by whichever row came first, silently, and a
    /// name stored with an underscore could never match anything, because every lookup folds.
    #[test]
    fn no_two_apps_answer_to_the_same_name() {
        let mut seen = std::collections::HashSet::new();
        for (id, others) in SURFACES {
            for name in std::iter::once(id).chain(others.iter()) {
                assert!(seen.insert(*name), "`{name}` is claimed by two apps");
                assert_eq!(fold(name), *name, "`{name}` is not folded, so nothing can match it");
            }
        }
    }

    /// The other names of a running app are its socket under another name, not another socket.
    #[cfg(unix)]
    #[test]
    fn another_name_for_an_app_points_at_the_socket_it_bound() {
        let dir = std::env::temp_dir().join(format!("yantrik-names-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("app-containers.sock");
        std::fs::write(&socket, b"stands in for the bound socket").unwrap();

        let linked = link_names(&dir, "containers", other_names("containers"));
        assert_eq!(linked, vec!["container-manager".to_string()]);
        let link = dir.join("app-container-manager.sock");
        assert_eq!(std::fs::read_link(&link).unwrap().to_str(), Some("app-containers.sock"));
        assert_eq!(std::fs::read(&link).unwrap(), std::fs::read(&socket).unwrap());

        // Run twice, as a reopened app does: the second link is the same link, not an error.
        assert_eq!(link_names(&dir, "containers", other_names("containers")).len(), 1);

        // A real file under that name, left by a release that bound it for real, is replaced —
        // otherwise the other name would go on answering out of a socket nothing is behind.
        std::fs::remove_file(&link).unwrap();
        std::fs::write(&link, b"an older release bound this name").unwrap();
        assert_eq!(link_names(&dir, "containers", other_names("containers")).len(), 1);
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());

        let _ = std::fs::remove_dir_all(&dir);
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

        let err = reg.act("open_note", &serde_json::json!({}), None, "t#1", &open()).unwrap_err();
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
            .act("open_note", &serde_json::json!({"title": "a", "colour": "red"}), None, "t#1", &open())
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
            .act("new_note", &serde_json::json!({"title": "Handover"}), None, "t#1", &open())
            .unwrap_err();
        assert!(err.contains("takes no arguments"), "{err}");
        assert!(err.contains("title"), "{err}");

        // And the no-argument call it was always meant to accept still works.
        assert!(reg.act("new_note", &serde_json::json!({}), None, "t#2", &open()).is_ok());
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

        let err = reg.act("nope", &serde_json::json!({}), None, "t#1", &open()).unwrap_err();
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
            .act("rename", &serde_json::json!({ "to": "Kernel answers" }), None, "notes#1", &open())
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

        let answer = reg.act("build", &serde_json::json!({}), None, "builder#1", &open()).unwrap();
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
            .act("rename", &serde_json::json!({ "to": "x" }), Some(&stale), "notes#1", &open())
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
            .act("rename", &serde_json::json!({ "to": "ok" }), Some(&current), "notes#1", &open())
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
            .act("rename", &serde_json::json!({ "to": "ok" }), None, "notes#1", &open())
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

        let err = reg.act("go", &serde_json::json!({}), Some("0000000000000000"), "n#1", &open());
        assert!(err.is_err());
        assert!(!ran.get(), "the handler must not have run");
    }

    // ── The ceiling ──

    /// The shape the bug was measured in: `files_delete`, graded `dangerous`, on a machine
    /// whose ceiling is `sensitive`. The bridge refused it; the dispatch waved it through, and
    /// the file was gone. These tests live here, beside the dispatch, so the boundary fails
    /// loudly if anyone moves the check back out to a caller.
    fn delete_surface(ran: std::rc::Rc<std::cell::Cell<bool>>) -> Registry {
        Registry {
            app_id: "shell".into(),
            describe: Some(Box::new(|| View::new("Shell \u{2014} Files"))),
            actions: vec![(
                Action::new("files_delete", "Delete a file").risk("dangerous").arg(Param::text("name")),
                Box::new(move |args| {
                    ran.set(true);
                    Ok(serde_json::json!({ "deleted": args["name"].clone() }))
                }),
            )],
        }
    }

    #[test]
    fn an_action_above_the_ceiling_is_refused_by_the_dispatch_itself() {
        let ran = std::rc::Rc::new(std::cell::Cell::new(false));
        let reg = delete_surface(ran.clone());

        let err = reg
            .act("files_delete", &serde_json::json!({"name": "x"}), None, "shell#1", &under("sensitive"))
            .unwrap_err();

        assert!(err.starts_with("CEILING:"), "a caller has to be able to branch on this: {err}");
        assert!(err.contains("dangerous"), "the refusal names the grade: {err}");
        assert!(err.contains("sensitive"), "and the ceiling it was over: {err}");
        assert!(err.contains("tool_permission"), "and where that ceiling is set: {err}");
        assert!(!ran.get(), "the handler must not have run");
    }

    #[test]
    fn the_ceiling_refuses_on_the_grade_alone_before_anything_is_checked() {
        // The measured MCP behaviour, now the dispatch's too: it refused `files_delete {"name":
        // "x"}` on grade before even establishing whether `x` existed. A caller over the ceiling
        // gets one answer regardless of what else is wrong with its call — otherwise fixing the
        // smaller mistake looks like progress toward a call that was never going to run.
        let reg = delete_surface(std::rc::Rc::new(std::cell::Cell::new(false)));

        let err = reg.act("files_delete", &serde_json::json!({}), None, "shell#1", &under("sensitive")).unwrap_err();
        assert!(err.starts_with("CEILING:"), "not the missing-argument error: {err}");
        assert!(!err.contains("needs argument"), "{err}");
    }

    #[test]
    fn an_action_at_or_below_the_ceiling_runs() {
        // "At or below" is the whole contract; the ceiling is not a blanket refusal.
        let ran = std::rc::Rc::new(std::cell::Cell::new(false));
        let reg = delete_surface(ran.clone());
        let answer = reg
            .act("files_delete", &serde_json::json!({"name": "x"}), None, "shell#1", &under("dangerous"))
            .unwrap();
        assert_eq!(answer["accepted"], true);
        assert!(ran.get());

        // And the everyday case: a `standard` action under the shipped `sensitive` default.
        let answer = notes_at("Kernel asks")
            .act("rename", &serde_json::json!({ "to": "ok" }), None, "notes#1", &under("sensitive"))
            .unwrap();
        assert_eq!(answer["accepted"], true);
    }

    #[test]
    fn a_ceiling_tightened_to_safe_binds_the_default_actions_too() {
        // The setting has to actually tighten, not only refuse the graded-dangerous few: every
        // action floors at `standard`, so `safe` closes the door to programmatic callers entirely.
        let err = notes_at("Kernel asks")
            .act("rename", &serde_json::json!({ "to": "ok" }), None, "notes#1", &under("safe"))
            .unwrap_err();
        assert!(err.starts_with("CEILING:"), "{err}");
        assert!(err.contains("standard"), "the refusal names the action's own grade: {err}");
    }

    #[test]
    fn an_action_graded_off_the_ladder_is_refused_not_waved_through() {
        // The mirror of the bridge's rule: an ungradeable action is not "safe". A typo in a
        // `.risk(...)` must fail closed, or the typo silently becomes an exemption.
        let reg = Registry {
            app_id: "notes".into(),
            describe: None,
            actions: vec![(
                Action::new("nuke", "Typo'd grade").risk("catastrophic"),
                Box::new(|_| Ok(serde_json::json!("never reached"))),
            )],
        };

        let err = reg.act("nuke", &serde_json::json!({}), None, "notes#1", &open()).unwrap_err();
        assert!(err.starts_with("CEILING:"), "{err}");
        assert!(err.contains("not a level this OS defines"), "{err}");
    }

    #[test]
    fn the_ceiling_comes_from_the_settings_file() {
        // Same file, same key, same default as the shell's Settings screen — a boundary that
        // reads a different source than the one a person can see is a boundary nobody can
        // predict. Missing or unrecognised falls back to the shipped default, never open.
        assert_eq!(ceiling_from("dark_mode: true\ntool_permission: standard\n"), "standard");
        assert_eq!(ceiling_from("tool_permission: \"safe\"\n"), "safe");
        assert_eq!(ceiling_from("dark_mode: true\n"), DEFAULT_CEILING, "absent key");
        assert_eq!(ceiling_from(""), DEFAULT_CEILING, "empty file");
        assert_eq!(ceiling_from("tool_permission: whenever-i-feel_like_it\n"), DEFAULT_CEILING);
    }

    #[test]
    fn an_apps_own_grade_can_be_read_without_a_round_trip() {
        // The shell needs this to check a caller's *claimed* grade against the app's real one,
        // and for its own actions it cannot ask over the socket: the answer would have to come
        // from the UI thread that is making the call. `None` for an action that does not exist,
        // because an unknown action has no grade and defaulting one would invent a permission.
        REGISTRY.with(|cell| {
            *cell.borrow_mut() = Some(Registry {
                app_id: "shell".into(),
                describe: None,
                actions: vec![
                    (
                        Action::new("files_delete", "Delete a file").risk("dangerous"),
                        Box::new(|_| Ok(serde_json::Value::Null)),
                    ),
                    (
                        Action::new("open_app", "Open an app"),
                        Box::new(|_| Ok(serde_json::Value::Null)),
                    ),
                ],
            })
        });

        assert_eq!(published_grade("files_delete"), Some("dangerous"));
        assert_eq!(published_grade("open_app"), Some("standard"), "unstated risk is standard");
        assert_eq!(published_grade("no_such_action"), None);

        REGISTRY.with(|cell| *cell.borrow_mut() = None);
        assert_eq!(published_grade("files_delete"), None, "and nothing is served here now");
    }

    #[test]
    fn an_action_can_be_regraded_while_the_app_runs_and_the_ceiling_follows() {
        // Studio's shape: `generate` is graded when the surface is published, and the backend it
        // sends prompts to can be changed afterwards by another action on the same surface. The
        // grade has to move with it, or a caller under a `standard` ceiling can be talked into
        // sending a prompt off the machine by an action that never asked for anything.
        REGISTRY.with(|cell| {
            *cell.borrow_mut() = Some(Registry {
                app_id: "studio".into(),
                describe: None,
                actions: vec![(
                    Action::new("generate", "Make a picture from a sentence"),
                    Box::new(|_| Ok(serde_json::json!({"queued": 1}))),
                )],
            })
        });

        // Local backend: a `standard` ceiling lets it through, and says it settled nothing yet only
        // because this handler is not deferred.
        REGISTRY.with(|cell| {
            let installed = cell.borrow();
            let registry = installed.as_ref().unwrap();
            assert!(registry.act("generate", &serde_json::json!({}), None, "studio#1", &under("standard")).is_ok());
        });

        assert_eq!(regrade("generate", "sensitive").unwrap(), "sensitive");
        assert_eq!(published_grade("generate"), Some("sensitive"), "the two readers disagree");

        REGISTRY.with(|cell| {
            let installed = cell.borrow();
            let registry = installed.as_ref().unwrap();
            // The same call, the same arguments, the same ceiling: refused now, and refused for the
            // grade rather than for anything about the arguments.
            let err = registry
                .act("generate", &serde_json::json!({}), None, "studio#2", &under("standard"))
                .unwrap_err();
            assert!(err.starts_with("CEILING:"), "{err}");
            assert!(err.contains("graded `sensitive`"), "{err}");
            assert!(registry.act("generate", &serde_json::json!({}), None, "studio#3", &under("sensitive")).is_ok());
            // And `describe` — what a caller reads before deciding — reports the new grade, so the
            // card a person is shown is the card the dispatch will enforce.
            let described = registry.describe();
            assert_eq!(described["actions"][0]["permission"], serde_json::json!("sensitive"));
        });

        // Down again, because the backend can be pointed back at a machine the person owns.
        assert_eq!(regrade("generate", "standard").unwrap(), "standard");
        REGISTRY.with(|cell| {
            let installed = cell.borrow();
            let registry = installed.as_ref().unwrap();
            assert!(registry.act("generate", &serde_json::json!({}), None, "studio#4", &under("standard")).is_ok());
        });
        REGISTRY.with(|cell| *cell.borrow_mut() = None);
        OVERRIDES.with(|cell| cell.borrow_mut().clear());
    }

    #[test]
    fn a_grade_this_os_does_not_define_leaves_the_action_at_the_one_it_had() {
        REGISTRY.with(|cell| {
            *cell.borrow_mut() = Some(Registry {
                app_id: "studio".into(),
                describe: None,
                actions: vec![
                    (
                        Action::new("generate", "Make a picture").risk("standard"),
                        Box::new(|_| Ok(serde_json::Value::Null)),
                    ),
                    (
                        Action::new("refresh", "Read the gallery again"),
                        Box::new(|_| Ok(serde_json::Value::Null)),
                    ),
                ],
            })
        });

        // A typo must not quietly un-grade an action, which is what writing the string through
        // without checking it would do.
        let err = regrade("generate", "catastrophic").unwrap_err();
        assert!(err.contains("not a level this OS defines"), "{err}");
        assert!(err.contains(LADDER[0]) && err.contains(LADDER[3]), "{err} does not name the ladder");
        assert_eq!(published_grade("generate"), Some("standard"), "the grade moved anyway");

        // Nor may one action's regrade touch another, or an action that does not exist.
        let err = regrade("no_such_action", "sensitive").unwrap_err();
        assert!(err.contains("no action `no_such_action`"), "{err}");
        assert!(err.contains("generate") && err.contains("refresh"), "{err} does not name what is there");
        assert_eq!(published_grade("refresh"), Some("standard"), "an unknown action regraded a known one");

        REGISTRY.with(|cell| *cell.borrow_mut() = None);
        OVERRIDES.with(|cell| cell.borrow_mut().clear());
        let err = regrade("generate", "sensitive").unwrap_err();
        assert!(err.contains("published no control surface"), "{err}");
    }

    #[test]
    fn a_handler_can_regrade_from_inside_its_own_dispatch() {
        // The test the first two were missing, and the only way `regrade` is ever actually used.
        //
        // Both tests above called `regrade` from open code, where nothing was holding the
        // registry. Real callers do not: `on_ui_thread` runs every handler from INSIDE
        // `REGISTRY.borrow()`, so the first shipped version — which took `borrow_mut` — panicked
        // with "RefCell already borrowed" the moment Studio's `set_backend` ran, and took the
        // whole app down with it. The config had already been written by then, so the app came
        // back pointed at a hosted service with the grade never raised: precisely the state
        // `regrade` exists to prevent.
        //
        // This mirrors the dispatch: the borrow is held across `act`, exactly as it is in
        // `on_ui_thread`. It panics on the old implementation and passes on this one.
        REGISTRY.with(|cell| {
            *cell.borrow_mut() = Some(Registry {
                app_id: "studio".into(),
                describe: None,
                actions: vec![
                    (
                        Action::new("generate", "Make a picture from a sentence"),
                        Box::new(|_| Ok(serde_json::json!({"queued": 1}))),
                    ),
                    (
                        Action::new("set_backend", "Choose where pictures are made").risk("sensitive"),
                        Box::new(|_| {
                            // A handler, doing the one thing this function is for.
                            let now = regrade("generate", "sensitive")?;
                            Ok(serde_json::json!({ "generate_is_graded": now }))
                        }),
                    ),
                ],
            })
        });

        let answered = REGISTRY.with(|cell| {
            let installed = cell.borrow();
            let registry = installed.as_ref().unwrap();
            registry
                .act("set_backend", &serde_json::json!({}), None, "studio#1", &under("sensitive"))
                .expect("set_backend must not take the app down")
        });
        assert_eq!(answered["accepted"], serde_json::json!(true));
        assert_eq!(answered["result"]["generate_is_graded"], serde_json::json!("sensitive"));

        // And the move took effect for every reader, still from inside the same kind of borrow.
        REGISTRY.with(|cell| {
            let installed = cell.borrow();
            let registry = installed.as_ref().unwrap();
            let err = registry
                .act("generate", &serde_json::json!({}), None, "studio#2", &under("standard"))
                .unwrap_err();
            assert!(err.starts_with("CEILING:") && err.contains("graded `sensitive`"), "{err}");
            let described = registry.describe();
            assert_eq!(described["actions"][0]["permission"], serde_json::json!("sensitive"));
        });
        assert_eq!(published_grade("generate"), Some("sensitive"));

        REGISTRY.with(|cell| *cell.borrow_mut() = None);
        OVERRIDES.with(|cell| cell.borrow_mut().clear());
    }

    // ── The mode, and the grant ──

    /// Blender's `render`, graded `sensitive`, over a flag that says whether it ran — the action
    /// the account from inside VM 520 found running through `yos act` with no card.
    fn render_surface(ran: std::rc::Rc<std::cell::Cell<bool>>) -> Registry {
        Registry {
            app_id: "blender".into(),
            describe: Some(Box::new(|| View::new("Blender \u{2014} cube.blend"))),
            actions: vec![(
                Action::new("render", "Render the scene").risk("sensitive").arg(Param::text("out")),
                Box::new(move |args| {
                    ran.set(true);
                    Ok(serde_json::json!({ "rendered_to": args["out"].clone() }))
                }),
            )],
        }
    }

    /// The defect of #116 and #49: `blender.render` is `sensitive`, the machine was in `ask`,
    /// and through `yos act` it ran in 1.72 s with no card and no record, because only the MCP
    /// bridge knew the mode. The dispatch is the one function every door crosses, so it is
    /// where the refusal has to live — and the refusal has to say how to get a grant, or a
    /// program reading it goes looking for another door.
    #[test]
    fn a_sensitive_act_without_a_grant_is_refused_in_ask_mode() {
        let ran = std::rc::Rc::new(std::cell::Cell::new(false));
        let reg = render_surface(ran.clone());

        let err = reg
            .act("render", &serde_json::json!({"out": "x.png"}), None, "blender#1", &in_mode("ask", false))
            .unwrap_err();

        assert!(err.starts_with("GRANT:"), "a caller has to be able to branch on this: {err}");
        assert!(err.contains("graded `sensitive`"), "the refusal names the grade: {err}");
        assert!(err.contains("ask mode"), "and the mode it was over: {err}");
        assert!(err.contains("request_approval") && err.contains("press Allow"), "and how to get a grant: {err}");
        assert!(!ran.get(), "the handler must not have run");
    }

    /// On the grade alone, before the arguments — as the ceiling refuses. A caller over the
    /// mode gets one answer whatever else is wrong with its call; otherwise fixing the smaller
    /// mistake looks like progress toward a call that was never going to run unasked.
    #[test]
    fn the_mode_refuses_before_the_arguments_are_looked_at() {
        let reg = render_surface(std::rc::Rc::new(std::cell::Cell::new(false)));
        let err = reg.act("render", &serde_json::json!({}), None, "blender#1", &in_mode("ask", false)).unwrap_err();
        assert!(err.starts_with("GRANT:"), "not the missing-argument error: {err}");
        assert!(!err.contains("needs argument"), "{err}");
    }

    /// `auto` exists for routine sensitive work, and bypass says "it does not ask" on a red
    /// panel with a countdown. A card in either would make the mode chip a lie.
    #[test]
    fn a_sensitive_act_runs_in_auto_mode() {
        for mode in ["auto", "bypass"] {
            let ran = std::rc::Rc::new(std::cell::Cell::new(false));
            let answer = render_surface(ran.clone())
                .act("render", &serde_json::json!({"out": "x.png"}), None, "blender#1", &in_mode(mode, false))
                .unwrap_or_else(|e| panic!("{mode}: {e}"));
            assert_eq!(answer["accepted"], true, "{mode}");
            assert!(ran.get(), "{mode}: the handler ran");
        }
    }

    /// A grant is a person's Allow for this exact call, and that answer stands whatever the
    /// mode — including plan, where the shell raises no card at all, so a grant there can only
    /// have come from somewhere a person said yes.
    #[test]
    fn a_grant_lets_a_sensitive_act_run_in_any_mode() {
        for mode in ["plan", "ask", "auto", "bypass"] {
            let ran = std::rc::Rc::new(std::cell::Cell::new(false));
            let answer = render_surface(ran.clone())
                .act("render", &serde_json::json!({"out": "x.png"}), None, "blender#1", &in_mode(mode, true))
                .unwrap_or_else(|e| panic!("{mode}: {e}"));
            assert_eq!(answer["accepted"], true, "{mode}");
            assert!(ran.get(), "{mode}: the handler ran");
        }
    }

    /// The everyday case has to stay everyday: a `standard` action asks nobody, in any mode.
    /// Plan included, because the desktop's own processes cross this dispatch with `standard`
    /// calls — every app starts its services on demand through the shell's `start_service` —
    /// and a dispatch that refused them in plan would stop the person's desktop working the
    /// moment they chose plan for the mind. Plan's refusal of `standard` is the bridge's.
    #[test]
    fn a_standard_act_needs_no_grant_in_any_mode() {
        for mode in ["plan", "ask", "auto", "bypass"] {
            let answer = notes_at("Kernel asks")
                .act("rename", &serde_json::json!({ "to": "ok" }), None, "notes#1", &in_mode(mode, false))
                .unwrap_or_else(|e| panic!("{mode}: {e}"));
            assert_eq!(answer["accepted"], true, "{mode}");
        }
    }

    /// Plan raises no card, so above the floor it refuses outright — in words that say no card
    /// is coming, or `yos act` would promise one the shell will not put up.
    #[test]
    fn plan_mode_refuses_a_sensitive_act_and_says_no_card_is_coming() {
        let ran = std::rc::Rc::new(std::cell::Cell::new(false));
        let err = render_surface(ran.clone())
            .act("render", &serde_json::json!({"out": "x.png"}), None, "blender#1", &in_mode("plan", false))
            .unwrap_err();
        assert!(err.starts_with("GRANT:") && err.contains("plan mode"), "{err}");
        assert!(err.contains("raises no card"), "plan must not promise a card: {err}");
        assert!(!ran.get());
    }

    /// "Allow for this session" is the person's standing answer for one action: it covers any
    /// arguments of that action and nothing beside it — not the app's other actions, not
    /// another app's action of the same name.
    #[test]
    fn a_session_rule_covers_the_action_it_names_and_no_other() {
        let with_rule = |app: &str, action: &str| {
            let mut mode = Mode::named("ask");
            mode.session_rules.push((app.to_string(), action.to_string()));
            Authority { ceiling: OPEN.into(), mode, granted: false }
        };
        let args = serde_json::json!({"out": "anything.png"});

        let ran = std::rc::Rc::new(std::cell::Cell::new(false));
        render_surface(ran.clone()).act("render", &args, None, "b#1", &with_rule("blender", "render")).unwrap();
        assert!(ran.get());

        for (app, action) in [("blender", "bake"), ("studio", "render")] {
            let ran = std::rc::Rc::new(std::cell::Cell::new(false));
            let err = render_surface(ran.clone())
                .act("render", &args, None, "b#2", &with_rule(app, action))
                .unwrap_err();
            assert!(err.starts_with("GRANT:"), "a rule for {app}.{action} is not a rule for blender.render: {err}");
            assert!(!ran.get());
        }
    }

    /// The ceiling is the machine's wall and nothing reaches past it: not bypass, not a grant,
    /// not both. The shell will not even raise a card above it, so a grant there is a grant
    /// from a ceiling that has since been tightened — and the tightening wins.
    #[test]
    fn the_ceiling_still_refuses_dangerous_whatever_the_grant_or_mode() {
        for (mode, granted) in [("bypass", false), ("ask", true), ("bypass", true)] {
            let ran = std::rc::Rc::new(std::cell::Cell::new(false));
            let authority = Authority { ceiling: "sensitive".into(), mode: Mode::named(mode), granted };
            let err = delete_surface(ran.clone())
                .act("files_delete", &serde_json::json!({"name": "x"}), None, "shell#1", &authority)
                .unwrap_err();
            assert!(err.starts_with("CEILING:"), "{mode}, granted={granted}: {err}");
            assert!(!ran.get(), "{mode}, granted={granted}: the handler must not have run");
        }
    }

    /// `describe` takes no authority — the signature is the proof — and it reports the grade a
    /// call would be asked for, so a caller can see the cost before paying it.
    #[test]
    fn describe_needs_nothing() {
        let described = render_surface(std::rc::Rc::new(std::cell::Cell::new(false))).describe();
        assert_eq!(described["actions"][0]["name"], serde_json::json!("render"));
        assert_eq!(described["actions"][0]["permission"], serde_json::json!("sensitive"));
    }

    /// A stand-in for the shell's store: `fresh-*` ids are grants that hold once, for exactly
    /// `blender.render {"out": "x.png"}`; anything else is refused in the shell's own words.
    /// Installed once per test binary, because the spender is process-wide as the shell's is,
    /// and only by the tests that spend — every other test never attaches a grant.
    fn spend_through_a_stand_in_shell() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let spent = std::sync::Mutex::new(std::collections::HashSet::<String>::new());
            spend_grants_with(move |id, app, action, args| {
                if !id.starts_with("fresh-") {
                    return Err(format!("no approval request `{id}` — it may have been dropped when the shell restarted. Ask again."));
                }
                if app != "blender" || action != "render" || *args != serde_json::json!({"out": "x.png"}) {
                    return Err(format!(
                        "`{id}` was approved for `blender.render` with arguments {{\"out\":\"x.png\"}}, and this call carries {args}. Nothing was authorised."
                    ));
                }
                let mut spent = spent.lock().unwrap_or_else(|e| e.into_inner());
                if !spent.insert(id.to_string()) {
                    return Err(format!("`{id}` was already used. A grant authorises one action once; this second use authorises nothing."));
                }
                Ok(())
            });
        });
    }

    /// Spend `id` for `blender.render`, graded `sensitive`, under `authority`, the way the RPC
    /// thread does before anything reaches the UI thread.
    fn spend_for_render(mut authority: Authority, id: &str, args: &serde_json::Value) -> Result<Authority, String> {
        authority.spend(id, "blender", "render", "sensitive", args).map(|()| authority)
    }

    /// A grant is spent on the RPC thread, before anything reaches the UI thread: a spent one,
    /// one bound to other arguments, and one that never existed each end the call there, with
    /// the shell's reason in the refusal. Without this, "with a grant it runs" would be "with
    /// any string called grant it runs".
    #[test]
    fn a_spent_or_wrong_grant_is_refused_before_anything_is_dispatched() {
        spend_through_a_stand_in_shell();
        let args = serde_json::json!({"out": "x.png"});

        let first = spend_for_render(open(), "fresh-1", &args).expect("a fresh grant holds");
        assert!(first.granted);

        let err = spend_for_render(open(), "fresh-1", &args).unwrap_err();
        assert!(err.starts_with("GRANT:") && err.contains("already used"), "replayed: {err}");

        let err = spend_for_render(open(), "fresh-2", &serde_json::json!({"out": "y.png"})).unwrap_err();
        assert!(err.starts_with("GRANT:") && err.contains("this call carries"), "swapped: {err}");

        let err = spend_for_render(open(), "made-up", &args).unwrap_err();
        assert!(err.starts_with("GRANT:") && err.contains("no approval request"), "invented: {err}");
    }

    /// #154, item 2: `authority_for` spent the grant before the grade had been looked at, and the
    /// dispatch then refused the act for being above the ceiling — so the person's Allow was
    /// used up on an act that never ran, and could not be offered again. The ceiling comes
    /// first now. Refused above it, the grant is still whole: once the ceiling allows the act,
    /// the same grant holds, once.
    #[test]
    fn a_grant_is_not_spent_on_an_act_the_ceiling_refuses() {
        spend_through_a_stand_in_shell();
        let args = serde_json::json!({"out": "x.png"});

        let err = spend_for_render(under("standard"), "fresh-154", &args).unwrap_err();
        assert!(err.starts_with("CEILING:"), "the ceiling's refusal, not the grant's: {err}");
        assert!(err.contains("graded `sensitive`") && err.contains("`standard` ceiling"), "{err}");

        let raised = spend_for_render(under("sensitive"), "fresh-154", &args)
            .expect("the refusal above the ceiling left the grant unspent");
        assert!(raised.granted);
        let err = spend_for_render(open(), "fresh-154", &args).unwrap_err();
        assert!(err.contains("already used"), "and it still holds only once: {err}");

        // And the dispatch reaches the same answer on the UI thread, whatever was spent: the
        // ceiling is decided again there, on the grade `describe` is showing at that moment.
        let mut granted = under("standard");
        granted.granted = true;
        let ran = std::rc::Rc::new(std::cell::Cell::new(false));
        let err = render_surface(ran.clone())
            .act("render", &args, None, "blender#1", &granted)
            .unwrap_err();
        assert!(err.starts_with("CEILING:"), "{err}");
        assert!(!ran.get());
    }

    /// Same file, same shape as the shell writes (see `mind_mode::policy_json`), and the same
    /// fallback as the bridge: anything unreadable is `ask`, never something looser. The
    /// shell's own test drives its writer through this reader.
    #[test]
    fn the_mode_comes_from_the_file_the_shell_writes() {
        let now = 1_800_000_000;
        assert_eq!(mode_from(r#"{"mode":"auto","session_rules":[]}"#, now).name, "auto");
        let with_rule = mode_from(
            r#"{"mode":"ask","session_rules":[{"app":"calendar","action":"move_event"}]}"#,
            now,
        );
        assert_eq!(with_rule.name, "ask");
        assert_eq!(with_rule.session_rules, vec![("calendar".to_string(), "move_event".to_string())]);

        // Missing, empty, not JSON, or a mode this OS does not define: `ask`.
        for text in ["", "{}", "mode: auto", r#"{"mode":"yolo"}"#, r#"{"mode":"BYPASS"}"#] {
            assert_eq!(mode_from(text, now).name, DEFAULT_MODE, "{text:?}");
        }

        // A bypass with an end honours it: live before, back to what it was after — and a
        // "previous" of bypass, which the shell never writes, comes back as `ask`.
        let bypass = format!(r#"{{"mode":"bypass","previous":"auto","bypass_expires_unix":{}}}"#, now + 60);
        assert_eq!(mode_from(&bypass, now).name, "bypass");
        assert_eq!(mode_from(&bypass, now + 60).name, "auto");
        let odd = format!(r#"{{"mode":"bypass","previous":"bypass","bypass_expires_unix":{}}}"#, now);
        assert_eq!(mode_from(&odd, now).name, DEFAULT_MODE);
        assert_eq!(mode_from(r#"{"mode":"bypass","previous":"ask","bypass_expires_unix":null}"#, now).name, "bypass");

        // And each mode's column of the table: what it runs unasked.
        for (mode, top) in MODES {
            assert_eq!(LADDER[Mode::named(mode).allows()], top, "{mode}");
        }
        assert_eq!(LADDER[Mode::named("yolo").allows()], "standard", "an unknown mode reads as ask");
    }

    // ── Who is calling ──

    #[test]
    fn a_caller_is_current_only_while_its_own_dispatch_runs() {
        // The reason this is a thread-local with a guard rather than a global: two connections
        // can be in flight at once, and a handler must never read the pid of somebody else's
        // request. Outside a scope there is no caller at all — not a stale one.
        assert_eq!(caller(), None, "nothing is calling before anything has called");

        let hermes = Caller { pid: 696, uid: 1000, gid: 1000 };
        {
            let _scope = CallerScope::enter(Some(hermes));
            assert_eq!(caller(), Some(hermes));

            // Nested, because `describe` inside an `act` is a real shape.
            {
                let _inner = CallerScope::enter(Some(Caller { pid: 4242, uid: 1000, gid: 1000 }));
                assert_eq!(caller().map(|c| c.pid), Some(4242));
            }
            assert_eq!(caller(), Some(hermes), "the outer dispatch gets its own caller back");
        }
        assert_eq!(caller(), None, "and nothing is left behind");
    }

    #[test]
    fn a_handler_that_panics_does_not_leave_its_caller_behind() {
        // A leaked caller would be worse than none: the next request on this thread would be
        // attributed to the process that crashed the previous one, and the shell would print
        // that pid on an approval card as a verified fact.
        let panicked = std::panic::catch_unwind(|| {
            let _scope = CallerScope::enter(Some(Caller { pid: 7, uid: 0, gid: 0 }));
            assert_eq!(caller().map(|c| c.pid), Some(7));
            panic!("a handler blew up");
        });
        assert!(panicked.is_err(), "the panic has to actually happen for this to prove anything");
        assert_eq!(caller(), None);
    }

    /// The socket the tests below talk to: one served surface per test binary, because the UI
    /// stand-in is one per binary (see `test_ui_thread`). `who` reports the caller as the handler
    /// sees it; `slow` finishes its answer off the UI thread with [`answer_later`]; `echo` hands back
    /// its arguments and agent token; `nuke` is graded off the ladder, for the ceiling.
    #[cfg(unix)]
    fn served_test_surface() -> &'static str {
        use std::os::unix::net::UnixStream;
        use std::sync::OnceLock;

        static ADDRESS: OnceLock<String> = OnceLock::new();
        ADDRESS.get_or_init(|| {
            const APP: &str = "caller-test";

            // The server binds wherever `XDG_RUNTIME_DIR` points when its thread gets there, and
            // the connect below looks wherever it points then. Nothing else may move it in between
            // — see `env_lock` — and it points at a directory this test owns, not at the runner's
            // `/run/user/<uid>`, which need not exist on a machine with no login session. Once
            // something has connected, the address is a path and the variable no longer matters.
            let _env = crate::env_lock();
            let runtime =
                std::env::temp_dir().join(format!("yantrik-ui-hop-test-{}", std::process::id()));
            std::fs::create_dir_all(&runtime).expect("a runtime dir of our own");
            std::env::set_var("XDG_RUNTIME_DIR", &runtime);

            test_ui_thread::start(Box::new(|| Registry {
                app_id: APP.into(),
                describe: Some(Box::new(|| View::new("caller-test"))),
                actions: vec![
                    (
                        // `safe` so the machine ceiling cannot refuse this on a developer's box
                        // that has tightened `tool_permission`; the ceiling has its own tests above.
                        Action::new("who", "Report who is calling").risk("safe"),
                        Box::new(|_| {
                            // The handler's own view, on the thread the handler actually runs on.
                            // If the caller had been left on the socket thread this would be null.
                            Ok(match caller() {
                                Some(c) => serde_json::json!({ "pid": c.pid, "uid": c.uid }),
                                None => serde_json::Value::Null,
                            })
                        }),
                    ),
                    (
                        Action::new("slow", "Take `ms` milliseconds to answer, off the UI thread")
                            .risk("safe")
                            .arg(Param::number("ms"))
                            .arg(Param::flag("refuse").optional()),
                        Box::new(|args| {
                            let ms = args["ms"].as_u64().unwrap_or(0);
                            let refuse = args["refuse"].as_bool().unwrap_or(false);
                            // Read here, where it is set, and carried into the work.
                            let pid = caller().map(|c| c.pid);
                            let work = move || {
                                std::thread::sleep(Duration::from_millis(ms));
                                if refuse {
                                    return Err(format!("refused after {ms} ms"));
                                }
                                Ok(serde_json::json!({ "slept_ms": ms, "pid": pid }))
                            };
                            answer_later(work)
                                .map(|()| serde_json::json!("replaced by the work's own answer"))
                                .or_else(|work| work())
                        }),
                    ),
                    (
                        // What a handler that records or shows its arguments would record or
                        // show — an approval card, an audit line — and the token beside them.
                        Action::new("echo", "Answer with the arguments and the agent token as the handler got them")
                            .risk("safe")
                            .arg(Param::text("command").optional()),
                        Box::new(|args| {
                            Ok(serde_json::json!({ "args": args, "agent_token": agent_token() }))
                        }),
                    ),
                    (
                        // Graded off the ladder, so the ceiling refuses it whatever the machine
                        // running the tests has in its settings file.
                        Action::new("nuke", "Refused by the ceiling on every machine").risk("catastrophic"),
                        Box::new(|_| Ok(serde_json::json!("never reached"))),
                    ),
                ],
            }));
            serve_rpc(APP, 4);

            // Thirty seconds is a bound on a hung server, not a budget for a slow one: the server
            // binds on its own thread after building a tokio runtime, and the failure this loop
            // used to report was never slowness but the environment race described at `env_lock`.
            let address = RpcServer::default_address(&service_id_for(APP));
            for _ in 0..1000 {
                if UnixStream::connect(&address).is_ok() {
                    return address;
                }
                std::thread::sleep(Duration::from_millis(30));
            }
            panic!("nothing ever bound {address}");
        })
    }

    /// One JSON-RPC line out, one back, on a connection of its own.
    #[cfg(unix)]
    fn call(request: &str) -> serde_json::Value {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixStream;

        let mut socket = UnixStream::connect(served_test_surface()).expect("connect");
        socket.write_all(format!("{request}\n").as_bytes()).expect("write the request");
        let mut line = String::new();
        BufReader::new(socket).read_line(&mut line).expect("read the reply");
        serde_json::from_str(&line).expect(&line)
    }

    /// See `test_ui_thread` for why the hop is a channel.
    #[cfg(unix)]
    #[test]
    fn the_caller_reaches_the_handler_across_the_ui_hop() {
        use std::os::unix::fs::MetadataExt;

        let reply = call(r#"{"jsonrpc":"2.0","id":1,"method":"app.act","params":{"action":"who","args":{}}}"#);
        let seen = &reply["result"]["result"];
        assert!(
            !seen.is_null(),
            "the handler saw no caller at all — the credentials did not cross the hop: {reply}"
        );
        assert_eq!(
            seen["pid"].as_u64(),
            Some(u64::from(std::process::id())),
            "the kernel's pid for this connection is this test process: {reply}"
        );
        // The uid the kernel reported has to be the uid that owns the socket — this test is both
        // ends of the connection, so anything else means the field is not the peer's.
        let owner = std::fs::metadata(served_test_surface()).expect("the socket exists").uid();
        assert_eq!(seen["uid"].as_u64(), Some(u64::from(owner)), "{reply}");
    }

    /// The shell's `agent_run` owes its caller an exit code that may be minutes away. The handler
    /// hands the wait to `answer_later`; the caller gets the work's own result, and in the
    /// meantime the socket and the UI thread both go on answering everybody else.
    #[cfg(unix)]
    #[test]
    fn an_answer_that_takes_time_is_finished_off_the_ui_thread_and_holds_up_nobody() {
        use std::time::Instant;

        served_test_surface();
        let asked = Instant::now();
        let slow = std::thread::spawn(|| {
            call(r#"{"jsonrpc":"2.0","id":1,"method":"app.act","params":{"action":"slow","args":{"ms":1500}}}"#)
        });

        // While that one waits: another caller, another connection, served at once. `describe`
        // runs on the UI stand-in, so this also shows the UI thread is not the one waiting.
        std::thread::sleep(Duration::from_millis(200));
        let glance = Instant::now();
        let described = call(r#"{"jsonrpc":"2.0","id":2,"method":"app.describe","params":{}}"#);
        assert_eq!(described["result"]["app"], "caller-test", "{described}");
        assert!(
            glance.elapsed() < Duration::from_millis(700),
            "a describe waited {:?} behind a slow act",
            glance.elapsed()
        );

        let reply = slow.join().expect("the slow call");
        assert!(asked.elapsed() >= Duration::from_millis(1500), "the reply is the finished work");
        assert_eq!(reply["result"]["accepted"], true, "{reply}");
        assert_eq!(reply["result"]["result"]["slept_ms"], 1500, "the work's result, not the handler's: {reply}");
        assert_eq!(
            reply["result"]["result"]["pid"].as_u64(),
            Some(u64::from(std::process::id())),
            "the caller read in the handler reached the work: {reply}"
        );
        assert!(reply["result"]["revision"].as_str().is_some(), "the envelope keeps its view: {reply}");

        // Work that refuses is refused to the caller, as a handler's refusal would be.
        let refused = call(
            r#"{"jsonrpc":"2.0","id":3,"method":"app.act","params":{"action":"slow","args":{"ms":10,"refuse":true}}}"#,
        );
        assert_eq!(refused["error"]["message"], "refused after 10 ms", "{refused}");
        assert_eq!(refused["error"]["code"], -32602, "an application refusal, not a transport fault");
    }

    /// The token rides beside `args` and reaches the handler through `agent_token()`; `args` —
    /// what an approval card shows and an audit line keeps — never holds it, even when a caller
    /// puts it there.
    #[cfg(unix)]
    #[test]
    fn an_agent_token_reaches_the_handler_beside_the_arguments_and_never_among_them() {
        let reply = call(
            r#"{"jsonrpc":"2.0","id":1,"method":"app.act","params":{"action":"echo","args":{"command":"ls"},"agent_token":"tok-7f3a"}}"#,
        );
        let seen = &reply["result"]["result"];
        assert_eq!(seen["agent_token"], "tok-7f3a", "the handler reads the token: {reply}");
        assert_eq!(seen["args"], serde_json::json!({"command": "ls"}), "and its args are only args: {reply}");

        // Smuggled inside `args` as well: taken out, not used, and not refused as an undeclared
        // argument either — the call goes on as if it had never been there.
        let reply = call(
            r#"{"jsonrpc":"2.0","id":2,"method":"app.act","params":{"action":"echo","args":{"command":"ls","agent_token":"smuggled"},"agent_token":"tok-7f3a"}}"#,
        );
        assert_eq!(reply["result"]["result"]["args"], serde_json::json!({"command": "ls"}), "{reply}");
        assert_eq!(reply["result"]["result"]["agent_token"], "tok-7f3a", "the one beside args wins: {reply}");
        assert!(!reply.to_string().contains("smuggled"), "nothing in the reply carries it: {reply}");

        // Only inside `args`: stripped, and the handler sees no token at all.
        let reply = call(
            r#"{"jsonrpc":"2.0","id":3,"method":"app.act","params":{"action":"echo","args":{"agent_token":"smuggled"}}}"#,
        );
        assert_eq!(reply["result"]["result"]["args"], serde_json::json!({}), "{reply}");
        assert!(reply["result"]["result"]["agent_token"].is_null(), "{reply}");

        // No token, no token.
        let reply = call(r#"{"jsonrpc":"2.0","id":4,"method":"app.act","params":{"action":"echo","args":{}}}"#);
        assert!(reply["result"]["result"]["agent_token"].is_null(), "{reply}");
    }

    #[test]
    fn an_agent_token_is_current_only_while_its_own_dispatch_runs() {
        assert_eq!(agent_token(), None);
        {
            let _outer = AgentTokenScope::enter(Some("tok-a".into()));
            assert_eq!(agent_token().as_deref(), Some("tok-a"));
            {
                let _inner = AgentTokenScope::enter(None);
                assert_eq!(agent_token(), None, "a nested call without one has none");
            }
            assert_eq!(agent_token().as_deref(), Some("tok-a"));
        }
        assert_eq!(agent_token(), None, "and nothing is left behind for the next dispatch");

        let mut args = serde_json::json!({"command": "ls", "agent_token": "x"});
        let params = serde_json::json!({"agent_token": "  tok-b  "});
        assert_eq!(agent_token_of(&params, &mut args).as_deref(), Some("tok-b"));
        assert_eq!(args, serde_json::json!({"command": "ls"}));
        let mut args = serde_json::json!({});
        assert_eq!(agent_token_of(&serde_json::json!({"agent_token": " "}), &mut args), None, "blank is none");
    }

    #[test]
    fn a_handler_called_directly_is_handed_its_work_back_to_run_itself() {
        // No socket, no dispatch: nothing would run the work, so it comes back.
        let back = answer_later(|| Ok(serde_json::json!("ran inline")));
        let work = back.err().expect("no dispatch is in progress on this thread");
        assert_eq!(work(), Ok(serde_json::json!("ran inline")));

        // And inside a dispatch's scope it is kept, once, for the dispatch to finish.
        let scope = LaterScope::enter();
        assert!(answer_later(|| Ok(serde_json::json!(1))).is_ok());
        let kept = scope.take().expect("the work was kept");
        assert_eq!(kept(), Ok(serde_json::json!(1)));
        assert!(scope.take().is_none(), "taken once");
        drop(scope);
        assert!(answer_later(|| Ok(serde_json::json!(2))).is_err(), "the scope closed with the dispatch");
    }

    /// #154 through the real dispatch: the RPC thread asks the UI thread for the grade before it
    /// offers a grant to the shell, so an act the ceiling refuses, and an action the app does not
    /// have, spend nothing. Before, both came back `GRANT: … does not authorise …` — the grant had
    /// already gone to the shell by the time anything looked at the action.
    #[cfg(unix)]
    #[test]
    fn a_grant_on_the_socket_is_offered_to_the_shell_only_past_the_ceiling() {
        spend_through_a_stand_in_shell();
        let act = |action: &str, args: serde_json::Value, grant: &str| {
            let request = serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "method": "app.act",
                "params": { "action": action, "args": args, "grant": grant },
            });
            let reply = call(&request.to_string());
            reply["error"]["message"].as_str().unwrap_or_default().to_string()
        };

        let err = act("nuke", serde_json::json!({}), "fresh-socket");
        assert!(err.starts_with("CEILING:"), "the ceiling's answer, not the shell's: {err}");

        let err = act("nope", serde_json::json!({}), "fresh-socket");
        assert!(err.starts_with("unknown action `nope`"), "the app's answer, not the shell's: {err}");

        // Past the ceiling the grant does go to the shell, and one that does not hold ends the
        // call in the shell's words.
        let err = act("who", serde_json::json!({}), "made-up");
        assert!(err.starts_with("GRANT:") && err.contains("no approval request"), "{err}");

        // What a grant is spent against is the arguments with any agent token a caller put among
        // them already lifted off (see `agent_token`): the shell is shown `{"command":"ls"}` and
        // the grant is bound to that, never to a token. The stand-in shell names what it was
        // handed, which is how this can be seen from here.
        let err = act("echo", serde_json::json!({"command": "ls", "agent_token": "smuggled"}), "fresh-echo");
        assert!(err.starts_with("GRANT:") && err.contains(r#"this call carries {"command":"ls"}"#), "{err}");
        assert!(!err.contains("smuggled"), "the token reached the shell as an argument: {err}");

        // And the grant the two refusals carried was never spent.
        spend_for_render(open(), "fresh-socket", &serde_json::json!({"out": "x.png"}))
            .expect("nothing spent `fresh-socket` on the way to either refusal");
    }

    #[test]
    fn every_dispatch_gets_its_own_name() {
        let first = next_action_id("app-notes");
        let second = next_action_id("app-notes");
        assert_ne!(first, second, "two waits must not key on the same id");
        assert!(first.starts_with("app-notes#"), "{first}");
    }
}
