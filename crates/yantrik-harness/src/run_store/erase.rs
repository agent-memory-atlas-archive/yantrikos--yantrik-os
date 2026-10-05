//! Erasing a conversation's words from the run store, at the person's request (`redact`).
//!
//! # The acceptance rule
//!
//! A harness could otherwise scrub its own trail whenever it liked. So the store erases only when
//! all of these hold, and otherwise refuses with the reason and changes nothing:
//!
//! - **(a)** the request id names a question *this* run asked (a row in `requests`);
//! - **(b)** the person's stored answer to it is exactly [`ERASE_ANSWER`], and that was one of
//!   the question's offered options — a typed "erase" or a free answer does not count;
//! - **(c)** the run is still in flight, or ended no more than [`ERASE_WINDOW_MS`] ago;
//! - **(d)** the redaction comes from the harness and the session that hold the run.
//!
//! And once per question: a second `redact` for the same request is refused.
//!
//! # What it touches
//!
//! The words the person and the agent said, in every run of the same agent (harness and
//! conversation): the reply text (each run's `text` chunks, joined before matching), its thinking
//! (`thinking` deltas, joined the same way), why a run failed, and the prompt of each question the
//! agent asked. Never the record of what happened: tool calls and their output, usage, status,
//! states, owners, answers and options, sequence numbers and times all stay as they are, and so
//! does the order of the log. What was erased is recorded in `redactions` as the run, the request
//! and how many places, with no words and no digest.
//!
//! # How it reaches the disk
//!
//! `secure_delete` is on for the update, so the space the old text occupied is zeroed rather than
//! left free; the updates commit in one transaction; and then `wal_checkpoint(TRUNCATE)` copies
//! the new pages into the database and empties the write-ahead log, so the old page images leave
//! `runs.db-wal` too. What this cannot reach is below SQLite: the filesystem's own journal, blocks
//! a truncated file gave back, snapshots and backups.

use rusqlite::{params, OptionalExtension};
use serde_json::Value;

use super::{state_in, RunStore};
use crate::redact::{self, Needle};

/// The answer that lets a question's words be erased. Exact: the case matters.
pub const ERASE_ANSWER: &str = "Erase";

/// How long after a run ended its question may still be acted on: five minutes.
pub const ERASE_WINDOW_MS: i64 = 5 * 60 * 1000;

/// What a harness asked to erase, and who is asking.
#[derive(Debug, Clone)]
pub struct Erasure<'a> {
    /// The run that asked the Keep/Erase question.
    pub run_id: u64,
    pub request_id: &'a str,
    /// The harness and session the `redact` came from.
    pub harness: &'a str,
    pub owner: &'a str,
    pub needles: &'a [Needle],
}

/// What the store erased.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Erased {
    /// The agent whose conversation it was: its harness and conversation.
    pub harness: String,
    pub conversation: String,
    /// How many places in the run store.
    pub places: usize,
    /// Whether the write-ahead log was emptied. `false` only when another connection held it.
    pub checkpointed: bool,
}

/// One erasure, as it is kept: no words and no digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redaction {
    pub run_id: u64,
    pub request_id: String,
    pub places: u64,
    /// Unix milliseconds.
    pub at: i64,
}

/// Why nothing was erased.
#[derive(Debug, Clone, PartialEq)]
pub enum Refusal {
    NoSuchRun(u64),
    /// (d): the run belongs to another harness or another session.
    NotYours(u64),
    /// (a): this run never asked it.
    NoSuchRequest { run_id: u64, request_id: String },
    /// (b): the question has not been answered.
    NotAnswered(String),
    /// (b): it was answered with something other than the offered `Erase`.
    NotErase(String),
    /// (c): the run ended too long ago.
    Expired { run_id: u64, ended_ms_ago: i64 },
    /// Once per question.
    AlreadyErased(String),
    Storage(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::NoSuchRun(id) => write!(f, "run {id} does not exist"),
            Refusal::NotYours(id) => write!(f, "run {id} is not held by this harness and session"),
            Refusal::NoSuchRequest { run_id, request_id } => write!(f, "run {run_id} never asked {request_id:?}"),
            Refusal::NotAnswered(r) => write!(f, "the person has not answered {r:?}"),
            Refusal::NotErase(r) => write!(
                f,
                "the person's answer to {r:?} was not the offered {ERASE_ANSWER:?}, so nothing may be erased"
            ),
            Refusal::Expired { run_id, ended_ms_ago } => write!(
                f,
                "run {run_id} ended {}s ago; a redaction must come within {}s of its end",
                ended_ms_ago / 1000,
                ERASE_WINDOW_MS / 1000
            ),
            Refusal::AlreadyErased(r) => write!(f, "{r:?} has already been acted on; one redaction per question"),
            Refusal::Storage(e) => write!(f, "run store: {e}"),
        }
    }
}

