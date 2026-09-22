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
    ("calendar", &[]),
    ("containers", &["container-manager"]),
    ("documents", &["document-editor"]),
    ("download-manager", &["downloads"]),
    ("editor", &["text-editor"]),
    ("email", &[]),
    ("image-viewer", &["images"]),
    ("network", &["network-manager"]),
    ("notes", &[]),
    ("presentation", &["slides"]),
    // Nothing "opens" the desktop, so it is in no launcher table; it does publish a surface, and
    // its own notifications' buttons and approval cards have to reach it.
    ("shell", &[]),
    ("snippets", &["snippet-manager"]),
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

// ── The ceiling ─────────────────────────────────────────────────────

/// The grades an action can carry, lowest first. The same ladder the MCP bridge and the
/// companion's `parse_permission` use; held as strings here because an [`Action`]'s own
/// `permission` is a `&'static str` and this crate must not grow a dependency to compare it.
pub const LADDER: [&str; 4] = ["safe", "standard", "sensitive", "dangerous"];

/// Where a grade sits on [`LADDER`], or `None` if it is not a level this OS defines.
fn grade(permission: &str) -> Option<usize> {
    LADDER.iter().position(|g| *g == permission)
}

/// The ceiling used when `settings.yaml` is missing, unreadable, or says nothing usable —
/// the same default the shell's own `UserSettings` carries, so a machine that has never
/// opened Settings behaves the way Settings would show it.
const DEFAULT_CEILING: &str = "sensitive";

/// The machine's ceiling for programmatic callers, from the shell's settings file.
///
/// Read per call rather than cached at `serve()`: the whole point of the setting is that a
/// person can tighten it while apps are running, and a boundary that only notices at launch
/// is a boundary the Settings screen lies about. The file is a few hundred bytes and an
/// `act` happens at human-or-model speed, so the read costs nothing that matters. It happens
/// on the RPC thread — the dispatch closure runs where windows are painted, and file IO
/// does not belong there.
pub fn configured_ceiling() -> String {
    let Ok(text) = std::fs::read_to_string(crate::theme::settings_path()) else {
        return DEFAULT_CEILING.to_string();
    };
    ceiling_from(&text)
}

