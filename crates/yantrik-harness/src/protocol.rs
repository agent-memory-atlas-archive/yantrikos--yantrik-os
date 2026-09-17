//! The wire between a harness and this OS.
//!
//! Six methods, spoken by the harness to the `harness` socket. That is the whole interface, and
//! its smallness is the point: a harness already knows how to be itself — `yantrik-mind` has its
//! own models and config, hermes-agent has its own — and none of that is the OS's business. The
//! OS offers somewhere to attach and a way to be handed turns.
//!
//! # Why the harness dials in, and polls
//!
//! The socket bus here is request/response: a server answers calls, it does not push. Rather than
//! bend that, the harness asks for work — [`POLL`] answers with a turn if one is waiting and with
//! `{}` if none is, and the harness asks again. Three things fall out of it, all of them wanted:
//!
//! - **Anything with a JSON-RPC client can be a harness.** No callback URL to configure, no port
//!   to open, no inbound reachability. A harness behind NAT or in a container works the same as
//!   one on this machine.
//! - **The OS never has to know where a harness lives.** It has no endpoint, no key, no model
//!   name — the harness brought its own.
//! - **Attachment is liveness.** A harness exists because it is polling. Nothing has to be
//!   deregistered when one dies, and nothing can be listed that is not actually there.
//!
//! # A whole harness
//!
//! ```text
//! attach  {id, name, capabilities}      → {session}
//! loop:
//!   poll  {session}                     → {turn_id, text, context} | {}
//!   …if {}: wait POLL_INTERVAL_MS and poll again
//!   chunk {session, turn_id, delta}     → {}          … as many as you like
//!   complete {session, turn_id}         → {}
//! ```
//!
//! Driving the desktop is deliberately NOT here. An attached harness reads and steers the OS
//! through the control surface every app already publishes — `app.describe` / `app.act`, or `yos`
//! — which exists, is permission-graded, and works the same for a harness as for anything else.
//! Putting a second way to do it in this protocol would be a second thing to keep correct.

use serde::{Deserialize, Serialize};

/// Announce yourself. Answers with a session id used by every later call.
pub const ATTACH: &str = "harness.attach";
/// Ask for a turn. Answers immediately: the turn if one is waiting, `{}` if none is.
pub const POLL: &str = "harness.poll";
/// Part of an answer, as soon as it exists.
pub const CHUNK: &str = "harness.chunk";
/// This turn is finished.
pub const COMPLETE: &str = "harness.complete";
/// This turn failed, and why.
pub const FAIL: &str = "harness.fail";
/// Leave cleanly. Not required — dropping off is also how you leave.
pub const DETACH: &str = "harness.detach";

/// Every method this service answers, for the error when something else is called.
pub const METHODS: &[&str] = &[ATTACH, POLL, CHUNK, COMPLETE, FAIL, DETACH];

/// How long a harness may go without polling before it is considered gone.
///
/// Generous, because a harness is usually blocked in its own long poll and a slow one must not be
/// evicted mid-answer. It only has to be shorter than a person's patience with a picker listing
/// something that is no longer there.
pub const PRESENCE_TIMEOUT_SECS: u64 = 90;

/// How long to wait after an empty poll before asking again.
///
/// This used to be `MAX_POLL_MS = 30_000`, documented as "the longest a poll is held open" — and
/// nothing held a poll open for any length of time. `poll` takes the lock, pops the queue and
/// returns `{}` if it is empty, which is a deliberate and good design (it never occupies a
/// connection, so a harness cannot wedge the bus by existing). But the constant and the module
/// doc both described a long poll that was never written, and the only complete harness in the
/// tree quietly slept 300ms in a loop to work around the contract it had been handed.
///
/// A published protocol that describes something other than what the server does is the single
/// most expensive kind of wrong here: a harness is written by someone who has this file and no
/// access to the host, and every one of them would have written `timeout_ms: 30000`, got an empty
/// answer in a millisecond, and spun a core.
///
/// So: this is the client's wait, it is named for what it is, and 200ms is what the host's own
/// `poll_interval()` has always returned. Nobody notices 200ms in front of a model call.
pub const POLL_INTERVAL_MS: u64 = 200;

/// What a harness says about itself when it attaches.
///
/// It says this; nothing else does. There is no config file describing a harness, because the
/// harness is the authority on what it is and on this OS a thing that is not attached does not
/// exist.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Attach {
    /// Stable, and what a person types to select it: `mind`, `hermes`, `openclaw`.
    pub id: String,
    /// What the picker shows.
    pub name: String,
    /// Optional, for the panel: model, version, where it is running.
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub tools: bool,
    #[serde(default)]
    pub memory: bool,
}

/// One turn handed to a harness.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Assignment {
    pub turn_id: u64,
    pub text: String,
    #[serde(default)]
    pub context: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_harness_announces_itself_with_almost_nothing() {
        // The shortest thing that can attach. Everything optional is genuinely optional, because
        // requiring a field is requiring every harness author to have an opinion about it.
        let attach: Attach = serde_json::from_str(r#"{"id":"mind","name":"Yantrik Mind"}"#).unwrap();
        assert_eq!(attach.id, "mind");
        assert!(!attach.tools);
        assert_eq!(attach.detail, None);
    }

    #[test]
    fn there_is_nowhere_to_put_an_endpoint_or_a_key() {
        // The whole correction this protocol exists to encode: a harness manages its own models,
        // endpoints and credentials. If this struct ever grows a field for one, the OS has gone
        // back to configuring things it does not own.
        let json = serde_json::to_value(Attach {
            id: "mind".into(),
            name: "Mind".into(),
            detail: Some("qwen2.5 on node1".into()),
            tools: true,
            memory: true,
        })
        .unwrap();
        let keys: Vec<&str> = json.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        for forbidden in ["endpoint", "model", "api_key", "api_key_env", "command", "url"] {
            assert!(!keys.contains(&forbidden), "`{forbidden}` has no business in this protocol");
        }
    }

    #[test]
    fn an_assignment_carries_the_turn_and_nothing_about_who_answers_it() {
        let a: Assignment =
            serde_json::from_str(r#"{"turn_id":7,"text":"what is open?"}"#).unwrap();
        assert_eq!(a.turn_id, 7);
        assert_eq!(a.context, None);
    }
}
