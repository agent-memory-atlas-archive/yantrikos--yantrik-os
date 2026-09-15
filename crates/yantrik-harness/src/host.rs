//! The OS side: who is attached, who is answering, and the turns in flight.
//!
//! Deliberately knows nothing about sockets. [`Host::handle`] takes a method name and a JSON
//! value and gives one back, so the shell can serve it on the bus it already has and every rule
//! in here can be tested without a compositor, a socket or a second process.
//!
//! # A harness exists because it is attached
//!
//! There is no registry file, no list of known harnesses, nothing to install. `mind` appears in
//! the picker when `mind` attaches and disappears when it stops polling. This is the whole
//! correction: the OS was configuring endpoints and models that the harnesses already manage
//! themselves, and now it holds the one thing it actually owns — which mind the person is
//! talking to.

use std::collections::HashMap;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::protocol::{self, Assignment, Attach};
use crate::{Answer, Capabilities, Chunk, Harness, Health, Turn};

/// One harness that has attached.
struct Attached {
    announced: Attach,
    session: String,
    last_seen: Instant,
    /// Turns handed out but not yet finished, and where their chunks go.
    in_flight: HashMap<u64, Sender<Chunk>>,
    /// Turns waiting to be collected by the next poll.
    queued: Vec<(Assignment, Sender<Chunk>)>,
}

impl Attached {
    fn present(&self) -> bool {
        self.last_seen.elapsed() < Duration::from_secs(protocol::PRESENCE_TIMEOUT_SECS)
    }
}

/// One row of the picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub detail: Option<String>,
    /// `true` for something compiled into the shell, `false` for an attached client.
    pub builtin: bool,
    pub active: bool,
    pub capabilities: Capabilities,
}

struct State {
    attached: HashMap<String, Attached>,
    active: String,
    next_turn: u64,
    next_session: u64,
}

/// Everything the OS knows about the minds available to it.
#[derive(Clone)]
pub struct Host {
    builtins: Arc<Vec<Arc<dyn Harness>>>,
    state: Arc<Mutex<State>>,
}