/// Pull `tool_permission` out of settings text. Only that key is parsed, for the same reason
/// `theme::parse` only parses its two: the rest of the file is the shell's business.
fn ceiling_from(text: &str) -> String {
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else { continue };
        if key.trim() != "tool_permission" {
            continue;
        }
        let value = value.trim().trim_matches('"').trim_matches('\'');
        if grade(value).is_some() {
            return value.to_string();
        }
        tracing::warn!(value = %value, "tool_permission is not a grade; using {DEFAULT_CEILING}");
        return DEFAULT_CEILING.to_string();
    }
    DEFAULT_CEILING.to_string()
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

    /// Check the ceiling, check the guard, dispatch, and read what came of it — without leaving
    /// the UI thread.
    ///
    /// These steps are one function because they have to be one turn of the event loop. Split
    /// across RPC calls, the gap between the check and the dispatch is a window in which the user
    /// can type, and the gap between the dispatch and the read is a window in which they can undo
    /// it. Here nothing runs in between, because there is no in between: this is the thread that
    /// would have to run it.
    ///
    /// `ceiling` arrives as an argument, already read from settings by the RPC thread (see
    /// [`configured_ceiling`]), so the boundary is enforced in the dispatch itself — the one
    /// function every `app.act` crosses, whoever sent it — while the file IO stays off the UI
    /// thread and tests can pin the ceiling instead of inheriting the developer's.
    fn act(
        &self,
        name: &str,
        args: &serde_json::Value,
        expect_revision: Option<&str>,
        action_id: &str,
        ceiling: &str,
    ) -> Result<serde_json::Value, String> {
        let Some((spec, run)) = self.actions.iter().find(|(a, _)| a.name == name) else {
            let known: Vec<&str> = self.actions.iter().map(|(a, _)| a.name.as_str()).collect();
            return Err(format!("unknown action `{name}`; this app offers: {}", known.join(", ")));
        };

        // The ceiling, before anything else about this call is even looked at. It refuses on the
        // grade alone — before the arguments are checked, before the revision guard, and long
        // before the handler — because "may this caller use this action at all" is a question
        // about the action, and answering any narrower question first would mean doing work for
        // a call that was never allowed. An unrecognised ceiling falls back to the default rather
        // than failing open: the same choice the companion's `parse_permission` makes.
        let Some(level) = grade(spec.permission) else {
            return Err(format!(
                "CEILING: {}.{} is graded `{}`, which is not a level this OS defines ({}), \
                 so it was not run.",
                self.app_id,
                name,
                spec.permission,
                LADDER.join(" < ")
            ));
        };
        let cap = grade(ceiling).unwrap_or_else(|| grade(DEFAULT_CEILING).unwrap());
        if level > cap {
            return Err(format!(
                "CEILING: {app}.{name} is graded `{perm}`, above this machine's `{ceiling}` \
                 ceiling (`tool_permission` in ~/.config/yantrik/settings.yaml), so it was not \
                 run. An action at that grade needs a person to authorise it directly — raise \
                 the ceiling in Settings if that is the intent.",
                app = self.app_id,
                perm = spec.permission
            ));
        }

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
    REGISTRY.with(|cell| {
        cell.borrow().as_ref().and_then(|reg| {
            reg.actions.iter().find(|(a, _)| a.name == action).map(|(a, _)| a.permission)
        })
    })
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
    let (tx, rx) = mpsc::sync_channel::<Result<T, String>>(1);
    post_to_ui(Box::new(move || {
        let _scope = CallerScope::enter(who);
        let answer = REGISTRY.with(|cell| match cell.borrow().as_ref() {
            Some(reg) => Ok(job(reg)),
            None => Err("this app published no control surface".to_string()),
        });
        // The receiver is gone only if we already timed out; dropping the answer is correct.
        let _ = tx.send(answer);
    }))?;

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

    /// The transport's older entry point. Kept so the trait is satisfied for any caller that
    /// still uses it; it means "nobody said who was calling", which is exactly what `None` is.
    fn handle(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, ServiceError> {
        self.dispatch(method, params, None)
    }

    fn handle_from(
        &self,
        method: &str,
        params: serde_json::Value,
        peer: Option<PeerCred>,
    ) -> Result<serde_json::Value, ServiceError> {
        self.dispatch(
            method,
            params,
            peer.map(|p| Caller { pid: p.pid, uid: p.uid, gid: p.gid }),
        )
    }
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
                let args = params.get("args").cloned().unwrap_or(serde_json::json!({}));
                // Optional, and deliberately so: a caller acting on its own initiative has nothing
                // to compare against, and demanding a revision it never read would only teach it
                // to send back whatever it last saw.
                let expect = params
                    .get("expect_revision")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let action_id = next_action_id(&self.service_id);

                // Read on this thread, enforced on the UI one: the settings file is IO and the
                // dispatch closure is a turn of the event loop.
                let ceiling = configured_ceiling();
                tracing::info!(
                    action = %action,
                    id = %action_id,
                    ceiling = %ceiling,
                    // Logged as a pair so a line in the journal says who as well as what. The
                    // audit log is the shell's job; this is the runtime's own record.
                    caller_pid = who.map(|c| c.pid).unwrap_or(0),
                    caller_uid = who.map(|c| c.uid).unwrap_or(0),
                    "app.act"
                );
                let id = action_id.clone();
                let outcome = on_ui_thread(who, move |reg| {
                    reg.act(&action, &args, expect.as_deref(), &id, &ceiling)
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
        link_other_names(app_id);
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
    /// ceiling. The boundary has its own tests below, with the ceiling pinned per case rather
    /// than inherited from whatever `settings.yaml` the machine running them happens to have.
    const OPEN: &str = "dangerous";

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

        let err = reg.act("open_note", &serde_json::json!({}), None, "t#1", OPEN).unwrap_err();
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
            .act("open_note", &serde_json::json!({"title": "a", "colour": "red"}), None, "t#1", OPEN)
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
            .act("new_note", &serde_json::json!({"title": "Handover"}), None, "t#1", OPEN)
            .unwrap_err();
        assert!(err.contains("takes no arguments"), "{err}");
        assert!(err.contains("title"), "{err}");

        // And the no-argument call it was always meant to accept still works.
        assert!(reg.act("new_note", &serde_json::json!({}), None, "t#2", OPEN).is_ok());
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

        let err = reg.act("nope", &serde_json::json!({}), None, "t#1", OPEN).unwrap_err();
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
            .act("rename", &serde_json::json!({ "to": "Kernel answers" }), None, "notes#1", OPEN)
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

        let answer = reg.act("build", &serde_json::json!({}), None, "builder#1", OPEN).unwrap();
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
            .act("rename", &serde_json::json!({ "to": "x" }), Some(&stale), "notes#1", OPEN)
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
            .act("rename", &serde_json::json!({ "to": "ok" }), Some(&current), "notes#1", OPEN)
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
            .act("rename", &serde_json::json!({ "to": "ok" }), None, "notes#1", OPEN)
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

        let err = reg.act("go", &serde_json::json!({}), Some("0000000000000000"), "n#1", OPEN);
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
            .act("files_delete", &serde_json::json!({"name": "x"}), None, "shell#1", "sensitive")
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

        let err = reg.act("files_delete", &serde_json::json!({}), None, "shell#1", "sensitive").unwrap_err();
        assert!(err.starts_with("CEILING:"), "not the missing-argument error: {err}");
        assert!(!err.contains("needs argument"), "{err}");
    }

    #[test]
    fn an_action_at_or_below_the_ceiling_runs() {
        // "At or below" is the whole contract; the ceiling is not a blanket refusal.
        let ran = std::rc::Rc::new(std::cell::Cell::new(false));
        let reg = delete_surface(ran.clone());
        let answer = reg
            .act("files_delete", &serde_json::json!({"name": "x"}), None, "shell#1", "dangerous")
            .unwrap();
        assert_eq!(answer["accepted"], true);
        assert!(ran.get());

        // And the everyday case: a `standard` action under the shipped `sensitive` default.
        let answer = notes_at("Kernel asks")
            .act("rename", &serde_json::json!({ "to": "ok" }), None, "notes#1", "sensitive")
            .unwrap();
        assert_eq!(answer["accepted"], true);
    }

    #[test]
    fn a_ceiling_tightened_to_safe_binds_the_default_actions_too() {
        // The setting has to actually tighten, not only refuse the graded-dangerous few: every
        // action floors at `standard`, so `safe` closes the door to programmatic callers entirely.
        let err = notes_at("Kernel asks")
            .act("rename", &serde_json::json!({ "to": "ok" }), None, "notes#1", "safe")
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

        let err = reg.act("nuke", &serde_json::json!({}), None, "notes#1", OPEN).unwrap_err();
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

    /// The one test with a real socket in it. See `test_ui_thread` for why the hop is a channel.
    #[cfg(unix)]
    #[test]
    fn the_caller_reaches_the_handler_across_the_ui_hop() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::net::UnixStream;

        const APP: &str = "caller-test";

        // The server binds wherever `XDG_RUNTIME_DIR` points when its thread gets there, and the
        // connect below looks wherever it points then. Nothing else may move it in between —
        // see `env_lock` — and it points at a directory this test owns, not at the runner's
        // `/run/user/<uid>`, which need not exist on a machine with no login session.
        let _env = crate::env_lock();
        let runtime = std::env::temp_dir().join(format!("yantrik-ui-hop-test-{}", std::process::id()));
        std::fs::create_dir_all(&runtime).expect("a runtime dir of our own");
        std::env::set_var("XDG_RUNTIME_DIR", &runtime);

        test_ui_thread::start(Box::new(|| Registry {
            app_id: APP.into(),
            describe: Some(Box::new(|| View::new("caller-test"))),
            actions: vec![(
                // `safe` so the machine ceiling cannot refuse this on a developer's box that
                // has tightened `tool_permission`; the ceiling has its own tests above.
                Action::new("who", "Report who is calling").risk("safe"),
                Box::new(|_| {
                    // The handler's own view, on the thread the handler actually runs on. If
                    // the caller had been left on the socket thread this would be null.
                    Ok(match caller() {
                        Some(c) => serde_json::json!({ "pid": c.pid, "uid": c.uid }),
                        None => serde_json::Value::Null,
                    })
                }),
            )],
        }));
        serve_rpc(APP, 1);

        // Thirty seconds is a bound on a hung server, not a budget for a slow one: the server
        // binds on its own thread after building a tokio runtime, and the failure this loop used
        // to report was never slowness but the environment race described at `env_lock`.
        let address = RpcServer::default_address(&service_id_for(APP));
        let mut socket = None;
        for _ in 0..1000 {
            if let Ok(s) = UnixStream::connect(&address) {
                socket = Some(s);
                break;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        let mut socket = socket.unwrap_or_else(|| panic!("nothing ever bound {address}"));

        socket
            .write_all(
                b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"app.act\",\
                  \"params\":{\"action\":\"who\",\"args\":{}}}\n",
            )
            .expect("write the request");
        let mut line = String::new();
        BufReader::new(socket.try_clone().expect("clone the socket"))
            .read_line(&mut line)
            .expect("read the reply");

        let reply: serde_json::Value = serde_json::from_str(&line).expect(&line);
        let seen = &reply["result"]["result"];
        assert!(
            !seen.is_null(),
            "the handler saw no caller at all — the credentials did not cross the hop: {line}"
        );
        assert_eq!(
            seen["pid"].as_u64(),
            Some(u64::from(std::process::id())),
            "the kernel's pid for this connection is this test process: {line}"
        );
        // The uid the kernel reported has to be the uid that owns the socket — this test is both
        // ends of the connection, so anything else means the field is not the peer's.
        let owner = std::fs::metadata(&address).expect("the socket exists").uid();
        assert_eq!(seen["uid"].as_u64(), Some(u64::from(owner)), "{line}");
    }

    #[test]
    fn every_dispatch_gets_its_own_name() {
        let first = next_action_id("app-notes");
        let second = next_action_id("app-notes");
        assert_ne!(first, second, "two waits must not key on the same id");
        assert!(first.starts_with("app-notes#"), "{first}");
    }
}
