//! `decide`: the decision model in use, asked by surfaces in other processes.
//!
//! The browser surface (apps/browser) holds the page and asks whether a press would buy, send or
//! delete something; an agent such as the Mind asks its own quick questions. The model is the
//! companion's, chosen in Settings. This is the door to it. It answers in the verdict's wire form
//! (`docs/decisions.md`): the same shape whichever model answered, abstentions included, so the
//! caller decides as it would without a model whenever one is off, down, switched off for that
//! use, or the desktop is incognito.
//!
//! Each `purpose` is a use from `JUDGE_USES`, with its own switch in Settings. Who may ask for
//! which is `yantrik_companion::decisions`'s rule: the person's surfaces for any use with a door;
//! an agent (a token, or the mind account) only for `agent`, and only of a model on this machine
//! or the home network, since a cloud model would be a way to carry out whatever the agent had
//! read that no taint rule sees.
//!
//! `safe`: it changes nothing. Off the UI thread, and bounded: a model takes up to seconds, and
//! no more than `MAX_AT_ONCE` are asked at a time.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use yantrik_app_runtime::control::{self, Action, App as ControlSurface, Param};
use yantrik_companion::config::JUDGE_USES;
use yantrik_companion::decisions::Caller;

use crate::bridge::CompanionHandle;

/// How long a caller is kept waiting for the model. The browser gives its own shorter budget
/// on top; past it, it decides with its word list alone.
const WAIT: Duration = Duration::from_secs(8);

/// Largest state accepted, in bytes of JSON: a control and its page context, not a document.
const MAX_STATE: usize = 16 * 1024;

/// Decisions in flight at once, from every caller together.
const MAX_AT_ONCE: usize = 4;
static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// One place in the `MAX_AT_ONCE`, given back when dropped.
struct Place;

impl Place {
    fn take() -> Option<Place> {
        IN_FLIGHT
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| (n < MAX_AT_ONCE).then_some(n + 1))
            .ok()
            .map(|_| Place)
    }
}

impl Drop for Place {
    fn drop(&mut self) {
        IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
    }
}

pub fn actions(surface: ControlSurface, companion: CompanionHandle) -> ControlSurface {
    let purposes: Vec<&'static str> = JUDGE_USES.iter().filter(|u| u.door).map(|u| u.id).collect();
    surface.action(
        Action::new(
            "decide",
            "Put typed questions (noul, choice, score) about a state to the decision model chosen \
             in Settings, for one of its uses, and answer with its verdict. An agent asks for \
             `agent`, and only a model on this machine or the home network answers it",
        )
        .risk("safe")
        .arg(Param::one_of("purpose", &purposes).describe("Which use of the decision model this is"))
        .arg(Param::object("state").describe("What is being judged, as a JSON object"))
        .arg(Param::object("questions").describe("Question id to {type, instructions, criteria?}, as docs/decisions.md")),
        move |args| {
            let caller = if control::agent_is_calling() { Caller::Agent } else { Caller::Person };
            let purpose = args["purpose"].as_str().unwrap_or_default().trim().to_string();
            let state = args["state"].clone();
            let questions = args["questions"].clone();
            if !state.is_object() {
                return Err("`state` is a JSON object".into());
            }
            if serde_json::to_string(&state).map(|s| s.len()).unwrap_or(usize::MAX) > MAX_STATE {
                return Err(format!("`state` is larger than {} KB; send what is judged and its context, not a document", MAX_STATE / 1024));
            }
            yantrik_ml::judge::questions_from_json(&questions)?;
            let place = Place::take()
                .ok_or_else(|| format!("{MAX_AT_ONCE} decisions are already being made; nothing was sent, ask again shortly"))?;
            let decisions = companion.decisions().clone();
            let work = move || {
                let (tx, rx) = crossbeam_channel::bounded(1);
                std::thread::spawn(move || {
                    let _place = place;
                    let _ = tx.send(decisions.ask_json(&state, &questions, &purpose, caller));
                });
                rx.recv_timeout(WAIT)
                    .unwrap_or_else(|_| Err(format!("the decision model did not answer within {} s", WAIT.as_secs())))
            };
            control::answer_later(work)
                .map(|()| serde_json::json!({ "answering": "off the UI thread" }))
                .or_else(|work| work())
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_more_than_max_at_once_are_in_flight() {
        let held: Vec<Place> = std::iter::from_fn(Place::take).take(MAX_AT_ONCE + 1).collect();
        assert_eq!(held.len(), MAX_AT_ONCE);
        assert!(Place::take().is_none());
        drop(held);
        assert!(Place::take().is_some(), "places come back when the decisions finish");
    }
}
