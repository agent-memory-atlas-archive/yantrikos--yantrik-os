//! Which mind is answering — the shell's side of it.
//!
//! Three jobs. Wrap the companion so it is one candidate among others rather than the only one.
//! Serve the `harness` socket so anything else can attach. Keep the Settings screen and the
//! status bar showing the truth about both.
//!
//! The [`Host`] lives for the life of the shell and is reachable from anywhere in it through
//! [`host`], because two things need it that cannot hand each other a reference: this wiring, and
//! the shell's own control surface in `crate::control`.

use std::sync::{Arc, OnceLock};

use slint::{ComponentHandle, Model, ModelRc, Timer, TimerMode, VecModel};
use yantrik_harness::{Answer, Capabilities, Chunk, Harness, Health, Host, Turn};

use crate::app_context::AppContext;
use crate::bridge::CompanionBridge;
use crate::harness_catalogue::{self, Manifest};
use crate::{App, HarnessData, HarnessRowData};

/// How often the list is refreshed.
///
/// Harnesses arrive and leave on their own, so a list that only updated when the screen opened
/// would show one that left ten minutes ago. Two seconds is below noticing and costs a lock and a
/// few string clones.
const REFRESH: std::time::Duration = std::time::Duration::from_secs(2);

/// The screen and the section the catalogue is drawn on — `settings`, and `Harnesses` inside it.
///
/// The catalogue costs a directory walk, a handful of `stat` calls and one `systemctl show`, all
/// of which are free once and wasteful as a habit: on a machine nobody is touching it would be a
/// process spawn every two seconds forever. So it is only gathered while somebody is looking at
/// it, while a job this shell started is still running, or once at the start so the first open is
/// not blank. Everything else on this page — the picker, the status bar — needs only the attach
/// registry, which is in memory.
const SETTINGS_SCREEN: i32 = 7;
const HARNESSES_SECTION: i32 = 8;

static HOST: OnceLock<Host> = OnceLock::new();

/// The shell's harness host. Available once [`wire`] has run.
pub fn host() -> Option<&'static Host> {
    HOST.get()
}

// ── The companion, as one harness among others ──────────────────────

/// The built-in mind.
///
/// It is not reached over the protocol — it lives in this process and always has — so it is
/// wrapped rather than ported. That is the whole reason [`Harness`] still exists as a trait: for
/// the one mind that is compiled in. Everything else attaches.
///
/// Named once here so the chat path can ask "is the builtin driving?" without a string literal
/// of its own drifting away from this one.
pub const BUILTIN_ID: &str = "companion";

struct Companion {
    bridge: Arc<CompanionBridge>,
}

impl Harness for Companion {
    fn id(&self) -> &str {
        BUILTIN_ID
    }

    fn name(&self) -> &str {
        "Yantrik Companion"
    }

    fn capabilities(&self) -> Capabilities {
        // The only mind here with the OS's tools and its memory, because it is the only one
        // inside the process that owns them.
        Capabilities { streaming: true, tools: true, memory: true }
    }

    fn health(&self) -> Health {
        Health::Ready
    }

    fn send(&self, turn: Turn) -> Answer {
        let tokens = self.bridge.send_message(turn.text);
        let (tx, rx) = std::sync::mpsc::channel();
        // A thread rather than draining here: send() must return at once so the panel can start
        // rendering, and the companion's channel produces for as long as the model is talking.
        std::thread::Builder::new()
            .name("harness-companion".into())
            .spawn(move || {
                for token in tokens {
                    if tx.send(Chunk::Text(token)).is_err() {
                        return; // the panel stopped listening
                    }
                }
            })
            .ok();
        rx
    }
}

// ── Wiring ──────────────────────────────────────────────────────────

pub fn wire(ui: &App, ctx: &AppContext) {
    let host = Host::new(vec![Arc::new(Companion { bridge: ctx.bridge.clone() })]);
    let _ = HOST.set(host.clone());

    serve_socket(host.clone());

    // Choosing a mind, from Settings or from anywhere else that offers it.
    {
        let weak = ui.as_weak();
        let host = host.clone();
        ui.on_use_harness(move |id| {
            let Some(ui) = weak.upgrade() else { return };
            match host.set_active(&id) {
                Ok(()) => {
                    tracing::info!(harness = %id, "Now answering");
                    // Remembered, because choosing a mind is a decision about the machine and
                    // not about this run of the shell. It was not: the id lived in the host and
                    // the host lives with the process, so every update, crash or reboot handed
                    // the conversation back to the built-in without saying so — and the picker
                    // still showed the right name until you looked.
                    crate::wire::settings::set_preferred_mind(&id);
                    ui.set_harness_error("".into());
                }
                // Shown rather than logged: the person just clicked something and is owed an
                // answer about whether it worked.
                Err(e) => ui.set_harness_error(e.into()),
            }
            publish(&ui, &host);
        });
    }

    // Installing a mind, and starting one whose unit is merely stopped. Both change the machine,
    // so both are jobs: the row says what is happening and streams what the command says, rather
    // than freezing the settings screen for the half minute an `npm install -g` takes.
    {
        let weak = ui.as_weak();
        let host = host.clone();
        ui.on_install_harness(move |id| {
            let Some(ui) = weak.upgrade() else { return };
            let outcome = manifest(&id).and_then(|m| crate::harness_install::install(&m));
            report(&ui, &id, outcome);
            // Straight away rather than on the next tick: two seconds between pressing a button
            // and the row changing is two seconds in which it looks like nothing happened, and
            // that is exactly how a button gets pressed twice.
            publish(&ui, &host);
        });
    }
    {
        let weak = ui.as_weak();
        let host = host.clone();
        ui.on_start_harness(move |id| {
            let Some(ui) = weak.upgrade() else { return };
            let outcome = manifest(&id).and_then(|m| crate::harness_install::start(&m));
            report(&ui, &id, outcome);
            publish(&ui, &host);
        });
    }

    publish(ui, &host);

    let timer = Timer::default();
    {
        let weak = ui.as_weak();
        let host = host.clone();
        timer.start(TimerMode::Repeated, REFRESH, move || {
            let Some(ui) = weak.upgrade() else { return };
            restore_choice(&host);
            publish(&ui, &host);
        });
    }
    // Keep timer alive
    std::mem::forget(timer);
}

