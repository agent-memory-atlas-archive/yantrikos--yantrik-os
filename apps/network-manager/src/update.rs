//! The only function that changes the model: every button, intent, timer and job result lands
//! here as a `Command`, and asks for its effects.
//!
//! Each change to the machine is one command, raised by the button on screen and by the intent
//! of the same name, so a person and a mind cannot be told different things about it. Its job
//! asks the service, then re-reads the machine, and the command that carries both back puts the
//! reading on screen first and the change's own verdict over it: the verdict is the more specific
//! of the two. The service verifies each change against a re-read before answering.

use std::sync::atomic::Ordering;

use yantrik_app::effects::Effects;
use yantrik_app::jobs::{Concurrency, JobKey, Scope};
use yantrik_app::mind::AskError;
use yantrik_app::rail::{self, RailCommand};
use yantrik_app::Refused;
use yantrik_ipc_contracts::network::{method, WifiConnectParams, WifiForgetParams, WifiRadioParams};
use yantrik_ipc_transport::privacy;

use crate::backend::{self, call, to_params, Joiner, NetworkApi};
use crate::connect;
use crate::model::*;
use crate::opening;
use crate::reading::read;
use crate::NetworkManager;

type Fx = Effects<NetworkManager>;

/// What a redraw-nothing command names, so a tick that found a read in flight, or a reading that
/// lost to a newer one, does not redraw the window.
const NOTHING: &str = "nothing";

/// The rail suggestion and the header button ask the same question.
pub const EXPLAIN: &str = "explain";

pub fn update(m: &mut Model, command: Command, fx: &mut Fx) -> Result<(), Refused> {
    match command {
        Command::Tick => {
            if m.refreshing == 0 {
                start_read(m, fx);
            }
            fx.changed(NOTHING);
        }
        Command::Refresh => {
            start_read(m, fx);
            fx.changed(NOTHING);
        }
        Command::Read(taken) => {
            m.refreshing = m.refreshing.saturating_sub(1);
            if !apply_reading(m, *taken, fx) {
                fx.changed(NOTHING);
            }
        }
        Command::ReadFailed => {
            // The next tick reads again; what is on screen stays the last reading taken.
            m.refreshing = m.refreshing.saturating_sub(1);
            fx.changed(NOTHING);
        }
        Command::SetRadio(enabled) => {
            start_change(m, fx, Change::Radio, move |api| {
                Outcome::Radio(call(api, method::WIFI_RADIO, to_params(WifiRadioParams { enabled })))
            });
            fx.changed(NOTHING);
        }
        Command::Disconnect => {
            start_change(m, fx, Change::Disconnect, |api| {
                Outcome::Disconnect(call(api, method::WIFI_DISCONNECT, serde_json::json!({})))
            });
            fx.changed(NOTHING);
        }
        Command::Forget(ssid) => {
            // The service refuses the one that is currently in use.
            start_change(m, fx, Change::Forget, move |api| {
                Outcome::Forget(call(api, method::WIFI_FORGET, to_params(WifiForgetParams { ssid })))
            });
            fx.changed(NOTHING);
        }
        Command::Scan => start_scan(m, fx),
        Command::ScanAsked => {
            // Refused before anything is started, naming the reason. A deferred action that
            // accepts and then fails silently is worse than one that says no.
            if let Some((kind, reason)) = no_radio(m) {
                refuse_on_screen(m, fx, kind, reason);
                return Ok(());
            }
            start_scan(m, fx);
            fx.settle(Ok(()));
        }
        Command::Connect(request) => start_connect(m, fx, request),
        Command::Join(ssid) => {
            if let Some((kind, reason)) = no_radio(m) {
                refuse_on_screen(m, fx, kind, reason);
                return Ok(());
            }
            match connect::plan(&ssid, &m.reading.known) {
                connect::Plan::Join => {
                    // Saved on this machine: NetworkManager holds the credential, so no secret
                    // is needed and none is asked for.
                    m.answers.connect = connect::joining_answer(&ssid);
                    start_connect(m, fx, connect::join_request(&ssid, ""));
                }
                connect::Plan::AskPerson => {
                    // Nothing is saved here and a password is not an argument this intent can
                    // carry (#178). The window's own prompt goes up, the person types into it,
                    // and what they type reaches the service through the same `Connect` their
                    // click would. A prompt nobody can see is a connect that never happens, so
                    // the window comes out of minimized.
                    m.answers.connect = connect::waiting_answer(&ssid);
                    m.prompt_ssid = ssid;
                    m.prompt_status.clear();
                    fx.changed(PART_PROMPT_STATUS);
                    fx.changed(PART_PROMPT_CLEAR);
                    fx.changed(PART_PROMPT_OPEN);
                }
            }
            fx.settle(Ok(()));
        }
        Command::Changed(outcome, taken) => changed(m, *outcome, *taken, fx),
        Command::ChangeFailed(change) => {
            let sentence = match change {
                Change::Radio => "The radio change did not finish; try again.",
                Change::Disconnect => "The disconnect did not finish; try again.",
                Change::Forget => "Forgetting the network did not finish; try again.",
                Change::Scan => "The scan did not finish; try again.",
                Change::Connect => "The connect did not finish; try again.",
            };
            match change {
                Change::Scan => {
                    m.scanning = false;
                    fx.changed(PART_SCAN);
                }
                Change::Connect => {
                    m.prompt_status = sentence.into();
                    fx.changed(PART_PROMPT_STATUS);
                    fx.changed(PART_PROMPT_CLEAR);
                }
                _ => {}
            }
            m.notice = sentence.into();
            fx.changed(PART_NOTICE);
        }
        Command::OpenSection(id) => {
            // A rail row goes to the section it is about: the smallest real thing it can do, and
            // what a person pressing "Firewall state unknown" is asking for.
            let section = match id.as_str() {
                "wifi" => 0,
                "link" if m.reading.status.is_wifi() => 0,
                "link" => 1,
                "firewall" => 2,
                _ => {
                    fx.changed(NOTHING);
                    return Ok(());
                }
            };
            m.section = section;
            fx.changed(PART_SECTION);
        }
        Command::Explain => {
            // The header's button and the rail's suggestion are one question. While the rail can
            // ask, it asks (its card shows the answer, the panel here too); when it cannot, the
            // question still goes to the companion, and the panel says what came back.
            if m.rail.reach().hint().is_none() && !m.rail.is_applying() {
                rail::update::<NetworkManager>(m, RailCommand::Activate(EXPLAIN.into()), fx)?;
            } else {
                let prompt = crate::rail::network_question(m);
                let mind = m.mind.clone();
                fx.spawn_keyed_or(
                    JobKey {
                        feature: "ai",
                        scope: Scope::App,
                    },
                    Concurrency::ReplacePrevious,
                    Command::Explained(Err(AskError::Failed("the question did not finish".into()))),
                    move |_| {
                        // Private mode keeps this desktop from every agent, as the rail's own ask
                        // does.
                        let answer = if privacy::is_private() {
                            Err(AskError::Failed(privacy::REFUSAL.into()))
                        } else {
                            mind.ask(&prompt)
                        };
                        Command::Explained(answer)
                    },
                );
            }
            m.ai.working = true;
            fx.changed(PART_AI);
        }
        Command::Explained(answer) => {
            m.ai.working = false;
            m.ai.response = spoken(answer);
            fx.changed(PART_AI);
        }
        Command::DismissAi => {
            m.ai.response.clear();
            fx.changed(PART_AI);
        }
        Command::Rail(command) => rail_command(m, command, fx)?,
    }
    Ok(())
}

