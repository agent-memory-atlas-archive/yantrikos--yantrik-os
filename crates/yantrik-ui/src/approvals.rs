//! Pending approval requests, and the grants only a person can create.
//!
//! # Why this exists
//!
//! Someone typed "Delete the dentist appointment from my calendar." The mind found the event,
//! called `os_act calendar delete_event`, and the MCP bridge refused it: `delete_event` is
//! graded `sensitive` and the bridge's own ceiling is `standard`. The mind then told the person
//! to "approve it right here in the chat panel (a /approve prompt should appear), or raise
//! YOS_MCP_MAX_PERMISSION in the settings." No such prompt existed. Somebody asked for an
//! ordinary thing and was told to set an environment variable.
//!
//! The missing piece was never a policy. It was a way to *ask*. This module is that: a request
//! a caller can raise, a card the shell puts in front of the person, and a grant that exists
//! only because a person pressed a button.
//!
//! # The invariant: a mind must not be able to approve itself
//!
//! [`grant`] and [`deny`] are `pub(crate)` and have exactly one caller: the Slint callback in
//! `control_approvals.rs` that a person's click reaches. There is no `app.act` action on the
//! shell that grants or denies, and `published_actions_cannot_grant` in `control_approvals.rs`
//! reads the source of every `control*.rs` file and fails if one appears. Everything a caller on
//! the socket *can* do — raise a request, poll it, burn a grant — is `safe`, because none of it
//! decides anything. The decision is a click.
//!
//! That is the whole security argument, and it rests on the surface being small enough to read.
//! Do not add a way to grant from code. If some future automation needs standing permission,
//! that is `tool_permission` in the machine's settings — the owner's standing policy, set at the
//! keyboard — not a grant minted here.
//!
//! # What a grant is bound to
//!
//! One grant, one action, one exact set of arguments, once. [`Store::consume`] compares the
//! canonical JSON of the arguments handed to it against the canonical JSON of the arguments the
//! card showed the person. Key order does not matter (nobody reads JSON key order, and the
//! transport does not preserve it); any value change does, because that is what the person
//! looked at. A second consume of the same grant authorises nothing.
//!
//! # What it deliberately is not
//!
//! Not persistent: the store is memory, and a shell restart drops every request and grant. That
//! is the correct failure — a grant that survives the thing that was asking is a grant nobody
//! remembers giving. Not an "always allow": there is no way to record a standing yes here. Not
//! run-bound (see `design/next-focus-2026-09.md` §3 and its stop rule) — a grant is bound to the
//! arguments, not to a run, because runs do not exist yet.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How long a request waits for an answer before it stops being one.
///
/// Two minutes is roughly how long a person takes to notice a card, read six lines of arguments
/// and decide. Longer and a forgotten card is still live when they have moved on; shorter and an
/// honest "hold on" loses.
pub const REQUEST_TTL: Duration = Duration::from_secs(120);

/// How long a grant survives being given, if nobody burns it.
///
/// Short on purpose. The grant exists to carry one decision across one round trip; anything
/// slower than that is a different situation and deserves to be asked about again.
pub const GRANT_TTL: Duration = Duration::from_secs(60);

/// How many requests may be waiting at once.
///
/// This is the approval-fatigue bound, not a resource bound. A stack of cards is a stack nobody
/// reads, and a caller that can make ten of them can make the eleventh — the dangerous one —
/// look like more of the same. Three fits on screen and stays legible.
pub const MAX_PENDING: usize = 3;

/// How long a denial silences the identical request.
///
/// Without this, "no" costs the person one click and the caller one retry, which is a losing
/// trade for the person. Re-asking the same thing inside this window is refused with a message
/// that says so; anything else — different arguments, a different action, or the same one two
/// minutes later — is allowed through, because a person changing their mind is normal.
pub const DENIAL_QUIET: Duration = Duration::from_secs(120);

/// How many decided requests are kept for the transcript record.
const RECORD_TAIL: usize = 8;

