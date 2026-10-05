//! The `redact` event: erasing the shell's copies of a conversation at the person's request.
//!
//! A mind that asked "Keep or erase?" and was answered *Erase* erases its own memory, then sends
//! `{"kind": "redact", "request_id", "needles": [{"sha256", "len"}]}` on the run that asked. The
//! host takes it here rather than as one of the turn's events: it may arrive after the turn
//! closed, and it is never handed to a reader, logged or kept — not even as digests.
//!
//! The run store decides whether it may happen at all (`run_store::erase` has the rule: a question
//! this run asked, answered with the offered `Erase`, from the harness and session that hold the
//! run, while the run is in flight or within five minutes of its end, once per question) and
//! erases its own copy. Only then is the shell's [`Redactor`] asked to erase the agent's pane
//! transcript. The reply says how many places, and where: `{"redacted": n, "where":
//! ["transcript", "runs"]}`, or `{"refused": why}` with nothing changed.

use std::sync::Arc;

use super::{refused, Host};
use crate::event::{AgentId, Event};
use crate::protocol;
use crate::redact::{self, Needle};
use crate::run_store::{now_ms, Erasure};

/// What the shell is asked to erase from its own copy of one agent's conversation.
#[derive(Debug, Clone)]
pub struct ShellErasure<'a> {
    /// The Keep/Erase question, which the person answered *Erase*: what the record is keyed by.
    pub request_id: &'a str,
    pub needles: &'a [Needle],
    /// How many places the run store already erased, so the shell's record can say the whole.
    pub places_in_runs: usize,
}

/// What the shell erased from its copy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ShellErased {
    /// Places whose words were replaced with the marker.
    pub places: usize,
    /// Records that keep their words (a tool call, an approval) whose display now shows the
    /// marker instead.
    pub masked: usize,
}