impl From<rusqlite::Error> for Refusal {
    fn from(e: rusqlite::Error) -> Self {
        Refusal::Storage(e.to_string())
    }
}

impl From<super::RunError> for Refusal {
    fn from(e: super::RunError) -> Self {
        match e {
            super::RunError::NoSuchRun(id) => Refusal::NoSuchRun(id),
            other => Refusal::Storage(other.to_string()),
        }
    }
}

impl RunStore {
    /// Erase `e.needles` from the conversation of the agent that ran `e.run_id`, if the acceptance
    /// rule holds (see the module docs) at `now` (Unix milliseconds). The erasure is recorded with
    /// the places counted here; [`RunStore::set_redaction_places`] adds the shell's own.
    pub fn redact(&self, e: &Erasure<'_>, now: i64) -> Result<Erased, Refusal> {
        self.with_refusal(|db| {
            let previous: i64 = db.query_row("PRAGMA secure_delete", [], |r| r.get(0))?;
            db.pragma_update(None, "secure_delete", "ON")?;
            let outcome = erase_in(db, e, now);
            let checkpointed = match &outcome {
                Ok(_) => checkpoint(db),
                Err(_) => true,
            };
            db.pragma_update(None, "secure_delete", previous)?;
            outcome.map(|erased| Erased { checkpointed, ..erased })
        })
    }

    /// Add the places the shell erased from its own copies to the record of `request_id`.
    pub fn set_redaction_places(&self, run_id: u64, request_id: &str, places: u64) -> Result<(), super::RunError> {
        self.with(|db| {
            db.execute(
                "UPDATE redactions SET places = ?3 WHERE run_id = ?1 AND request_id = ?2",
                params![run_id as i64, request_id, places as i64],
            )?;
            Ok(())
        })
    }

    /// The erasures recorded against the runs of one agent, oldest first.
    pub fn redactions(&self, harness: &str, conversation: &str) -> Result<Vec<Redaction>, super::RunError> {
        self.with(|db| {
            let mut stmt = db.prepare(
                "SELECT d.run_id, d.request_id, d.places, d.at FROM redactions d JOIN runs r ON r.run_id = d.run_id
                 WHERE r.harness = ?1 AND r.conversation = ?2 ORDER BY d.at, d.run_id",
            )?;
            let rows = stmt
                .query_map(params![harness, conversation], |r| {
                    Ok(Redaction {
                        run_id: r.get::<_, i64>(0)? as u64,
                        request_id: r.get(1)?,
                        places: r.get::<_, i64>(2)? as u64,
                        at: r.get(3)?,
                    })
                })?
                .collect::<Result<_, _>>()?;
            Ok(rows)
        })
    }

    fn with_refusal<R>(&self, f: impl FnOnce(&mut rusqlite::Connection) -> Result<R, Refusal>) -> Result<R, Refusal> {
        let mut db = self.db.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&mut db)
    }

    /// Move a finished run's end `ms` into the past, for tests of the window.
    #[cfg(test)]
    pub(crate) fn backdate_end(&self, run_id: u64, ms: i64) {
        self.with(|db| {
            db.execute("UPDATE events SET at = at - ?2 WHERE run_id = ?1", params![run_id as i64, ms])?;
            Ok(())
        })
        .unwrap();
    }
}