impl Host {
    /// `builtins` are compiled into the shell — the companion. They are always present and cannot
    /// be displaced by something attaching under the same id, so a misbehaving client cannot take
    /// over the conversation or leave the machine with no mind at all.
    pub fn new(builtins: Vec<Arc<dyn Harness>>) -> Host {
        let active = builtins.first().map(|h| h.id().to_string()).unwrap_or_default();
        Host {
            builtins: Arc::new(builtins),
            state: Arc::new(Mutex::new(State {
                attached: HashMap::new(),
                active,
                next_turn: 1,
                next_session: 1,
            })),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Forget harnesses that stopped polling. Called before anything that reads the list.
    fn reap(state: &mut State) {
        let gone: Vec<String> = state
            .attached
            .iter()
            .filter(|(_, a)| !a.present())
            .map(|(id, _)| id.clone())
            .collect();
        for id in gone {
            if let Some(lost) = state.attached.remove(&id) {
                // Anyone waiting on an answer from it is told, rather than left on a channel that
                // will never produce anything.
                for (_, tx) in lost.in_flight {
                    let _ = tx.send(Chunk::Failed(format!("{id} stopped responding")));
                }
                for (_, tx) in lost.queued {
                    let _ = tx.send(Chunk::Failed(format!("{id} left before it answered")));
                }
            }
        }
    }

    /// Everything selectable right now: built-ins, then whoever is attached.
    pub fn list(&self) -> Vec<Entry> {
        let mut state = self.lock();
        Self::reap(&mut state);
        let active = state.active.clone();

        let mut rows: Vec<Entry> = self
            .builtins
            .iter()
            .map(|h| Entry {
                id: h.id().to_string(),
                name: h.name().to_string(),
                detail: None,
                builtin: true,
                active: h.id() == active,
                capabilities: h.capabilities(),
            })
            .collect();

        let mut attached: Vec<&Attached> = state.attached.values().collect();
        attached.sort_by(|a, b| a.announced.id.cmp(&b.announced.id));
        for a in attached {
            rows.push(Entry {
                id: a.announced.id.clone(),
                name: a.announced.name.clone(),
                detail: a.announced.detail.clone(),
                builtin: false,
                active: a.announced.id == active,
                capabilities: Capabilities {
                    streaming: true,
                    tools: a.announced.tools,
                    memory: a.announced.memory,
                },
            });
        }
        rows
    }

    pub fn active_id(&self) -> String {
        self.lock().active.clone()
    }

    /// Choose which mind answers.
    pub fn set_active(&self, id: &str) -> Result<(), String> {
        let known: Vec<String> = self.list().into_iter().map(|e| e.id).collect();
        if !known.iter().any(|k| k == id) {
            return Err(format!(
                "no harness `{id}` is attached; this machine has: {}",
                known.join(", ")
            ));
        }
        self.lock().active = id.to_string();
        Ok(())
    }

    /// Put a turn to whichever harness is active.
    ///
    /// Returns immediately. A built-in answers on its own thread; an attached one is handed the
    /// turn by its next poll.
    pub fn send(&self, turn: Turn) -> Answer {
        let active = self.active_id();

        if let Some(builtin) = self.builtins.iter().find(|h| h.id() == active) {
            return builtin.send(turn);
        }

        let (tx, rx) = mpsc::channel();
        let mut state = self.lock();
        Self::reap(&mut state);
        let turn_id = state.next_turn;
        state.next_turn += 1;

        match state.attached.get_mut(&active) {
            Some(harness) => {
                harness.queued.push((
                    Assignment { turn_id, text: turn.text, context: turn.context },
                    tx,
                ));
            }
            None => {
                // Said rather than left silent: a question typed into a panel whose harness has
                // gone should come back with that, not with nothing.
                let _ = tx.send(Chunk::Failed(if active.is_empty() {
                    "no harness is attached to answer this".to_string()
                } else {
                    format!("`{active}` is no longer attached")
                }));
            }
        }
        rx
    }

    /// Health of one harness, for the picker.
    pub fn health(&self, id: &str) -> Health {
        if let Some(builtin) = self.builtins.iter().find(|h| h.id() == id) {
            return builtin.health();
        }
        let mut state = self.lock();
        Self::reap(&mut state);
        match state.attached.get(id) {
            Some(_) => Health::Ready,
            None => Health::Unreachable("not attached".into()),
        }
    }

    // ── The wire ────────────────────────────────────────────────────

    /// Answer one protocol call. The shell wires this to the `harness` socket.
    pub fn handle(&self, method: &str, params: &serde_json::Value) -> Result<serde_json::Value, String> {
        match method {
            protocol::ATTACH => self.attach(params),
            protocol::POLL => self.poll(params),
            protocol::CHUNK => self.chunk(params),
            protocol::COMPLETE => self.finish(params, None),
            protocol::FAIL => {
                let why = params["error"].as_str().unwrap_or("the harness reported a failure");
                self.finish(params, Some(why.to_string()))
            }
            protocol::DETACH => self.detach(params),
            other => Err(format!(
                "unknown method `{other}`; this service speaks: {}",
                protocol::METHODS.join(", ")
            )),
        }
    }

    fn attach(&self, params: &serde_json::Value) -> Result<serde_json::Value, String> {
        let announced: Attach = serde_json::from_value(params.clone())
            .map_err(|e| format!("`attach` needs at least an `id` and a `name`: {e}"))?;
        if announced.id.trim().is_empty() {
            return Err("`id` is what a person types to select this harness; it cannot be empty".into());
        }
        if announced.id.contains(char::is_whitespace) {
            return Err(format!(
                "`id` cannot contain spaces — it is what a person types to select this harness (`{}`)",
                announced.id
            ));
        }
        if self.builtins.iter().any(|h| h.id() == announced.id) {
            return Err(format!(
                "`{}` is a built-in harness; attach under a different id",
                announced.id
            ));
        }

        let mut state = self.lock();
        Self::reap(&mut state);
        let session = format!("s{}", state.next_session);
        state.next_session += 1;

        // Re-attaching under an existing id replaces it, which is what a harness that restarted
        // should get. Anything the old one owed is failed rather than abandoned.
        if let Some(previous) = state.attached.remove(&announced.id) {
            for (_, tx) in previous.in_flight {
                let _ = tx.send(Chunk::Failed(format!("{} restarted mid-answer", announced.id)));
            }
        }

        let id = announced.id.clone();
        state.attached.insert(
            id.clone(),
            Attached {
                announced,
                session: session.clone(),
                last_seen: Instant::now(),
                in_flight: HashMap::new(),
                queued: Vec::new(),
            },
        );

        // The first mind to attach on a machine with no built-in becomes the one answering,
        // rather than leaving a desktop that has a harness and is not using it.
        if state.active.is_empty() {
            state.active = id;
        }
        Ok(serde_json::json!({ "session": session }))
    }

    /// Find the harness holding this session, refreshing its presence.
    fn touch<'a>(state: &'a mut State, params: &serde_json::Value) -> Result<&'a mut Attached, String> {
        let session = params["session"].as_str().unwrap_or_default().to_string();
        if session.is_empty() {
            return Err("`session` is missing; call harness.attach first".into());
        }
        let id = state
            .attached
            .iter()
            .find(|(_, a)| a.session == session)
            .map(|(id, _)| id.clone())
            .ok_or_else(|| {
                "this session is not attached any more; call harness.attach again".to_string()
            })?;
        let harness = state.attached.get_mut(&id).expect("just found it");
        harness.last_seen = Instant::now();
        Ok(harness)
    }