/// Where a request is. `Consumed` is reported rather than folded into `Granted` or `Expired`:
/// a caller that polls after burning its grant asked a real question, and "the grant you were
/// given has been used" is the true answer to it. Telling it `granted` invites a replay that
/// would be refused anyway; telling it `expired` is simply false.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Pending,
    Granted,
    Denied,
    Expired,
    Consumed,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::Granted => "granted",
            Status::Denied => "denied",
            Status::Expired => "expired",
            Status::Consumed => "consumed",
        }
    }

    /// Whether anything more can happen to a request in this state.
    pub fn is_final(self) -> bool {
        !matches!(self, Status::Pending | Status::Granted)
    }
}

/// What the person was asked, and what they said.
#[derive(Clone, Debug)]
struct Record {
    id: String,
    requester: String,
    app: String,
    action: String,
    args: serde_json::Value,
    /// The arguments as the grant is bound to them. Computed once, at request time, so the
    /// bytes the person's card was built from are the bytes a consume is compared against.
    canonical: String,
    grade: String,
    purpose: String,
    created: Instant,
    /// Wall-clock `HH:MM` for the transcript record. `Instant` cannot render as a time of day,
    /// and the record a person reads afterwards is about when, not about how long ago.
    created_at: String,
    decided: Option<Instant>,
    decided_at: String,
    /// The stored state. Expiry is not stored: it is a fact about the clock, derived on every
    /// read, so a request cannot be alive merely because nothing looked at it.
    state: Status,
}

impl Record {
    fn status(&self, now: Instant) -> Status {
        match self.state {
            Status::Pending if now.duration_since(self.created) >= REQUEST_TTL => Status::Expired,
            Status::Granted => match self.decided {
                Some(at) if now.duration_since(at) >= GRANT_TTL => Status::Expired,
                _ => Status::Granted,
            },
            other => other,
        }
    }
}

/// One request as the UI and `describe` see it.
#[derive(Clone, Debug)]
pub struct Card {
    pub id: String,
    pub requester: String,
    pub app: String,
    pub action: String,
    pub grade: String,
    pub purpose: String,
    /// The arguments, one `key: value` line each, in the order a person reads them (sorted, the
    /// same order the grant is bound in — so what is shown and what is bound cannot drift).
    pub args_lines: String,
    /// A sentence to put in front of the buttons, or empty. See [`warning_for`].
    pub warning: String,
    pub status: Status,
    /// The one-line transcript record, once this has been decided. Empty while pending.
    pub record: String,
    pub age_secs: u64,
}

/// Canonical JSON: object keys sorted, everything else as serde renders it.
///
/// `serde_json`'s map is a `BTreeMap` today, so `to_string` already sorts — but that is a
/// feature flag away from being insertion order (`preserve_order`), and a grant that silently
/// stops matching when somebody enables a Cargo feature is the worst kind of bug to find. The
/// sort is written out so the guarantee belongs to this function.
///
/// Arrays keep their order. An array is data, and `["a","b"]` is not `["b","a"]`.
pub fn canonical(value: &serde_json::Value) -> String {
    fn walk(value: &serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                let mut out = serde_json::Map::new();
                for key in keys {
                    out.insert(key.clone(), walk(&map[key]));
                }
                serde_json::Value::Object(out)
            }
            serde_json::Value::Array(items) => {
                serde_json::Value::Array(items.iter().map(walk).collect())
            }
            other => other.clone(),
        }
    }
    walk(value).to_string()
}