/// The rail's commands, with the panel kept beside the card for the one question both ask.
fn rail_command(m: &mut Model, command: RailCommand, fx: &mut Fx) -> Result<(), Refused> {
    match command {
        RailCommand::Activate(id) if id == EXPLAIN => {
            rail::update::<NetworkManager>(m, RailCommand::Activate(id), fx)?;
            m.ai.working = true;
            fx.changed(PART_AI);
        }
        RailCommand::Answered { suggestion, answer } => {
            if suggestion == EXPLAIN && m.rail.working() == Some(EXPLAIN) {
                m.ai.working = false;
                m.ai.response = spoken(answer.clone());
                fx.changed(PART_AI);
            }
            rail::update::<NetworkManager>(m, RailCommand::Answered { suggestion, answer }, fx)?;
        }
        other => rail::update::<NetworkManager>(m, other, fx)?,
    }
    Ok(())
}

/// The companion's answer for the panel: its words, or why there are none.
fn spoken(answer: Result<String, AskError>) -> String {
    match answer {
        Ok(text) => text,
        Err(e) => {
            tracing::warn!(error = %e, "companion call failed");
            e.to_string()
        }
    }
}

/// A refresh, numbered when it starts reading so that the newest reading taken is the one shown.
fn start_read(m: &mut Model, fx: &mut Fx) {
    m.refreshing += 1;
    let api = m.net.clone();
    let sequence = m.sequence.clone();
    let route = !m.opened;
    fx.spawn_or(Command::ReadFailed, move |_| {
        Command::Read(Box::new(take(api.as_ref(), &sequence, route)))
    });
}

fn take(api: &dyn NetworkApi, sequence: &std::sync::atomic::AtomicU64, route: bool) -> Taken {
    let seq = sequence.fetch_add(1, Ordering::SeqCst) + 1;
    Taken {
        seq,
        reading: read(api, route),
    }
}

/// One change to the machine, off the UI thread: the call, then the reading after it.
fn start_change(
    m: &mut Model,
    fx: &mut Fx,
    change: Change,
    work: impl FnOnce(&dyn NetworkApi) -> Outcome + Send + 'static,
) {
    let api = m.net.clone();
    let sequence = m.sequence.clone();
    let route = !m.opened;
    fx.spawn_or(Command::ChangeFailed(change), move |_| {
        let outcome = work(api.as_ref());
        let taken = take(api.as_ref(), &sequence, route);
        Command::Changed(Box::new(outcome), Box::new(taken))
    });
}