    /// Hand over a turn if one is waiting.
    ///
    /// Does not block here: blocking is the caller's business, and holding the state lock for
    /// thirty seconds would stop every other harness and the whole UI. The shell's socket layer
    /// re-checks on a short interval up to the requested timeout.
    fn poll(&self, params: &serde_json::Value) -> Result<serde_json::Value, String> {
        let mut state = self.lock();
        Self::reap(&mut state);
        let harness = Self::touch(&mut state, params)?;
        match harness.queued.pop() {
            Some((assignment, tx)) => {
                harness.in_flight.insert(assignment.turn_id, tx);
                Ok(serde_json::to_value(assignment).unwrap_or_default())
            }
            // Nothing waiting is an ordinary answer, not an error: a harness polls far more often
            // than a person types.
            None => Ok(serde_json::json!({})),
        }
    }

    fn chunk(&self, params: &serde_json::Value) -> Result<serde_json::Value, String> {
        let turn_id = params["turn_id"].as_u64().ok_or("`turn_id` must be a number")?;
        let delta = params["delta"].as_str().unwrap_or_default().to_string();
        let mut state = self.lock();
        let harness = Self::touch(&mut state, params)?;
        let Some(tx) = harness.in_flight.get(&turn_id) else {
            return Err(format!("turn {turn_id} is not one this harness was given"));
        };
        if tx.send(Chunk::Text(delta)).is_err() {
            // The panel stopped listening — the person closed it or asked something else.
            harness.in_flight.remove(&turn_id);
            return Ok(serde_json::json!({ "dropped": true }));
        }
        Ok(serde_json::json!({}))
    }

    fn finish(&self, params: &serde_json::Value, failure: Option<String>) -> Result<serde_json::Value, String> {
        let turn_id = params["turn_id"].as_u64().ok_or("`turn_id` must be a number")?;
        let mut state = self.lock();
        let harness = Self::touch(&mut state, params)?;
        let Some(tx) = harness.in_flight.remove(&turn_id) else {
            return Err(format!("turn {turn_id} is not one this harness was given"));
        };
        if let Some(why) = failure {
            let _ = tx.send(Chunk::Failed(why));
        }
        // Dropping the sender closes the channel, which is how the reader learns the answer is
        // complete.
        drop(tx);
        Ok(serde_json::json!({}))
    }

    fn detach(&self, params: &serde_json::Value) -> Result<serde_json::Value, String> {
        let mut state = self.lock();
        let id = {
            let harness = Self::touch(&mut state, params)?;
            harness.announced.id.clone()
        };
        if let Some(gone) = state.attached.remove(&id) {
            for (_, tx) in gone.in_flight {
                let _ = tx.send(Chunk::Failed(format!("{id} detached mid-answer")));
            }
            for (_, tx) in gone.queued {
                let _ = tx.send(Chunk::Failed(format!("{id} detached before answering")));
            }
        }
        Ok(serde_json::json!({}))
    }
}