/// The arguments as lines a person can read, in the order the grant is bound in.
pub fn args_lines(value: &serde_json::Value) -> String {
    let Some(map) = value.as_object() else {
        // Not an object. Show it rather than hiding it: a caller that sent something odd should
        // not get a card that looks empty.
        return match value {
            serde_json::Value::Null => "(no arguments)".to_string(),
            other => other.to_string(),
        };
    };
    if map.is_empty() {
        return "(no arguments)".to_string();
    }
    let mut keys: Vec<&String> = map.keys().collect();
    keys.sort();
    keys.iter()
        .map(|key| {
            let value = &map[*key];
            // A string argument reads better without its quotes; everything else is shown as
            // JSON, because `true` and `"true"` are different answers to the same question.
            let shown = match value {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            format!("{key}: {shown}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The warning line, or empty.
///
/// Two sources, because the two things a person needs warning about are different. The grade is
/// the OS's own judgement about the action; the purpose is the app's own sentence about it, and
/// `delete_event` publishes "It is not recoverable" there. A card that shows the grade but drops
/// that sentence is the exact failure commit d73760d fixed one layer down.
pub fn warning_for(grade: &str, purpose: &str) -> String {
    let lower = purpose.to_ascii_lowercase();
    let unrecoverable = [
        "not recoverable",
        "cannot be undone",
        "can't be undone",
        "irreversible",
        "permanently",
        "permanent",
        "no undo",
    ]
    .iter()
    .any(|phrase| lower.contains(phrase));

    match (grade == "dangerous", unrecoverable) {
        (true, true) => "This is graded dangerous and the app says it cannot be undone.".into(),
        (true, false) => "This is graded dangerous — it can destroy work or state.".into(),
        (false, true) => "The app says this cannot be undone.".into(),
        (false, false) => String::new(),
    }
}

/// What [`Store::request`] answers with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Requested {
    pub id: String,
    /// Always `pending`. Named rather than assumed, because the caller puts it in a JSON reply
    /// and a literal there would be a second place for the truth to live.
    pub status: Status,
}

/// The requests this shell is holding. See the module doc for what may mutate it.
pub struct Store {
    records: Vec<Record>,
    next: u64,
}

impl Default for Store {
    fn default() -> Self {
        Self::new()
    }
}

impl Store {
    pub fn new() -> Self {
        Store { records: Vec::new(), next: 1 }
    }

    /// Raise a request. The only thing this does is make a card appear.
    ///
    /// `now` and `at` are passed in rather than read here so the tests can move the clock. The
    /// public wrappers below supply the real ones; nothing outside this module can pick a time.
    pub fn request(
        &mut self,
        requester: &str,
        app: &str,
        action: &str,
        args: serde_json::Value,
        grade: &str,
        purpose: &str,
        now: Instant,
        at: &str,
    ) -> Result<Requested, String> {
        self.prune(now);

        let canonical = canonical(&args);

        // Asked and still waiting: the same question, so the same card. A caller that retries
        // (a poll that timed out, a model that repeated itself) must not put a second identical
        // card in front of the person — that is how a stack of cards becomes noise.
        if let Some(existing) = self.records.iter().find(|r| {
            r.status(now) == Status::Pending
                && r.app == app
                && r.action == action
                && r.canonical == canonical
        }) {
            return Ok(Requested { id: existing.id.clone(), status: Status::Pending });
        }

        // Asked and answered no. Re-asking immediately is how a refusal becomes a war of
        // attrition the person loses by clicking Allow to make it stop.
        if let Some(denied) = self.records.iter().find(|r| {
            r.state == Status::Denied
                && r.app == app
                && r.action == action
                && r.canonical == canonical
                && denied_recently(r, now)
        }) {
            let ago = denied.decided.map(|d| now.duration_since(d).as_secs()).unwrap_or(0);
            return Err(format!(
                "the person denied `{app}.{action}` with these exact arguments {ago}s ago, so \
                 this was not put in front of them again. Do not ask a third time unless they \
                 bring it up themselves."
            ));
        }

        let waiting = self.records.iter().filter(|r| r.status(now) == Status::Pending).count();
        if waiting >= MAX_PENDING {
            return Err(format!(
                "{waiting} approval requests are already waiting for an answer, which is the \
                 most this shell will show at once. A person cannot read a stack of them, and a \
                 stack is how the one that matters gets waved through. Wait for the ones on \
                 screen to be answered or to expire ({}s each), then ask again.",
                REQUEST_TTL.as_secs()
            ));
        }

        let id = format!("appr-{}", self.next);
        self.next += 1;
        self.records.push(Record {
            id: id.clone(),
            requester: requester.trim().to_string(),
            app: app.to_string(),
            action: action.to_string(),
            args,
            canonical,
            grade: grade.to_string(),
            purpose: purpose.trim().to_string(),
            created: now,
            created_at: at.to_string(),
            decided: None,
            decided_at: String::new(),
            state: Status::Pending,
        });
        Ok(Requested { id, status: Status::Pending })
    }

    /// Where a request stands. `None` means no request by that id — which is not the same as
    /// expired, and a caller that cannot tell those apart will retry forever on a typo.
    pub fn status(&self, id: &str, now: Instant) -> Option<Status> {
        self.records.iter().find(|r| r.id == id).map(|r| r.status(now))
    }

    /// A person pressed Allow. **UI only** — see the module doc.
    pub(crate) fn grant(&mut self, id: &str, now: Instant, at: &str) -> Result<(), String> {
        self.decide(id, Status::Granted, now, at)
    }

    /// A person pressed Deny. **UI only** — see the module doc.
    pub(crate) fn deny(&mut self, id: &str, now: Instant, at: &str) -> Result<(), String> {
        self.decide(id, Status::Denied, now, at)
    }

    fn decide(
        &mut self,
        id: &str,
        decision: Status,
        now: Instant,
        at: &str,
    ) -> Result<(), String> {
        let Some(record) = self.records.iter_mut().find(|r| r.id == id) else {
            return Err(format!("no approval request `{id}`"));
        };
        // Deciding twice is a double click, not a second decision. And a card that has already
        // expired must not become a grant: the request it stood for is gone, and the person
        // clicking now is answering a question nobody is asking any more.
        match record.status(now) {
            Status::Pending => {
                record.state = decision;
                record.decided = Some(now);
                record.decided_at = at.to_string();
                Ok(())
            }
            other => Err(format!(
                "`{id}` is {} and cannot be decided now",
                other.as_str()
            )),
        }
    }

    /// Burn a grant, if the triple matches exactly. Succeeds at most once per grant.
    ///
    /// The refusal says which part differed, because a caller that is told only "no" will
    /// retry the same thing. Telling it the arguments changed is what makes it stop and look.
    pub fn consume(
        &mut self,
        id: &str,
        app: &str,
        action: &str,
        args: &serde_json::Value,
        now: Instant,
    ) -> Result<(), String> {
        let Some(record) = self.records.iter_mut().find(|r| r.id == id) else {
            return Err(format!(
                "no approval request `{id}` — it may have been dropped when the shell restarted. \
                 Ask again."
            ));
        };

        match record.status(now) {
            Status::Granted => {}
            Status::Pending => {
                return Err(format!(
                    "`{id}` has not been answered yet; nothing was authorised. Keep polling \
                     approval_status, or let it expire."
                ))
            }
            Status::Denied => {
                return Err(format!(
                    "the person denied `{id}`; nothing was authorised and nothing was run."
                ))
            }
            Status::Consumed => {
                return Err(format!(
                    "`{id}` was already used. A grant authorises one action once; this second \
                     use authorises nothing. Ask again if the action still needs doing."
                ))
            }
            Status::Expired => {
                return Err(format!(
                    "`{id}` has expired — a grant lasts {}s and a request {}s. Nothing was \
                     authorised. Ask again.",
                    GRANT_TTL.as_secs(),
                    REQUEST_TTL.as_secs()
                ))
            }
        }

        // The triple, one part at a time, so the refusal names the part that moved.
        if record.app != app {
            return Err(format!(
                "`{id}` was approved for app `{}`, not `{app}`. Nothing was authorised.",
                record.app
            ));
        }
        if record.action != action {
            return Err(format!(
                "`{id}` was approved for action `{}.{}`, not `{}.{action}`. Nothing was \
                 authorised.",
                record.app, record.action, record.app
            ));
        }
        let given = canonical(args);
        if record.canonical != given {
            return Err(format!(
                "`{id}` was approved for `{}.{}` with arguments {}, and this call carries {}. \
                 The person approved what they were shown; a different argument is a different \
                 action. Nothing was authorised.",
                record.app, record.action, record.canonical, given
            ));
        }

        record.state = Status::Consumed;
        Ok(())
    }

    /// Everything the UI and `describe` show: what is waiting, then what was recently decided.
    pub fn cards(&self, now: Instant) -> Vec<Card> {
        let mut decided: Vec<Card> = Vec::new();
        let mut pending: Vec<Card> = Vec::new();
        for record in &self.records {
            let status = record.status(now);
            let card = Card {
                id: record.id.clone(),
                requester: record.requester.clone(),
                app: record.app.clone(),
                action: record.action.clone(),
                grade: record.grade.clone(),
                purpose: record.purpose.clone(),
                args_lines: args_lines(&record.args),
                warning: warning_for(&record.grade, &record.purpose),
                status,
                record: record_line(record, status),
                age_secs: now.duration_since(record.created).as_secs(),
            };
            if status == Status::Pending {
                pending.push(card);
            } else {
                decided.push(card);
            }
        }
        // Newest last in the record strip (a transcript reads downwards); the pending cards go
        // underneath them, because the thing waiting on you belongs closest to the buttons.
        let skip = decided.len().saturating_sub(RECORD_TAIL);
        let mut out: Vec<Card> = decided.into_iter().skip(skip).collect();
        out.extend(pending);
        out
    }

    /// Only what is waiting for an answer. What `describe shell` publishes.
    pub fn pending(&self, now: Instant) -> Vec<Card> {
        self.cards(now).into_iter().filter(|c| c.status == Status::Pending).collect()
    }

    /// Drop what nobody will look at again, so a long session does not grow a list forever.
    ///
    /// Decided records are kept well past their decision: they are the transcript, and the
    /// denial window reads them. Ten minutes covers both and is far shorter than a session.
    fn prune(&mut self, now: Instant) {
        const KEEP: Duration = Duration::from_secs(600);
        self.records.retain(|r| {
            let status = r.status(now);
            if !status.is_final() {
                return true;
            }
            let since = r.decided.unwrap_or(r.created);
            now.duration_since(since) < KEEP
        });
    }
}

fn denied_recently(record: &Record, now: Instant) -> bool {
    record
        .decided
        .map(|at| now.duration_since(at) < DENIAL_QUIET)
        .unwrap_or(false)
}

/// The line that stays in the conversation after the card is gone.
fn record_line(record: &Record, status: Status) -> String {
    let what = format!("{}.{}", record.app, record.action);
    match status {
        Status::Pending => String::new(),
        Status::Granted => format!("Allowed once: {what} — {}", record.decided_at),
        Status::Consumed => format!("Allowed once: {what} — {}", record.decided_at),
        Status::Denied => format!("Denied: {what} — {}", record.decided_at),
        Status::Expired if record.state == Status::Granted => {
            format!("Allowed once: {what} — {} (grant expired unused)", record.decided_at)
        }
        Status::Expired => format!("Not answered: {what} — asked {}", record.created_at),
    }
}

// ── The one store this shell has ────────────────────────────────────

fn store() -> &'static Mutex<Store> {
    static STORE: OnceLock<Mutex<Store>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(Store::new()))
}

/// A poisoned lock means a previous holder panicked mid-update. The records are plain data with
/// no invariant that a panic could have half-broken, so the contents are still readable and
/// refusing every approval forever is the worse outcome.
fn locked() -> std::sync::MutexGuard<'static, Store> {
    store().lock().unwrap_or_else(|e| e.into_inner())
}

fn hhmm() -> String {
    crate::app_context::current_time_hhmm()
}

pub fn request(
    requester: &str,
    app: &str,
    action: &str,
    args: serde_json::Value,
    grade: &str,
    purpose: &str,
) -> Result<Requested, String> {
    locked().request(requester, app, action, args, grade, purpose, Instant::now(), &hhmm())
}

pub fn status(id: &str) -> Option<Status> {
    locked().status(id, Instant::now())
}

/// **UI only.** See the module doc: the single caller is the Allow button's callback.
pub(crate) fn grant(id: &str) -> Result<(), String> {
    locked().grant(id, Instant::now(), &hhmm())
}

/// **UI only.** See the module doc: the single caller is the Deny button's callback.
pub(crate) fn deny(id: &str) -> Result<(), String> {
    locked().deny(id, Instant::now(), &hhmm())
}

pub fn consume(
    id: &str,
    app: &str,
    action: &str,
    args: &serde_json::Value,
) -> Result<(), String> {
    locked().consume(id, app, action, args, Instant::now())
}

pub fn cards() -> Vec<Card> {
    locked().cards(Instant::now())
}

pub fn pending() -> Vec<Card> {
    locked().pending(Instant::now())
}

#[cfg(test)]
mod approvals_tests {
    use super::*;

    fn args(json: serde_json::Value) -> serde_json::Value {
        json
    }

    /// A fresh store per test. Nothing here touches the process-wide one, so the tests do not
    /// have to run in any order and cannot interfere with each other.
    fn ask(store: &mut Store, now: Instant) -> String {
        store
            .request(
                "hermes",
                "calendar",
                "delete_event",
                args(serde_json::json!({"id": "evt-3", "confirm": true})),
                "sensitive",
                "Delete an event from the calendar. It is not recoverable.",
                now,
                "12:03",
            )
            .expect("a first request is accepted")
            .id
    }

    #[test]
    fn approvals_a_grant_authorises_once_and_only_once() {
        let mut store = Store::new();
        let now = Instant::now();
        let id = ask(&mut store, now);
        let call = serde_json::json!({"id": "evt-3", "confirm": true});

        assert_eq!(store.status(&id, now), Some(Status::Pending));
        store.grant(&id, now, "12:03").expect("a person pressed Allow");
        assert_eq!(store.status(&id, now), Some(Status::Granted));

        store
            .consume(&id, "calendar", "delete_event", &call, now)
            .expect("the grant covers exactly this call");

        assert_eq!(store.status(&id, now), Some(Status::Consumed));
        let again = store
            .consume(&id, "calendar", "delete_event", &call, now)
            .expect_err("a grant is single use");
        assert!(again.contains("already used"), "{again}");
    }

    #[test]
    fn approvals_key_order_does_not_matter_but_any_value_does() {
        let mut store = Store::new();
        let now = Instant::now();
        let id = ask(&mut store, now);
        store.grant(&id, now, "12:03").unwrap();

        // The same arguments, written the other way round. JSON objects are unordered and the
        // transport does not promise an order, so a reshuffle must not invalidate a grant.
        let reordered = serde_json::json!({"confirm": true, "id": "evt-3"});
        store
            .consume(&id, "calendar", "delete_event", &reordered, now)
            .expect("key order is not part of what the person approved");
    }

    #[test]
    fn approvals_a_changed_argument_invalidates_the_grant() {
        for changed in [
            serde_json::json!({"id": "evt-4", "confirm": true}),
            serde_json::json!({"id": "evt-3", "confirm": false}),
            serde_json::json!({"id": "evt-3"}),
            serde_json::json!({"id": "evt-3", "confirm": true, "force": true}),
            // `"true"` is not `true`. A string that looks like a boolean is a different value
            // and the person approved the one they were shown.
            serde_json::json!({"id": "evt-3", "confirm": "true"}),
        ] {
            let mut store = Store::new();
            let now = Instant::now();
            let id = ask(&mut store, now);
            store.grant(&id, now, "12:03").unwrap();
            let err = store
                .consume(&id, "calendar", "delete_event", &changed, now)
                .expect_err("a different argument is a different action");
            assert!(err.contains("Nothing was authorised"), "{changed}: {err}");
            assert!(err.contains("arguments"), "the refusal names the part that moved: {err}");
            assert_eq!(
                store.status(&id, now),
                Some(Status::Granted),
                "a refused consume must not burn the grant"
            );
        }
    }

    #[test]
    fn approvals_a_changed_app_or_action_invalidates_the_grant() {
        let mut store = Store::new();
        let now = Instant::now();
        let id = ask(&mut store, now);
        store.grant(&id, now, "12:03").unwrap();
        let call = serde_json::json!({"id": "evt-3", "confirm": true});

        let wrong_app = store
            .consume(&id, "notes", "delete_event", &call, now)
            .expect_err("a grant is bound to one app");
        assert!(wrong_app.contains("app `calendar`"), "{wrong_app}");

        let wrong_action = store
            .consume(&id, "calendar", "delete_all_events", &call, now)
            .expect_err("a grant is bound to one action");
        assert!(wrong_action.contains("calendar.delete_event"), "{wrong_action}");
    }

    #[test]
    fn approvals_denial_prevents_consumption() {
        let mut store = Store::new();
        let now = Instant::now();
        let id = ask(&mut store, now);
        store.deny(&id, now, "12:03").expect("a person pressed Deny");
        assert_eq!(store.status(&id, now), Some(Status::Denied));

        let err = store
            .consume(
                &id,
                "calendar",
                "delete_event",
                &serde_json::json!({"id": "evt-3", "confirm": true}),
                now,
            )
            .expect_err("a denial authorises nothing");
        assert!(err.contains("denied"), "{err}");

        // And granting afterwards is not a second chance at the same card.
        let flip = store.grant(&id, now, "12:04").expect_err("a decided card is decided");
        assert!(flip.contains("denied"), "{flip}");
    }

    #[test]
    fn approvals_an_unanswered_request_expires() {
        let mut store = Store::new();
        let now = Instant::now();
        let id = ask(&mut store, now);
        let later = now + REQUEST_TTL + Duration::from_secs(1);

        assert_eq!(store.status(&id, later), Some(Status::Expired));
        let err = store.grant(&id, later, "12:06").expect_err("an expired card cannot be granted");
        assert!(err.contains("expired"), "{err}");
    }

    #[test]
    fn approvals_a_grant_expires_unused() {
        let mut store = Store::new();
        let now = Instant::now();
        let id = ask(&mut store, now);
        store.grant(&id, now, "12:03").unwrap();

        let later = now + GRANT_TTL + Duration::from_secs(1);
        assert_eq!(store.status(&id, later), Some(Status::Expired));
        let err = store
            .consume(
                &id,
                "calendar",
                "delete_event",
                &serde_json::json!({"id": "evt-3", "confirm": true}),
                later,
            )
            .expect_err("an expired grant authorises nothing");
        assert!(err.contains("expired"), "{err}");
    }

    #[test]
    fn approvals_flooding_is_refused() {
        let mut store = Store::new();
        let now = Instant::now();
        for n in 0..MAX_PENDING {
            store
                .request(
                    "hermes",
                    "calendar",
                    "delete_event",
                    serde_json::json!({"id": format!("evt-{n}")}),
                    "sensitive",
                    "Delete an event.",
                    now,
                    "12:03",
                )
                .unwrap_or_else(|e| panic!("request {n} should be accepted: {e}"));
        }
        let err = store
            .request(
                "hermes",
                "calendar",
                "delete_event",
                serde_json::json!({"id": "evt-99"}),
                "sensitive",
                "Delete an event.",
                now,
                "12:03",
            )
            .expect_err("a flood is refused");
        assert!(err.contains("already waiting"), "{err}");
        assert_eq!(store.pending(now).len(), MAX_PENDING);
    }

    #[test]
    fn approvals_the_same_question_twice_is_one_card() {
        let mut store = Store::new();
        let now = Instant::now();
        let first = ask(&mut store, now);
        let second = ask(&mut store, now);
        assert_eq!(first, second, "a retry must not stack a second identical card");
        assert_eq!(store.pending(now).len(), 1);
    }

    #[test]
    fn approvals_a_denial_silences_the_identical_request() {
        let mut store = Store::new();
        let now = Instant::now();
        let id = ask(&mut store, now);
        store.deny(&id, now, "12:03").unwrap();

        let err = store
            .request(
                "hermes",
                "calendar",
                "delete_event",
                serde_json::json!({"id": "evt-3", "confirm": true}),
                "sensitive",
                "Delete an event.",
                now + Duration::from_secs(5),
                "12:03",
            )
            .expect_err("asking again straight after a no is refused");
        assert!(err.contains("denied"), "{err}");

        // Different arguments are a different question, and always allowed through.
        store
            .request(
                "hermes",
                "calendar",
                "delete_event",
                serde_json::json!({"id": "evt-9"}),
                "sensitive",
                "Delete an event.",
                now + Duration::from_secs(5),
                "12:03",
            )
            .expect("a different question is not the denied one");

        // And once the quiet window has passed, the person may be asked again.
        store
            .request(
                "hermes",
                "calendar",
                "delete_event",
                serde_json::json!({"id": "evt-3", "confirm": true}),
                "sensitive",
                "Delete an event.",
                now + DENIAL_QUIET + Duration::from_secs(1),
                "12:03",
            )
            .expect("a denial is not permanent");
    }

    #[test]
    fn approvals_an_unknown_id_is_not_an_expiry() {
        let store = Store::new();
        assert_eq!(store.status("appr-404", Instant::now()), None);
    }

    #[test]
    fn approvals_canonical_json_sorts_keys_and_keeps_array_order() {
        let a = serde_json::json!({"b": 1, "a": {"d": 4, "c": 3}});
        let b = serde_json::json!({"a": {"c": 3, "d": 4}, "b": 1});
        assert_eq!(canonical(&a), canonical(&b));
        assert_eq!(canonical(&a), r#"{"a":{"c":3,"d":4},"b":1}"#);

        let one = serde_json::json!({"to": ["ann", "bob"]});
        let other = serde_json::json!({"to": ["bob", "ann"]});
        assert_ne!(
            canonical(&one),
            canonical(&other),
            "array order is data — two recipients in the other order is a different send"
        );
    }

    #[test]
    fn approvals_the_card_shows_what_the_grant_is_bound_to() {
        let mut store = Store::new();
        let now = Instant::now();
        let id = ask(&mut store, now);
        let cards = store.pending(now);
        assert_eq!(cards.len(), 1);
        let card = &cards[0];
        assert_eq!(card.id, id);
        assert_eq!(card.requester, "hermes");
        assert_eq!(card.grade, "sensitive");
        // Every argument the grant is bound to is on the card, in the same order.
        assert_eq!(card.args_lines, "confirm: true\nid: evt-3");
        assert_eq!(
            card.warning, "The app says this cannot be undone.",
            "the app's own sentence about recoverability has to reach the person"
        );
    }

    #[test]
    fn approvals_a_dangerous_grade_always_warns() {
        assert!(warning_for("dangerous", "Kill a process.").contains("dangerous"));
        assert!(warning_for("dangerous", "Erase the disk. It is not recoverable.")
            .contains("cannot be undone"));
        assert!(warning_for("standard", "Open a note.").is_empty());
    }

    #[test]
    fn approvals_a_decision_leaves_a_line_in_the_transcript() {
        let mut store = Store::new();
        let now = Instant::now();
        let id = ask(&mut store, now);
        store.grant(&id, now, "12:03").unwrap();
        let record = store.cards(now).into_iter().find(|c| c.id == id).unwrap().record;
        assert_eq!(record, "Allowed once: calendar.delete_event — 12:03");

        let mut store = Store::new();
        let id = ask(&mut store, now);
        store.deny(&id, now, "12:05").unwrap();
        let record = store.cards(now).into_iter().find(|c| c.id == id).unwrap().record;
        assert_eq!(record, "Denied: calendar.delete_event — 12:05");
    }
}