/// Ask the adapter to sweep the band, then read the list back.
fn start_scan(m: &mut Model, fx: &mut Fx) {
    m.scanning = true;
    fx.changed(PART_SCAN);
    start_change(m, fx, Change::Scan, |api| Outcome::Scan(backend::scan_now(api)));
}

/// Join a network. The request is moved into the worker and its password is never copied
/// anywhere else — not into the notice, not into the prompt's status, not into a log line —
/// and the prompt's box is cleared when the attempt lands, so it does not sit in the window
/// either.
fn start_connect(m: &mut Model, fx: &mut Fx, request: WifiConnectParams) {
    m.prompt_status = format!("Connecting to {}\u{2026}", request.ssid);
    fx.changed(PART_PROMPT_STATUS);
    let named = request.ssid.clone();
    start_change(m, fx, Change::Connect, move |api| {
        // The same `submit` the tests drive with a recorder: one path from a typed password to
        // the backend, whether a person typed it or the intent raised the prompt for one.
        let result = connect::submit(&Joiner(api), &request.ssid, request.password.as_deref().unwrap_or(""));
        Outcome::Connect { ssid: named, result }
    });
}

/// A change landed: the reading after it first, then its own verdict over the notice, then its
/// answer to the intent that asked (a button's change has none to give).
fn changed(m: &mut Model, outcome: Outcome, taken: Taken, fx: &mut Fx) {
    let verdict: Result<(), String> = match outcome {
        Outcome::Radio(result) => {
            let verdict = result.as_ref().map(|_| ()).map_err(Clone::clone);
            m.answers.radio = result.ok();
            verdict
        }
        Outcome::Disconnect(result) => {
            let verdict = result.as_ref().map(|_| ()).map_err(Clone::clone);
            m.answers.disconnect = result.ok();
            verdict
        }
        Outcome::Forget(result) => {
            let verdict = result.as_ref().map(|_| ()).map_err(Clone::clone);
            m.answers.forget = result.ok();
            verdict
        }
        Outcome::Scan(result) => {
            m.scanning = false;
            // On a failure the list is cleared rather than left standing: a stale list
            // presented as the result of a scan that failed is a small version of the same lie.
            m.in_range = match &result {
                Ok(found) => found.iter().map(InRange::from_scan).collect(),
                Err(_) => Vec::new(),
            };
            fx.changed(PART_SCAN);
            result.map(|_| ())
        }
        Outcome::Connect { ssid, result } => {
            match &result {
                Ok(_) => {
                    m.prompt_status = format!("Connected to {ssid}");
                    fx.changed(PART_PROMPT_CLOSE);
                }
                // The prompt keeps the network's name with the reason above it; the box is
                // cleared: a wrong secret left in a text field gets tried again by accident.
                Err(reason) => m.prompt_status = reason.clone(),
            }
            fx.changed(PART_PROMPT_STATUS);
            fx.changed(PART_PROMPT_CLEAR);
            result.map(|_| ())
        }
    };
    apply_reading(m, taken, fx);
    m.notice = match &verdict {
        Ok(()) => String::new(),
        Err(reason) => reason.clone(),
    };
    fx.changed(PART_NOTICE);
    fx.settle(verdict.map_err(|reason| Refused::new("not_done", reason)));
}

/// Puts a reading on screen unless a newer one is there already. Says whether it did.
fn apply_reading(m: &mut Model, taken: Taken, fx: &mut Fx) -> bool {
    if taken.seq <= m.shown {
        tracing::debug!(seq = taken.seq, shown = m.shown, "an older reading, dropped");
        return false;
    }
    m.shown = taken.seq;
    m.reading = taken.reading;
    m.read_once = true;
    m.notice = m.reading.notice();
    // Once, from the first reading: afterwards the section is the person's to choose. The
    // section for the interface that carries traffic, the one the default route leaves by.
    if !m.opened {
        if let Some(table) = m.reading.route_table.take() {
            let ethernet: Vec<String> = m.reading.ethernet.iter().map(|e| e.name.clone()).collect();
            m.section = opening::opening_tab(
                opening::default_route_interface(&table).as_deref(),
                &ethernet,
                m.reading.wifi.adapter_present,
            );
            m.opened = true;
            fx.changed(PART_SECTION);
        }
    }
    fx.changed(PART_READING);
    fx.changed(PART_NOTICE);
    true
}

/// Why a Wi-Fi intent cannot start here, if it cannot: no adapter (in the reading's own words),
/// or nothing read yet to say whether there is one.
fn no_radio(m: &Model) -> Option<(&'static str, String)> {
    if !m.read_once {
        return Some(("not_read", "the network has not been read yet; try again in a moment".into()));
    }
    (!m.reading.wifi.adapter_present).then(|| ("no_adapter", m.reading.no_adapter()))
}

/// A refusal said on screen as well as returned to the intent that asked.
fn refuse_on_screen(m: &mut Model, fx: &mut Fx, kind: &'static str, reason: String) {
    m.notice = reason.clone();
    fx.changed(PART_NOTICE);
    fx.settle(Err(Refused::new(kind, reason)));
}