fn erase_in(db: &mut rusqlite::Connection, e: &Erasure<'_>, now: i64) -> Result<Erased, Refusal> {
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let run_id = e.run_id;
    let state = state_in(&tx, run_id)?;
    let (harness, conversation, owner): (String, String, String) = tx.query_row(
        "SELECT harness, conversation, owner FROM runs WHERE run_id = ?1",
        params![run_id as i64],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    // (d) the harness and session that hold the run.
    if harness != e.harness || owner != e.owner {
        return Err(Refusal::NotYours(run_id));
    }
    // (a) a question this run asked.
    let request: Option<(String, String, Option<String>)> = tx
        .query_row(
            "SELECT prompt, state, answer FROM requests WHERE run_id = ?1 AND request_id = ?2",
            params![run_id as i64, e.request_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((prompt, asked_state, answer)) = request else {
        return Err(Refusal::NoSuchRequest { run_id, request_id: e.request_id.to_string() });
    };
    // (b) answered exactly the offered `Erase`.
    if asked_state != "answered" {
        return Err(Refusal::NotAnswered(e.request_id.to_string()));
    }
    let answer: Value = answer.and_then(|a| serde_json::from_str(&a).ok()).unwrap_or(Value::Null);
    let prompt: Value = serde_json::from_str(&prompt).unwrap_or(Value::Null);
    let offered = prompt["options"].as_array().is_some_and(|o| o.iter().any(|v| v.as_str() == Some(ERASE_ANSWER)));
    if answer.as_str() != Some(ERASE_ANSWER) || !offered {
        return Err(Refusal::NotErase(e.request_id.to_string()));
    }
    // (c) in flight, or ended within the window.
    if state.is_final() {
        let ended: i64 = tx.query_row(
            "SELECT at FROM events WHERE run_id = ?1 AND kind = 'state' ORDER BY seq DESC LIMIT 1",
            params![run_id as i64],
            |r| r.get(0),
        )?;
        if now - ended > ERASE_WINDOW_MS {
            return Err(Refusal::Expired { run_id, ended_ms_ago: now - ended });
        }
    }
    // Once per question: the record is the claim.
    let claimed = tx.execute(
        "INSERT OR IGNORE INTO redactions (run_id, request_id, places, at) VALUES (?1, ?2, 0, ?3)",
        params![run_id as i64, e.request_id, now],
    )?;
    if claimed == 0 {
        return Err(Refusal::AlreadyErased(e.request_id.to_string()));
    }

    let runs: Vec<i64> = {
        let mut stmt = tx.prepare("SELECT run_id FROM runs WHERE harness = ?1 AND conversation = ?2 ORDER BY run_id")?;
        let ids = stmt.query_map(params![harness, conversation], |r| r.get(0))?.collect::<Result<_, _>>()?;
        ids
    };
    let mut places = 0;
    for run in runs {
        places += erase_run(&tx, run, e.needles)?;
    }
    tx.execute(
        "UPDATE redactions SET places = ?3 WHERE run_id = ?1 AND request_id = ?2",
        params![run_id as i64, e.request_id, places as i64],
    )?;
    tx.commit()?;
    Ok(Erased { harness, conversation, places, checkpointed: false })
}

/// Erase the needles from one run's words. How many places.
fn erase_run(tx: &rusqlite::Transaction<'_>, run: i64, needles: &[Needle]) -> Result<usize, Refusal> {
    let rows: Vec<(i64, String, Value)> = {
        let mut stmt = tx.prepare(
            "SELECT seq, kind, payload FROM events WHERE run_id = ?1
             AND kind IN ('text', 'event', 'failure', 'request') ORDER BY seq",
        )?;
        let rows = stmt
            .query_map(params![run], |r| {
                let payload: String = r.get(2)?;
                Ok((r.get(0)?, r.get(1)?, serde_json::from_str(&payload).unwrap_or(Value::Null)))
            })?
            .collect::<Result<_, _>>()?;
        rows
    };
    let mut places = 0;
    let mut changed: Vec<(i64, Value)> = Vec::new();

    // The reply, and the thinking, each joined across its chunks before matching.
    for (kind, inner) in [("text", None), ("event", Some("thinking"))] {
        let chunks: Vec<&(i64, String, Value)> = rows
            .iter()
            .filter(|(_, k, p)| k == kind && inner.map_or(true, |i| p["kind"] == i))
            .filter(|(_, _, p)| p["delta"].is_string())
            .collect();
        let pieces: Vec<&str> = chunks.iter().map(|(_, _, p)| p["delta"].as_str().unwrap_or_default()).collect();
        if let Some((erased, n)) = redact::redact_pieces(&pieces, needles) {
            places += n;
            for ((seq, _, payload), (before, after)) in chunks.iter().zip(pieces.iter().zip(erased)) {
                if *before != after {
                    let mut payload = payload.clone();
                    payload["delta"] = Value::String(after);
                    changed.push((*seq, payload));
                }
            }
        }
    }
    // Why it failed, and what it asked the person: words, not the record of what happened. A
    // question keeps its id and options; only its prompt is text.
    for (seq, kind, payload) in &rows {
        let field: &[&str] = match kind.as_str() {
            "failure" => &["why"],
            "request" => &["prompt", "prompt"],
            _ => continue,
        };
        let mut payload = payload.clone();
        let n = redact_at(&mut payload, field, needles);
        if n > 0 {
            places += n;
            changed.push((*seq, payload));
        }
    }
    for (seq, payload) in changed {
        tx.execute(
            "UPDATE events SET payload = ?3 WHERE run_id = ?1 AND seq = ?2",
            params![run, seq, payload.to_string()],
        )?;
    }

    let questions: Vec<(String, String)> = {
        let mut stmt = tx.prepare("SELECT request_id, prompt FROM requests WHERE run_id = ?1")?;
        let rows = stmt.query_map(params![run], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
        rows
    };
    for (request_id, prompt) in questions {
        let mut prompt: Value = serde_json::from_str(&prompt).unwrap_or(Value::Null);
        // Counted once: the `request` event above holds the same words.
        if redact_at(&mut prompt, &["prompt"], needles) > 0 {
            tx.execute(
                "UPDATE requests SET prompt = ?3 WHERE run_id = ?1 AND request_id = ?2",
                params![run, request_id, prompt.to_string()],
            )?;
        }
    }
    Ok(places)
}

/// Erase the needles from the string at `path` inside `value`. How many places.
fn redact_at(value: &mut Value, path: &[&str], needles: &[Needle]) -> usize {
    let mut at = value;
    for key in path {
        match at.get_mut(*key) {
            Some(next) => at = next,
            None => return 0,
        }
    }
    match at {
        Value::String(_) => redact::redact_json(at, needles),
        _ => 0,
    }
}

/// Copy the new pages into the database and empty the write-ahead log. `false` when another
/// connection kept it from finishing.
fn checkpoint(db: &rusqlite::Connection) -> bool {
    match db.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get::<_, i64>(0)) {
        Ok(busy) => busy == 0,
        Err(e) => {
            tracing::error!(error = %e, "the run store could not empty its write-ahead log after an erasure");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redact::MARKER;
    use crate::run_store::{now_ms, RunState};
    use serde_json::json;

    const SECRET: &str = "Priya lives at 12 Elm Street";

    fn needles() -> Vec<Needle> {
        vec![Needle::of("Priya"), Needle::of("12 Elm Street")]
    }

    /// A store where run 1 (`mind`, conversation `main`, session `s1`) said the secret across two
    /// chunks, ran a tool that named it, asked Keep/Erase as `forget-1`, and was answered `answer`.
    fn store_with(store: RunStore, answer: &str) -> RunStore {
        store.start(1, "mind", "main", "s1").unwrap();
        store.append(1, "text", &json!({"delta": "Noted: Pri"})).unwrap();
        store.append(1, "text", &json!({"delta": "ya lives at 12 Elm Street."})).unwrap();
        store.append(1, "event", &json!({"kind": "thinking", "delta": "remember Priya"})).unwrap();
        store
            .append(1, "event", &json!({"kind": "tool_start", "call": "t1", "name": "notes", "args": {"text": SECRET}}))
            .unwrap();
        store.append(1, "event", &json!({"kind": "tool_output", "call": "t1", "delta": SECRET})).unwrap();
        store
            .ask(1, "forget-1", &json!({"prompt": "Forget that Priya lives at 12 Elm Street?", "options": ["Keep", "Erase"]}))
            .unwrap();
        store.answer(1, "forget-1", &json!(answer)).unwrap();
        store
    }

    fn erasure<'a>(run_id: u64, request_id: &'a str, needles: &'a [Needle]) -> Erasure<'a> {
        Erasure { run_id, request_id, harness: "mind", owner: "s1", needles }
    }

    fn payloads(store: &RunStore, run: u64) -> Vec<(String, Value)> {
        store.events(run, 0, super::super::PAGE_MAX).unwrap().events.into_iter().map(|e| (e.kind, e.payload)).collect()
    }

    fn text_of(store: &RunStore, run: u64) -> String {
        payloads(store, run)
            .into_iter()
            .filter(|(k, _)| k == "text")
            .map(|(_, p)| p["delta"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn after_erase_the_words_go_and_the_record_of_what_happened_stays() {
        let store = store_with(RunStore::in_memory().unwrap(), "Erase");
        let before = payloads(&store, 1);
        let needles = needles();
        let erased = store.redact(&erasure(1, "forget-1", &needles), now_ms()).unwrap();
        assert_eq!((erased.harness.as_str(), erased.conversation.as_str()), ("mind", "main"));

        // The reply, joined across its chunks: "Pri" + "ya" was one name.
        assert_eq!(text_of(&store, 1), format!("Noted: {MARKER} lives at {MARKER}."));
        let after = payloads(&store, 1);
        assert_eq!(after.len(), before.len(), "nothing is added to or taken from the log");
        let thinking = after.iter().find(|(_, p)| p["kind"] == "thinking").unwrap();
        assert_eq!(thinking.1["delta"], format!("remember {MARKER}"));
        // The question's words go; its id, options and answer stay.
        let asked = after.iter().find(|(k, _)| k == "request").unwrap();
        assert_eq!(asked.1["prompt"]["prompt"], format!("Forget that {MARKER} lives at {MARKER}?"));
        assert_eq!(asked.1["prompt"]["options"], json!(["Keep", "Erase"]));
        assert_eq!(after.iter().find(|(k, _)| k == "answer").unwrap().1["answer"], "Erase");
        // Tool calls are the record of what happened: untouched, byte for byte.
        for (kind, payload) in &before {
            if kind == "event" && payload["kind"] != "thinking" {
                assert!(after.contains(&(kind.clone(), payload.clone())), "{payload} was changed");
            }
        }
        // 2 in the reply, 1 in the thinking, 2 in the question.
        assert_eq!(erased.places, 5);
        let kept = store.redactions("mind", "main").unwrap();
        assert_eq!(kept.len(), 1);
        assert_eq!((kept[0].run_id, kept[0].request_id.as_str(), kept[0].places), (1, "forget-1", 5));
    }

    #[test]
    fn every_run_of_the_same_agent_is_erased_and_no_other_agents() {
        let store = store_with(RunStore::in_memory().unwrap(), "Erase");
        store.start(2, "mind", "main", "s1").unwrap();
        store.append(2, "text", &json!({"delta": "Priya again"})).unwrap();
        store.start(3, "mind", "c-other", "s1").unwrap();
        store.append(3, "text", &json!({"delta": "Priya elsewhere"})).unwrap();
        store.redact(&erasure(1, "forget-1", &needles()), now_ms()).unwrap();
        assert_eq!(text_of(&store, 2), format!("{MARKER} again"));
        assert_eq!(text_of(&store, 3), "Priya elsewhere", "another agent's conversation is not this one");
    }

    #[test]
    fn refused_unless_the_person_answered_the_offered_erase() {
        let n = needles();
        for answer in ["Keep", "erase", "ERASE", "Erase please"] {
            let store = store_with(RunStore::in_memory().unwrap(), answer);
            assert_eq!(
                store.redact(&erasure(1, "forget-1", &n), now_ms()),
                Err(Refusal::NotErase("forget-1".into())),
                "answered {answer:?}"
            );
            assert!(text_of(&store, 1).contains("Elm Street"), "nothing changed after {answer:?}");
            assert!(store.redactions("mind", "main").unwrap().is_empty());
        }
        // `Erase` typed as a free answer to a question that never offered it.
        let store = RunStore::in_memory().unwrap();
        store.start(1, "mind", "main", "s1").unwrap();
        store.ask(1, "free", &json!({"prompt": "What should I do?", "options": []})).unwrap();
        store.answer(1, "free", &json!("Erase")).unwrap();
        assert_eq!(store.redact(&erasure(1, "free", &n), now_ms()), Err(Refusal::NotErase("free".into())));
        // Not answered yet.
        store.ask(1, "open", &json!({"prompt": "Forget?", "options": ["Keep", "Erase"]})).unwrap();
        assert_eq!(store.redact(&erasure(1, "open", &n), now_ms()), Err(Refusal::NotAnswered("open".into())));
    }

    #[test]
    fn refused_for_a_question_never_asked_another_runs_another_session_and_a_second_use() {
        let store = store_with(RunStore::in_memory().unwrap(), "Erase");
        store.start(2, "mind", "main", "s1").unwrap();
        let n = needles();
        assert_eq!(
            store.redact(&erasure(1, "nope", &n), now_ms()),
            Err(Refusal::NoSuchRequest { run_id: 1, request_id: "nope".into() })
        );
        // Run 2 is the same agent's, but it is not the run that asked.
        assert_eq!(
            store.redact(&erasure(2, "forget-1", &n), now_ms()),
            Err(Refusal::NoSuchRequest { run_id: 2, request_id: "forget-1".into() })
        );
        assert_eq!(store.redact(&erasure(9, "forget-1", &n), now_ms()), Err(Refusal::NoSuchRun(9)));
        let other_session = Erasure { owner: "s2", ..erasure(1, "forget-1", &n) };
        assert_eq!(store.redact(&other_session, now_ms()), Err(Refusal::NotYours(1)));
        let other_harness = Erasure { harness: "pi", ..erasure(1, "forget-1", &n) };
        assert_eq!(store.redact(&other_harness, now_ms()), Err(Refusal::NotYours(1)));
        assert!(text_of(&store, 1).contains("Elm Street"));

        store.redact(&erasure(1, "forget-1", &n), now_ms()).unwrap();
        assert_eq!(
            store.redact(&erasure(1, "forget-1", &[Needle::of("Noted")]), now_ms()),
            Err(Refusal::AlreadyErased("forget-1".into()))
        );
        assert!(text_of(&store, 1).starts_with("Noted"), "the second one changed nothing");
    }

    #[test]
    fn a_run_that_ended_is_erased_within_five_minutes_and_refused_after() {
        let n = needles();
        let store = store_with(RunStore::in_memory().unwrap(), "Erase");
        store.transition(1, RunState::Done).unwrap();
        store.backdate_end(1, ERASE_WINDOW_MS - 5_000);
        assert!(store.redact(&erasure(1, "forget-1", &n), now_ms()).is_ok(), "within the window");

        let store = store_with(RunStore::in_memory().unwrap(), "Erase");
        store.transition(1, RunState::Done).unwrap();
        store.backdate_end(1, ERASE_WINDOW_MS + 5_000);
        assert!(matches!(store.redact(&erasure(1, "forget-1", &n), now_ms()), Err(Refusal::Expired { run_id: 1, .. })));
        assert!(text_of(&store, 1).contains("Elm Street"));
    }

    #[test]
    fn composed_and_decomposed_words_are_erased_alike() {
        let store = RunStore::in_memory().unwrap();
        store.start(1, "mind", "main", "s1").unwrap();
        store.append(1, "text", &json!({"delta": "We met at Cafe\u{301} Lune"})).unwrap();
        store.ask(1, "f", &json!({"prompt": "Forget the café?", "options": ["Keep", "Erase"]})).unwrap();
        store.answer(1, "f", &json!("Erase")).unwrap();
        let composed = [Needle::of("Caf\u{e9} Lune")];
        let erased = store.redact(&erasure(1, "f", &composed), now_ms()).unwrap();
        assert_eq!(erased.places, 1);
        assert_eq!(text_of(&store, 1), format!("We met at {MARKER}"));
    }

    #[test]
    fn the_old_text_leaves_the_write_ahead_log_and_the_database_file() {
        let dir = std::env::temp_dir().join(format!("yantrik-erase-{}-{}", std::process::id(), now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("runs.db");
        let wal = dir.join("runs.db-wal");
        let unique = "Zanzibar-Quokka-7741";
        let store = RunStore::open(&path).unwrap();
        store.start(1, "mind", "main", "s1").unwrap();
        store.append(1, "text", &json!({"delta": format!("the code is {unique}, keep it")})).unwrap();
        store.ask(1, "f", &json!({"prompt": "Forget the code?", "options": ["Keep", "Erase"]})).unwrap();
        store.answer(1, "f", &json!("Erase")).unwrap();
        let holds = |file: &std::path::Path| {
            let bytes = std::fs::read(file).unwrap_or_default();
            bytes.windows(unique.len()).any(|w| w == unique.as_bytes())
        };
        assert!(holds(&wal), "before: the write-ahead log holds the words, so the check below means something");

        let erased = store.redact(&erasure(1, "f", &[Needle::of(unique)]), now_ms()).unwrap();
        assert!(erased.checkpointed);
        assert!(!holds(&wal), "after: runs.db-wal holds no copy of the words");
        assert!(!holds(&path), "after: runs.db holds no copy of the words");
        assert!(text_of(&store, 1).contains(MARKER));
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