/// The Settings list, for `describe shell`.
///
/// Read on demand rather than from what the screen last published: a `describe` is asked a
/// question and can afford one `systemctl show`, and answering from a cache that only refreshes
/// while a person has the page open would answer "not installed" about a harness installed ten
/// minutes ago.
pub fn catalogue_for_describe() -> serde_json::Value {
    let entries = match host() {
        Some(host) => host.list(),
        None => Vec::new(),
    };
    let machine = harness_catalogue::machine(crate::harness_install::views());
    serde_json::Value::Array(
        harness_catalogue::rows(&machine, &entries)
            .iter()
            .map(|row| {
                serde_json::json!({
                    "id": row.id,
                    "name": row.name,
                    // The key rather than the label: one is matched on by a program and the
                    // other is read by a person, and conflating them is how "needs setup"
                    // becomes a string comparison against a UI string.
                    "state": row.state.key(),
                    "detail": row.detail,
                    // What to do next, in the same words the row shows. Never a credential:
                    // this names a file at most.
                    "need": row.need,
                    "can_answer": row.state.can_answer(),
                    "can_install": row.can_install,
                    "can_start": row.can_start,
                    "builtin": row.builtin,
                    "docs": row.docs,
                })
            })
            .collect(),
    )
}

/// The manifest for an id, or a sentence saying there is none.
fn manifest(id: &str) -> Result<Manifest, String> {
    harness_catalogue::read_manifests(&harness_catalogue::roots())
        .remove(id)
        .ok_or_else(|| format!("nothing on this machine describes a harness called `{id}`"))
}

/// The same two jobs the buttons start, for the shell's control surface.
///
/// Parity, the same way `use_harness` has it: anything a person can do on the Harnesses screen
/// an agent can ask for, and the grading on the action is what decides whether the person is
/// asked first.
pub fn install(id: &str) -> Result<String, String> {
    crate::harness_install::install(&manifest(id)?)
}

pub fn start(id: &str) -> Result<String, String> {
    crate::harness_install::start(&manifest(id)?)
}

/// Say what happened where the person is looking.
///
/// A refusal goes on the page rather than into the log for the same reason the picker's does:
/// somebody just pressed a button and is owed an answer about whether it worked. The command that
/// was started is logged, never shown — it is long, and the row is already streaming its output.
fn report(ui: &App, id: &str, outcome: Result<String, String>) {
    match outcome {
        Ok(command) => {
            tracing::info!(harness = %id, command = %command, "started a harness job");
            ui.set_harness_error("".into());
        }
        Err(e) => ui.set_harness_error(format!("{id}: {e}").into()),
    }
}

/// Give the conversation back to the mind the person chose, once it is there to take it.
///
/// Not at startup: at startup the only mind on this machine is the built-in one, because a
/// harness exists by attaching and nothing has attached yet. A remembered choice therefore
/// cannot be honoured when it is read — only when the thing it names turns up, which may be
/// seconds after boot or minutes, and which is exactly what this timer is already watching for.
///
/// Silent when there is nothing to do, and it does not fight the person: choosing any mind
/// saves that choice, so switching back to the built-in makes the built-in the preference.
fn restore_choice(host: &Host) {
    let want = crate::wire::settings::preferred_mind();
    if want.is_empty() || host.active_id() == want {
        return;
    }
    if !host.list().iter().any(|e| e.id == want) {
        return;
    }
    match host.set_active(&want) {
        Ok(()) => tracing::info!(harness = %want, "Answering again with the chosen mind"),
        Err(e) => tracing::warn!(harness = %want, error = %e, "Could not restore the chosen mind"),
    }
}