/// The shell's half of an erasure: given the agent and what to erase, erase it from the agent's
/// session (in memory and on disk) and say how much. Called without the host's lock held.
pub type Redactor = Arc<dyn Fn(&AgentId, &ShellErasure<'_>) -> Result<ShellErased, String> + Send + Sync>;

impl Host {
    /// The same host, asking `redactor` to erase the shell's own copy of a conversation when a
    /// `redact` is accepted. Without one, only the run store is erased, and the reply says so.
    pub fn with_redactor(
        mut self,
        redactor: impl Fn(&AgentId, &ShellErasure<'_>) -> Result<ShellErased, String> + Send + Sync + 'static,
    ) -> Host {
        self.redactor = Some(Arc::new(redactor));
        self
    }

    /// One `redact` from `harness`'s session `session`, sent on run `run_id`. A refusal is an
    /// answer, never an error, and changes nothing.
    pub(super) fn redact(&self, run_id: u64, harness: &str, session: &str, raw: &serde_json::Value) -> serde_json::Value {
        let outcome = self.try_redact(run_id, harness, session, raw);
        let mut state = self.lock();
        match outcome {
            Ok(reply) => {
                state.events.redacted += 1;
                reply
            }
            Err(why) => {
                state.events.redact_refused += 1;
                // The reason only: never the needles, which are digests of what the person wants gone.
                tracing::info!(harness, run = run_id, why = %why, "redact refused; nothing changed");
                refused(why)
            }
        }
    }

    fn try_redact(&self, run_id: u64, harness: &str, session: &str, raw: &serde_json::Value) -> Result<serde_json::Value, String> {
        let size = serde_json::to_string(raw).map(|s| s.len()).unwrap_or(usize::MAX);
        if size > protocol::MAX_EVENT_BYTES {
            return Err(format!("this event is {size} bytes and one event may be at most {}", protocol::MAX_EVENT_BYTES));
        }
        let store = self.runs.as_ref().ok_or("this desktop keeps no runs, so it cannot tell what the person answered")?;
        // The parse error is not repeated: it could quote a digest back.
        let Ok(Event::Redact { request_id, needles }) = serde_json::from_value::<Event>(raw.clone()) else {
            return Err("a malformed `redact`: it needs `request_id` and `needles: [{sha256, len}]`".to_string());
        };
        if request_id.trim().is_empty() {
            return Err("a `redact` needs the `request_id` of the question the person answered".to_string());
        }
        redact::validate(&needles)?;

        let erasure = Erasure { run_id, request_id: &request_id, harness, owner: session, needles: &needles };
        let erased = store.redact(&erasure, now_ms()).map_err(|r| r.to_string())?;
        if !erased.checkpointed {
            tracing::error!(run = run_id, "erased, but the run store's write-ahead log could not be emptied yet");
        }
        let agent = AgentId::new(&erased.harness, &erased.conversation);
        let mut places = erased.places;
        let mut masked = 0;
        let mut places_in: Vec<&str> = Vec::new();
        let mut shell_failed = None;
        match &self.redactor {
            Some(redactor) => {
                let asked = ShellErasure { request_id: &request_id, needles: &needles, places_in_runs: erased.places };
                match redactor(&agent, &asked) {
                    Ok(done) => {
                        places += done.places;
                        masked = done.masked;
                        places_in.push("transcript");
                    }
                    Err(why) => {
                        tracing::error!(agent = %agent, run = run_id, why = %why, "the run store was erased and the transcript was not");
                        shell_failed = Some(why);
                    }
                }
            }
            None => shell_failed = Some("this desktop keeps no transcript to erase".to_string()),
        }
        places_in.push("runs");
        if let Err(e) = store.set_redaction_places(run_id, &request_id, places as u64) {
            tracing::error!(run = run_id, error = %e, "the erasure's count was not recorded");
        }
        tracing::info!(agent = %agent, run = run_id, places, masked, "erased at the person's request");
        let mut reply = serde_json::json!({ "redacted": places, "where": places_in });
        if masked > 0 {
            reply["masked"] = serde_json::json!(masked);
        }
        if let Some(why) = shell_failed {
            reply["transcript"] = serde_json::json!(why);
        }
        Ok(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_store::{RunState, RunStore};
    use crate::{Chunk, Turn};
    use serde_json::json;
    use std::sync::Mutex;

    /// What the shell's redactor was asked: the agent, the request, how many needles.
    type Asked = Arc<Mutex<Vec<(AgentId, String, usize)>>>;

    fn host_with_shell() -> (Host, Arc<RunStore>, Asked) {
        let store = Arc::new(RunStore::in_memory().unwrap());
        let asked: Asked = Arc::default();
        let log = asked.clone();
        let host = Host::new(vec![]).with_runs(store.clone()).with_redactor(move |agent, e| {
            log.lock().unwrap().push((agent.clone(), e.request_id.to_string(), e.needles.len()));
            Ok(ShellErased { places: 2, masked: 1 })
        });
        (host, store, asked)
    }

    fn call(host: &Host, method: &str, params: serde_json::Value) -> serde_json::Value {
        host.handle(method, &params).unwrap()
    }

    /// A pi agent whose run said "Priya" across two chunks, asked Keep/Erase as `forget`, and was
    /// answered `answer`: (session, agent, run, the reader's answer).
    fn answered(host: &Host, answer: &str) -> (String, AgentId, u64, crate::Answer) {
        let session = call(host, protocol::ATTACH, json!({ "id": "pi", "name": "pi", "conversations": true }))["session"]
            .as_str()
            .unwrap()
            .to_string();
        let agent = host.start_agent("pi").unwrap();
        let reader = host.send_to(&agent, Turn::new("forget my sister's name")).unwrap();
        let run = call(host, protocol::POLL, json!({ "session": session }))["turn_id"].as_u64().unwrap();
        for delta in ["Your sister is Pri", "ya."] {
            call(host, protocol::CHUNK, json!({ "session": session, "turn_id": run, "delta": delta }));
        }
        let ask = json!({ "kind": "request", "request_id": "forget", "prompt": "Forget your sister's name?", "options": ["Keep", "Erase"] });
        assert_eq!(call(host, protocol::EVENT, json!({ "session": session, "turn_id": run, "event": ask })), json!({}));
        host.answer(run, "forget", &json!(answer)).unwrap();
        (session, agent, run, reader)
    }

    fn redact(host: &Host, session: &str, run: u64, request_id: &str) -> serde_json::Value {
        let event = json!({ "kind": "redact", "request_id": request_id, "needles": [Needle::of("Priya")] });
        call(host, protocol::EVENT, json!({ "session": session, "turn_id": run, "event": event }))
    }

    fn reply_text(store: &RunStore, run: u64) -> String {
        store
            .events(run, 0, crate::run_store::PAGE_MAX)
            .unwrap()
            .events
            .into_iter()
            .filter(|e| e.kind == "text")
            .map(|e| e.payload["delta"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn after_an_erase_answer_the_run_and_the_transcript_are_erased_and_the_reply_says_where() {
        let (host, store, asked) = host_with_shell();
        let (session, agent, run, reader) = answered(&host, "Erase");
        let reply = redact(&host, &session, run, "forget");
        assert_eq!(reply, json!({ "redacted": 3, "where": ["transcript", "runs"], "masked": 1 }));
        assert_eq!(reply_text(&store, run), format!("Your sister is {}.", redact::MARKER));
        assert_eq!(*asked.lock().unwrap(), vec![(agent, "forget".to_string(), 1)]);
        assert_eq!(store.redactions("pi", agent_conversation(&store, run).as_str()).unwrap()[0].places, 3);
        // Never handed to the reader as an event, and never written to the run's log.
        assert!(!reader.try_iter().any(|c| matches!(c, Chunk::Event(Event::Redact { .. }))));
        assert!(store.events(run, 0, 500).unwrap().events.iter().all(|e| !e.payload.to_string().contains("sha256")));
        assert_eq!(host.event_counts().redacted, 1);
    }

    fn agent_conversation(store: &RunStore, run: u64) -> String {
        store.run(run).unwrap().unwrap().conversation
    }

    #[test]
    fn a_keep_an_unknown_question_and_a_second_use_are_refused_and_change_nothing() {
        let (host, store, asked) = host_with_shell();
        let (session, _, run, _reader) = answered(&host, "Keep");
        assert!(redact(&host, &session, run, "forget")["refused"].as_str().unwrap().contains("Erase"));
        assert!(redact(&host, &session, run, "never-asked")["refused"].as_str().unwrap().contains("never asked"));
        assert_eq!(reply_text(&store, run), "Your sister is Priya.");
        assert!(asked.lock().unwrap().is_empty(), "the shell is not asked unless the rule held");

        let (host, _, asked) = host_with_shell();
        let (session, _, run, _reader) = answered(&host, "Erase");
        assert!(redact(&host, &session, run, "forget").get("redacted").is_some());
        assert!(redact(&host, &session, run, "forget")["refused"].as_str().unwrap().contains("one redaction per question"));
        assert_eq!(asked.lock().unwrap().len(), 1);
        assert_eq!((host.event_counts().redacted, host.event_counts().redact_refused), (1, 1));
    }

    #[test]
    fn another_harness_cannot_erase_a_run_it_does_not_hold() {
        let (host, store, asked) = host_with_shell();
        let (_, _, run, _reader) = answered(&host, "Erase");
        let other = call(&host, protocol::ATTACH, json!({ "id": "hermes", "name": "hermes" }))["session"].as_str().unwrap().to_string();
        assert!(redact(&host, &other, run, "forget")["refused"].as_str().unwrap().contains("not held by this harness"));
        assert_eq!(reply_text(&store, run), "Your sister is Priya.");
        assert!(asked.lock().unwrap().is_empty());
    }

    #[test]
    fn a_turn_already_closed_can_still_be_erased_within_the_window() {
        let (host, store, _) = host_with_shell();
        let (session, _, run, _reader) = answered(&host, "Erase");
        call(&host, protocol::COMPLETE, json!({ "session": session, "turn_id": run }));
        assert_eq!(store.run(run).unwrap().unwrap().state, RunState::Done);
        assert_eq!(redact(&host, &session, run, "forget")["redacted"], 3);
    }

    #[test]
    fn a_malformed_redact_is_refused_without_quoting_it() {
        let (host, _, _) = host_with_shell();
        let (session, _, run, _reader) = answered(&host, "Erase");
        let digest = Needle::of("Priya").sha256;
        for event in [
            json!({ "kind": "redact", "request_id": "forget", "needles": [{ "sha256": digest.to_uppercase(), "len": 5 }] }),
            json!({ "kind": "redact", "request_id": "forget", "needles": [{ "sha256": digest, "len": 0 }] }),
            json!({ "kind": "redact", "request_id": "forget", "needles": [] }),
            json!({ "kind": "redact", "request_id": "forget", "needles": [{ "sha256": 7, "len": 5 }] }),
            json!({ "kind": "redact", "needles": [{ "sha256": digest, "len": 5 }] }),
        ] {
            let reply = call(&host, protocol::EVENT, json!({ "session": session, "turn_id": run, "event": event }));
            let why = reply["refused"].as_str().expect("refused");
            assert!(!why.to_lowercase().contains(&digest), "the refusal does not repeat a digest");
        }
    }

    #[test]
    fn without_a_run_store_a_redact_is_refused() {
        let host = Host::new(vec![]);
        let session = call(&host, protocol::ATTACH, json!({ "id": "pi", "name": "pi" }))["session"].as_str().unwrap().to_string();
        let agent = host.ensure_main("pi").unwrap();
        let _answer = host.send_to(&agent, Turn::new("hi")).unwrap();
        let run = call(&host, protocol::POLL, json!({ "session": session }))["turn_id"].as_u64().unwrap();
        assert!(redact(&host, &session, run, "forget")["refused"].as_str().unwrap().contains("keeps no runs"));
    }
}