/// Wait for a turn, for a harness client that wants one call rather than a loop.
///
/// Lives here so the polling interval is defined once, by the side that knows what the timeout
/// means, rather than guessed at by every harness author.
pub fn poll_interval() -> Duration {
    Duration::from_millis(200)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    struct Builtin;

    impl Harness for Builtin {
        fn id(&self) -> &str {
            "companion"
        }
        fn name(&self) -> &str {
            "Companion"
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities { streaming: true, tools: true, memory: true }
        }
        fn health(&self) -> Health {
            Health::Ready
        }
        fn send(&self, _turn: Turn) -> Answer {
            let (tx, rx) = mpsc::channel();
            tx.send(Chunk::Text("from the companion".into())).ok();
            rx
        }
    }

    fn host() -> Host {
        Host::new(vec![Arc::new(Builtin)])
    }

    fn attach(host: &Host, id: &str) -> String {
        let reply = host
            .handle(protocol::ATTACH, &serde_json::json!({ "id": id, "name": id }))
            .unwrap();
        reply["session"].as_str().unwrap().to_string()
    }

    #[test]
    fn a_harness_exists_because_it_attached_not_because_it_was_configured() {
        let host = host();
        assert_eq!(host.list().len(), 1, "only the built-in to begin with");
        attach(&host, "mind");
        let ids: Vec<String> = host.list().into_iter().map(|e| e.id).collect();
        assert_eq!(ids, vec!["companion", "mind"]);
    }

    #[test]
    fn a_whole_turn_goes_out_and_comes_back_in_pieces() {
        let host = host();
        let session = attach(&host, "mind");
        host.set_active("mind").unwrap();

        let answer = host.send(Turn::new("what is open?"));

        // The harness collects it.
        let assignment = host
            .handle(protocol::POLL, &serde_json::json!({ "session": session }))
            .unwrap();
        let turn_id = assignment["turn_id"].as_u64().unwrap();
        assert_eq!(assignment["text"], "what is open?");

        for delta in ["Two ", "windows."] {
            host.handle(
                protocol::CHUNK,
                &serde_json::json!({ "session": session, "turn_id": turn_id, "delta": delta }),
            )
            .unwrap();
        }
        host.handle(
            protocol::COMPLETE,
            &serde_json::json!({ "session": session, "turn_id": turn_id }),
        )
        .unwrap();

        assert_eq!(crate::collect(answer).unwrap(), "Two windows.");
    }

    #[test]
    fn nothing_waiting_is_an_answer_not_an_error() {
        // A harness polls far more often than a person types.
        let host = host();
        let session = attach(&host, "mind");
        let reply = host.handle(protocol::POLL, &serde_json::json!({ "session": session })).unwrap();
        assert_eq!(reply, serde_json::json!({}));
    }

    #[test]
    fn a_failure_from_the_harness_reaches_the_person_who_asked() {
        let host = host();
        let session = attach(&host, "mind");
        host.set_active("mind").unwrap();
        let answer = host.send(Turn::new("hi"));
        let assignment =
            host.handle(protocol::POLL, &serde_json::json!({ "session": session })).unwrap();
        host.handle(
            protocol::FAIL,
            &serde_json::json!({
                "session": session,
                "turn_id": assignment["turn_id"],
                "error": "my model is not loaded",
            }),
        )
        .unwrap();
        assert_eq!(crate::collect(answer).unwrap_err(), "my model is not loaded");
    }

    #[test]
    fn asking_a_harness_that_left_says_so_rather_than_hanging() {
        // The worst failure this design could have: a question typed into a panel whose harness
        // has gone, answered by silence for ever.
        let host = host();
        let session = attach(&host, "mind");
        host.set_active("mind").unwrap();
        host.handle(protocol::DETACH, &serde_json::json!({ "session": session })).unwrap();

        let answer = host.send(Turn::new("anyone there?"));
        assert!(crate::collect(answer).unwrap_err().contains("no longer attached"));
    }

    #[test]
    fn a_harness_that_detaches_mid_answer_does_not_leave_the_asker_waiting() {
        let host = host();
        let session = attach(&host, "mind");
        host.set_active("mind").unwrap();
        let answer = host.send(Turn::new("hi"));
        let assignment =
            host.handle(protocol::POLL, &serde_json::json!({ "session": session })).unwrap();
        host.handle(
            protocol::CHUNK,
            &serde_json::json!({ "session": session, "turn_id": assignment["turn_id"], "delta": "I was" }),
        )
        .unwrap();
        host.handle(protocol::DETACH, &serde_json::json!({ "session": session })).unwrap();
        assert!(crate::collect(answer).unwrap_err().contains("detached mid-answer"));
    }

    #[test]
    fn a_restarted_harness_replaces_itself_and_owes_nothing() {
        let host = host();
        let first = attach(&host, "mind");
        host.set_active("mind").unwrap();
        let answer = host.send(Turn::new("hi"));
        host.handle(protocol::POLL, &serde_json::json!({ "session": first })).unwrap();

        // It crashes and comes back. The old session is dead and the old turn is not left hanging.
        let second = attach(&host, "mind");
        assert_ne!(first, second);
        assert!(crate::collect(answer).unwrap_err().contains("restarted"));
        assert_eq!(host.list().iter().filter(|e| e.id == "mind").count(), 1);
    }

    #[test]
    fn a_stale_session_is_told_to_attach_again() {
        let host = host();
        let err = host
            .handle(protocol::POLL, &serde_json::json!({ "session": "s404" }))
            .unwrap_err();
        assert!(err.contains("attach"), "{err}");
    }

    #[test]
    fn nothing_can_attach_over_a_builtin() {
        let host = host();
        let err = host
            .handle(protocol::ATTACH, &serde_json::json!({ "id": "companion", "name": "Impostor" }))
            .unwrap_err();
        assert!(err.contains("built-in"), "{err}");
        assert_eq!(host.list().len(), 1);
    }

    #[test]
    fn an_id_a_person_could_not_type_is_refused() {
        let host = host();
        assert!(host
            .handle(protocol::ATTACH, &serde_json::json!({ "id": "my mind", "name": "X" }))
            .unwrap_err()
            .contains("spaces"));
        assert!(host
            .handle(protocol::ATTACH, &serde_json::json!({ "id": "", "name": "X" }))
            .unwrap_err()
            .contains("cannot be empty"));
    }

    #[test]
    fn selecting_something_not_attached_names_what_is() {
        let host = host();
        attach(&host, "mind");
        let err = host.set_active("hermes").unwrap_err();
        assert!(err.contains("companion"), "{err}");
        assert!(err.contains("mind"), "{err}");
    }

    #[test]
    fn a_builtin_still_answers_the_way_it_always_did() {
        let host = host();
        assert_eq!(host.active_id(), "companion");
        assert_eq!(crate::collect(host.send(Turn::new("hi"))).unwrap(), "from the companion");
    }

    #[test]
    fn a_chunk_for_a_turn_the_harness_was_never_given_is_refused() {
        let host = host();
        let session = attach(&host, "mind");
        let err = host
            .handle(
                protocol::CHUNK,
                &serde_json::json!({ "session": session, "turn_id": 999, "delta": "x" }),
            )
            .unwrap_err();
        assert!(err.contains("not one this harness was given"), "{err}");
    }

    #[test]
    fn an_unknown_method_lists_the_real_ones() {
        let host = host();
        let err = host.handle("harness.think", &serde_json::json!({})).unwrap_err();
        for method in protocol::METHODS {
            assert!(err.contains(method), "{err}");
        }
    }

    #[test]
    fn capabilities_are_what_the_harness_claimed_for_itself() {
        let host = host();
        host.handle(
            protocol::ATTACH,
            &serde_json::json!({ "id": "mind", "name": "Mind", "tools": true, "detail": "qwen2.5" }),
        )
        .unwrap();
        let row = host.list().into_iter().find(|e| e.id == "mind").unwrap();
        assert!(row.capabilities.tools);
        assert!(!row.capabilities.memory);
        assert_eq!(row.detail.as_deref(), Some("qwen2.5"));
        assert!(!row.builtin);
    }
}