/// Put the current list of minds in front of the person.
fn publish(ui: &App, host: &Host) {
    let entries = host.list();
    let active = host.active_id();

    let rows: Vec<HarnessData> = entries
        .iter()
        .map(|e| HarnessData {
            id: e.id.clone().into(),
            name: e.name.clone().into(),
            detail: e.detail.clone().unwrap_or_default().into(),
            builtin: e.builtin,
            active: e.active,
            tools: e.capabilities.tools,
            memory: e.capabilities.memory,
            status: if e.active {
                "answering".into()
            } else if e.builtin {
                "built in".into()
            } else {
                "attached".into()
            },
        })
        .collect();

    if let Some(model) = crate::models::changed(ui.get_harnesses(), rows) {
        ui.set_harnesses(model);
    }
    ui.set_harness_count(entries.len() as i32);

    // The status bar shows the NAME, not the id: it is read at a glance by a person, and `mind`
    // beside the clock says less than "Yantrik Mind".
    let name = entries
        .iter()
        .find(|e| e.active)
        .map(|e| e.name.clone())
        .unwrap_or_else(|| "no mind".to_string());
    ui.set_active_harness_name(name.into());
    ui.set_active_harness_id(active.into());

    // What the answering mind says it is running on — its model, its memory, wherever it lives.
    // Only an attached one has this; the built-in's model is the shell's own configuration and
    // the rail keeps showing that when there is nothing better. The two are not interchangeable:
    // with a harness driving, the rail read "qwen3.5:9b" from a settings file while every answer
    // came from a different model on a different machine.
    let driving = entries.iter().find(|e| e.active && !e.builtin);
    let detail = driving.and_then(|e| e.detail.clone()).unwrap_or_default();
    ui.set_active_harness_detail(detail.into());
    // Separate from the detail above, because a harness may attach without saying what it runs
    // on. "Something else is answering" is true either way, and it is what the status bar needs
    // in order to stop advertising the shell's own provider as the thing doing the work.
    ui.set_harness_driving(driving.is_some());

    publish_catalogue(ui, &entries);
}

/// Put the Settings list in front of the person: every mind this machine could have.
///
/// Deliberately not the same list as above. That one is the picker and holds only what can be
/// handed a turn; this one is what a person opens *because* a mind is missing, and its whole
/// point is the rows that are not attached.
fn publish_catalogue(ui: &App, entries: &[yantrik_harness::Entry]) {
    let busy = crate::harness_install::busy();
    let looking = ui.get_current_screen() == SETTINGS_SCREEN
        && ui.get_settings_category() == HARNESSES_SECTION;
    // The first pass always runs, so the page is populated before anyone can navigate to it.
    let first = ui.get_harness_rows().row_count() == 0;
    if !looking && !busy && !first {
        return;
    }

    // A job's outcome stops being news once the harness it was for is answering questions. The
    // row is about the present, and "install finished" on a mind that is now attached is the
    // page still talking about five minutes ago.
    let attached: Vec<String> = entries.iter().map(|e| e.id.clone()).collect();
    crate::harness_install::clear_settled(&attached);

    let machine = harness_catalogue::machine(crate::harness_install::views());
    let rows: Vec<HarnessRowData> = harness_catalogue::rows(&machine, entries)
        .into_iter()
        .map(|row| HarnessRowData {
            id: row.id.into(),
            name: row.name.into(),
            detail: row.detail.into(),
            state: row.state.label().into(),
            need: row.need.into(),
            log: row.log.into(),
            builtin: row.builtin,
            active: row.active,
            attached: row.attached,
            busy: row.busy,
            tools: row.tools,
            memory: row.memory,
            can_install: row.can_install,
            can_start: row.can_start,
            docs: row.docs.into(),
        })
        .collect();

    if let Some(model) = crate::models::changed(ui.get_harness_rows(), rows) {
        ui.set_harness_rows(model);
    }
    // Drives the one animation on the page, and only while something is really running.
    ui.set_harness_busy(busy);
}

/// Serve the `harness` socket for the life of the shell.
fn serve_socket(host: Host) {
    std::thread::Builder::new()
        .name("harness-socket".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(e) => {
                    tracing::warn!(error = %e, "No runtime; nothing can attach as a mind");
                    return;
                }
            };
            runtime.block_on(async {
                let address =
                    yantrik_ipc_transport::server::RpcServer::default_address("harness");
                tracing::info!(address = %address, "Harness socket listening (attach to answer)");
                let server = yantrik_ipc_transport::server::RpcServer::new(&address);
                if let Err(e) = server.serve(Arc::new(HarnessService { host })).await {
                    // Not fatal: a shell whose harness socket died still has its companion, and
                    // taking the desktop down over it would be the worse outcome.
                    tracing::warn!(error = %e, "Harness socket stopped; only built-in minds remain");
                }
            });
        })
        .ok();
}

struct HarnessService {
    host: Host,
}

impl yantrik_ipc_transport::server::ServiceHandler for HarnessService {
    fn service_id(&self) -> &str {
        "harness"
    }

    fn handle(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, yantrik_ipc_contracts::email::ServiceError> {
        self.host.handle(method, &params).map_err(|message| {
            yantrik_ipc_contracts::email::ServiceError { code: -32000, message }
        })
    }
}
