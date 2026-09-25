//! Calendar service — event CRUD via filesystem-backed JSON storage.
//!
//! Events stored as individual `.json` files in `~/.local/share/yantrik/calendar/`.
//!
//! Methods:
//!   calendar.events        { start_date, end_date }        → Vec<CalendarEvent>
//!   calendar.get_event     { id }                          → CalendarEvent
//!   calendar.create_event  { title, start, end, ... }      → CalendarEvent
//!   calendar.update_event  { id, title?, start?, ... }     → CalendarEvent
//!   calendar.delete_event  { id }                          → ()
//!   calendar.upsert_remote { remote_id, title, start, ... } → CalendarEvent
//!   calendar.revision      { }                             → CalendarRevision
//!
//! Those parameter names are not written out here any more. They come from
//! `yantrik_ipc_contracts::calendar`, which the calendar app builds its requests from, because
//! this list and the app's calls used to disagree while each looked right in its own file.
//!
//! This service is the machine's one calendar. The built-in companion's calendar tools used to
//! keep their own events in SQLite, so an appointment the mind made was somewhere the Calendar
//! app would never show. They call these methods now, which is what the last three exist for:
//! a mind needs to read one event before changing it, and a sync needs a way in that does not
//! store the same Google event twice.
//!
//! `calendar.revision` is the consequence of there being one owner and several writers. An open
//! window cannot re-list a month every few seconds to find out whether somebody else wrote
//! something, and it cannot go on showing what it read when it last navigated either. So it asks
//! this instead: two numbers, a `stat` per file and no parse, and a listing only when they move.

mod store;

use std::path::PathBuf;
use std::sync::Arc;

use store::EventStore;
use yantrik_ipc_contracts::calendar::{
    method, CreateEventParams, DeleteEventParams, EventsParams, GetEventParams, UpdateEventParams,
    UpsertRemoteEventParams, DEFAULT_REMINDER_MINUTES, MAX_REMINDER_MINUTES,
};
#[cfg(test)]
use yantrik_service_sdk::gate::{self, Authority};
use yantrik_service_sdk::prelude::*;
use yantrik_service_sdk::{
    agent_token, caller, reach, Action, Param, PeerCred, Surface, View,
};
use yantrik_ipc_transport::peer_identity;

/// The id this surface publishes, and the app a grant for one of its actions is bound to.
const APP: &str = "calendar";

fn main() {
    std::fs::create_dir_all(calendar_dir()).ok();
    yantrik_service_sdk::init_tracing("calendar");

    // The reminder timer is not started here, and on purpose: this service is started on demand
    // (`autostart = false`) and stopped freely, so a timer in it only ran while something
    // happened to have opened the calendar. The timer lives in the notifications service, which
    // the shell autostarts, and reads the event files this service writes — see its `reminders`.

    ServiceBuilder::new("calendar")
        .handler(CalendarHandler::default())
        .run();
}

fn calendar_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".local/share/yantrik/calendar")
    } else {
        PathBuf::from("/tmp/yantrik-calendar")
    }
}

struct CalendarHandler {
    /// Shared between the data methods, which read and write it, and the surface, which reports
    /// on it and moves it.
    store: Arc<EventStore>,
    /// `app.describe` and `app.act`, dispatched as an app window's are.
    surface: Surface,
}

impl Default for CalendarHandler {
    fn default() -> Self {
        let store = Arc::new(EventStore::new(calendar_dir()));
        CalendarHandler { store: store.clone(), surface: calendar_surface(store) }
    }
}

/// Read the parameters of one method, naming the method when they do not fit.
fn params_for<T: serde::de::DeserializeOwned>(
    method_name: &str,
    params: serde_json::Value,
) -> Result<T, ServiceError> {
    serde_json::from_value(params).map_err(|e| ServiceError {
        code: -32602,
        message: format!("{method_name}: {e}"),
    })
}

impl ServiceHandler for CalendarHandler {
    fn service_id(&self) -> &str {
        "calendar"
    }

    fn handle(
        &self,
        method_name: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, ServiceError> {
        self.handle_from(method_name, params, None)
    }

    fn handle_from(
        &self,
        method_name: &str,
        params: serde_json::Value,
        peer: Option<PeerCred>,
    ) -> Result<serde_json::Value, ServiceError> {
        // The agent-facing surface: what the calendar holds, and the three moves on it that are
        // this service's to make. The ceiling and the mode are read per call from the files the
        // shell writes, as an app window's dispatch reads them.
        if let Some(answer) = self.surface.answer(method_name, &params, peer) {
            return answer;
        }
        match method_name {
            method::EVENTS => {
                let p: EventsParams = params_for(method_name, params)?;
                let events = self.store.list(&p)?;
                Ok(serde_json::to_value(events).unwrap())
            }
            method::GET_EVENT => {
                let p: GetEventParams = params_for(method_name, params)?;
                let event = self.store.get(&p.id).ok_or_else(|| ServiceError {
                    code: -32602,
                    message: format!("No event here with id {}", p.id),
                })?;
                Ok(serde_json::to_value(event).unwrap())
            }
            method::CREATE_EVENT => {
                let p: CreateEventParams = params_for(method_name, params)?;
                let event = self.store.create(&p)?;
                tracing::info!(id = %event.id, title = %event.title, "Created event");
                Ok(serde_json::to_value(event).unwrap())
            }
            method::UPDATE_EVENT => {
                let p: UpdateEventParams = params_for(method_name, params)?;
                let event = self.store.update(&p)?;
                tracing::info!(id = %event.id, "Updated event");
                Ok(serde_json::to_value(event).unwrap())
            }
            method::DELETE_EVENT => {
                let p: DeleteEventParams = params_for(method_name, params)?;
                self.store.delete(&p.id)?;
                tracing::info!(id = %p.id, "Deleted event");
                Ok(serde_json::json!(null))
            }
            method::UPSERT_REMOTE => {
                let p: UpsertRemoteEventParams = params_for(method_name, params)?;
                let known = self.store.find_by_remote_id(&p.remote_id).is_some();
                let event = self.store.upsert_remote(&p)?;
                tracing::info!(
                    id = %event.id,
                    remote_id = %p.remote_id,
                    known,
                    "Stored event from a remote calendar"
                );
                Ok(serde_json::to_value(event).unwrap())
            }
            method::REVISION => {
                // No parameters, and whatever arrived is ignored rather than refused: this is the
                // cheapest question on the socket and a caller that sends `{}`, `null` or nothing
                // at all should get the same answer. Nothing here writes, touches or creates, so
                // asking repeatedly cannot be what makes the answer change.
                Ok(serde_json::to_value(self.store.revision()).unwrap())
            }
            _ => Err(ServiceError {
                code: -1,
                message: format!("Unknown method: {method_name}"),
            }),
        }
    }
}

impl CalendarHandler {
    /// `app.act` under a pinned authority, as the socket's dispatch would run it. The tests' door.
    #[cfg(test)]
    fn act(
        &self,
        params: &serde_json::Value,
        authority: Authority,
    ) -> Result<serde_json::Value, ServiceError> {
        self.surface.act(params, None, authority)
    }

    /// A handler over a scratch store in its own directory. The tests' calendar.
    #[cfg(test)]
    fn over(dir: PathBuf) -> CalendarHandler {
        let store = Arc::new(EventStore::new(dir));
        CalendarHandler { store: store.clone(), surface: calendar_surface(store) }
    }
}

// ══════════════════════════════════════════════════════════════════════
// Control surface (app.describe / app.act)
// ══════════════════════════════════════════════════════════════════════

/// The surface this socket answers `app.describe` and `app.act` with.
///
/// Three of the methods above — list, create, update — in the one store, under the rules a bare
/// method never had: arguments checked as declared, the grade enforced, the revision guarded.
/// The old comment on this socket called publishing actions "two ways into one store"; they are
/// two doors into one store, which is the arrangement weather and system-monitor already have —
/// the store owns the events, and a door that adds the protocol's checks adds no second copy.
///
/// What stays off it, on purpose: `calendar.delete_event` and the ownership rules beside it
/// (#201) belong to the Calendar app's window, where a delete that cannot be undone is graded
/// for a person to approve; `calendar.revision` and `calendar.upsert_remote` are plumbing an
/// open window and the syncer call directly — plumbing nobody should be asked to *act* on.
fn calendar_surface(store: Arc<EventStore>) -> Surface {
    let describing = store.clone();
    Surface::new(APP)
        .socket_name("calendar")
        .describe(move || describe_view(&describing))
        .action(list_events_action(), {
            let store = store.clone();
            move |args| list_events(&store, args)
        })
        .action(add_event_action(), {
            let store = store.clone();
            move |args| add_event(&store, args)
        })
        .action(update_event_action(), move |args| update_event(&store, args))
}

/// What this surface can be asked to do, as `describe` publishes it.
#[cfg(test)]
fn calendar_actions() -> Vec<Action> {
    vec![list_events_action(), add_event_action(), update_event_action()]
}

/// The grade this surface publishes for `action`, from the same table `describe` hands out, so
/// the grade a caller is shown and the grade that is enforced cannot come apart.
#[cfg(test)]
fn published_grade(action: &str) -> Option<&'static str> {
    calendar_actions().into_iter().find(|a| a.name == action).map(|a| a.permission)
}

/// The events overlapping a range — what the calendar holds.
///
/// `safe`: a read of files under the user's own home, the same listing the window draws. The
/// default is the seven days `describe` reports on, so a caller that says nothing sees what the
/// window opens on.
fn list_events_action() -> Action {
    Action::new("list_events", "List the events overlapping a date range, oldest first")
        .risk("safe")
        .arg(Param::text("start_date").describe("`YYYY-MM-DDTHH:MM:SS` or `YYYY-MM-DD`; defaults to now").optional())
        .arg(Param::text("end_date").describe("The other end of the range; defaults to seven days out").optional())
}

/// A new event, timed or all-day.
///
/// `standard` — `docs/sdk/grades.md`'s ruling on the Calendar app's own `add_event`: an event
/// can be deleted again, and nothing about it disturbs a person until a reminder fires. The
/// arguments are the store's own (`start`, `end`), not the window's date-and-time pair, because
/// this door reaches the same store the method above does.
fn add_event_action() -> Action {
    Action::new("add_event", "Put a new event on the calendar")
        .arg(Param::text("title").describe("What the event is called"))
        .arg(Param::text("start").describe("When it begins: `YYYY-MM-DDTHH:MM:SS` or `YYYY-MM-DD`"))
        .arg(Param::text("end").describe("When it ends; not before `start`"))
        .arg(Param::text("description").describe("Notes kept with the event").optional())
        .arg(Param::text("location").describe("Where it takes place").optional())
        .arg(Param::flag("all_day").describe("A day, not a time — it is not announced").optional())
        // The reminder is a fact about the event, stored with it and announced by the
        // notifications service whether or not anything ever opens the calendar again (#78).
        .arg(
            Param::integer("reminder_minutes")
                .describe("How many minutes before it starts to announce it; ten when not \
                           given. All-day events are not announced")
                .optional(),
        )
}

/// Change a stored event; fields left out keep what they had.
///
/// `standard` per `docs/sdk/grades.md`: an update moves something that still exists. It cannot
/// hand the event to a new owner — the stored creator record stays what it was (#201), so a
/// later delete-without-asking remains the original caller's alone.
fn update_event_action() -> Action {
    Action::new("update_event", "Change a stored event; fields left out keep what they had")
        .arg(Param::text("id").describe("The event's id, as `list_events` reports it"))
        .arg(Param::text("title").describe("A new title").optional())
        .arg(Param::text("start").describe("A new start, same formats as `add_event`").optional())
        .arg(Param::text("end").describe("A new end").optional())
        .arg(Param::text("description").describe("New notes kept with the event").optional())
        .arg(Param::text("location").describe("A new place").optional())
        .arg(Param::flag("all_day").describe("Whether the day is the appointment").optional())
        .arg(
            Param::integer("reminder_minutes")
                .describe("How many minutes before it starts to announce it; unchanged \
                           when not given")
                .optional(),
        )
}

fn list_events(store: &EventStore, args: &serde_json::Value) -> Result<serde_json::Value, String> {
    let now = chrono::Local::now().naive_local();
    let params = EventsParams {
        start_date: text_arg(args, "start_date").unwrap_or_else(|| iso(&now)),
        end_date: text_arg(args, "end_date").unwrap_or_else(|| iso(&(now + chrono::Duration::days(7)))),
    };
    store
        .list(&params)
        .map(|events| serde_json::to_value(events).unwrap())
        .map_err(|e| e.message)
}

fn add_event(store: &EventStore, args: &serde_json::Value) -> Result<serde_json::Value, String> {
    let params = CreateEventParams {
        // The dispatch has already refused anything missing or of another type; `check_arguments`
        // is why the reads below can unwrap_or_default without second-guessing the caller.
        title: text_arg(args, "title").unwrap_or_default(),
        start: text_arg(args, "start").unwrap_or_default(),
        end: text_arg(args, "end").unwrap_or_default(),
        description: text_arg(args, "description").unwrap_or_default(),
        location: text_arg(args, "location"),
        color: String::new(),
        is_all_day: args.get("all_day").and_then(|v| v.as_bool()).unwrap_or(false),
        attendees: Vec::new(),
        // Who is asking, as the machine establishes it — never from these arguments, which the
        // caller writes; see `requester` (#201).
        creator: requester(),
        reminder_minutes: reminder_arg(args)?,
    };
    let event = store.create(&params).map_err(|e| e.message)?;
    tracing::info!(id = %event.id, title = %event.title, "Created event");
    serde_json::to_value(event).map_err(|e| e.to_string())
}

fn update_event(store: &EventStore, args: &serde_json::Value) -> Result<serde_json::Value, String> {
    let params = UpdateEventParams {
        id: text_arg(args, "id").unwrap_or_default(),
        title: text_arg(args, "title"),
        start: text_arg(args, "start"),
        end: text_arg(args, "end"),
        description: text_arg(args, "description"),
        location: text_arg(args, "location"),
        is_all_day: args.get("all_day").and_then(|v| v.as_bool()),
        attendees: None,
        remote_id: None,
        reminder_minutes: reminder_arg(args)?,
    };
    let event = store.update(&params).map_err(|e| e.message)?;
    tracing::info!(id = %event.id, "Updated event");
    serde_json::to_value(event).map_err(|e| e.to_string())
}

/// A declared text argument, present or not. The dispatch guarantees the type.
fn text_arg(args: &serde_json::Value, name: &str) -> Option<String> {
    args.get(name).and_then(|v| v.as_str()).map(str::to_string)
}

/// The `reminder_minutes` argument as this door reads it — the same reading, and the same
/// sentences, as the Calendar window's (#78): absent is "did not say", and anything present
/// must be a number of minutes inside the contract's cap. Checked here rather than left to the
/// store, whose refusal names only the cap: the error a caller can act on names the whole
/// range it should have stayed inside.
fn reminder_arg(args: &serde_json::Value) -> Result<Option<u32>, String> {
    match args.get("reminder_minutes") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(v) => {
            let m = v.as_i64().ok_or("`reminder_minutes` must be a number of minutes")?;
            if !(0..=i64::from(MAX_REMINDER_MINUTES)).contains(&m) {
                return Err(format!(
                    "`reminder_minutes` must be between 0 and {MAX_REMINDER_MINUTES}, and was {m}"
                ));
            }
            Ok(Some(m as u32))
        }
    }
}

fn iso(dt: &chrono::NaiveDateTime) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S").to_string()
}

/// Who the machine establishes is asking, in the one spelling every surface agrees on (#201):
/// `agent <mind>:<conversation>` where the call carried an agent token whose reach the shell
/// knows, else the program the kernel's peer credentials lead to, else nothing.
///
/// The Calendar app's window records creators the same way, from the same two sources, so an
/// event made through either door carries the record `delete_own_event` reads back. A token the
/// reach file does not know is no agent — that falls through to the program rather than
/// refusing twice — and nothing is invented when the machine cannot tell: an event with no
/// creator is one nobody may delete unasked.
fn requester() -> Option<String> {
    if let Some(token) = agent_token() {
        if let Ok(Some(reach)) = reach::reach_of(&token) {
            return Some(format!("agent {}", reach.agent));
        }
    }
    let who = caller()?;
    let name = peer_identity::resolve(Some(who.pid)).name();
    if name.is_empty() { None } else { Some(name) }
}

/// What this calendar holds, and how an event gets announced.
///
/// The reminder note is the point of this view: the timer does not run here — this service
/// is on demand, so a timer in it would only tick while something had opened the calendar.
/// It runs in the notifications service, which is always up, and a caller reading this is
/// told where reminders live and what one is worth rather than left to assume the wrong
/// thing.
fn describe_view(store: &EventStore) -> View {
    let revision = store.revision();
    let now = chrono::Local::now().naive_local();
    let upcoming = store
        .list(&EventsParams {
            start_date: iso(&now),
            end_date: iso(&(now + chrono::Duration::days(7))),
        })
        .unwrap_or_default();

    let next = upcoming.first();
    let summary = match next {
        Some(e) => format!(
            "Calendar — {} events stored, next: {} at {}",
            revision.events, e.title, e.start
        ),
        None => format!(
            "Calendar — {} events stored, nothing in the next seven days",
            revision.events
        ),
    };

    View::new(summary)
        .with("events", revision.events as i64)
        .with("store", store.dir().to_string_lossy().to_string())
        .with(
            "upcoming",
            serde_json::Value::Array(
                upcoming
                    .iter()
                    .take(10)
                    .map(|e| {
                        serde_json::json!({
                            "id": e.id,
                            "title": e.title,
                            "start": e.start,
                            "end": e.end,
                            "all_day": e.is_all_day,
                        })
                    })
                    .collect(),
            ),
        )
        .with(
            "reminders",
            serde_json::json!({
                "default_minutes": DEFAULT_REMINDER_MINUTES,
                "hosted_by": "notifications",
                "note": "Every timed event is announced through the notifications \
                         service, once, its own `reminder_minutes` before it starts — ten \
                         when the event does not say. The timer runs in the notifications \
                         service, which the shell keeps up from boot, so a reminder set for \
                         tomorrow fires whether or not anything ever opens the calendar. \
                         All-day events are not announced: there is no time of day to \
                         announce them at.",
            }),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// A handler over a calendar of its own: a fresh directory under /tmp, never `$HOME`, so
    /// the tests neither touch the developer's events nor depend on what is in them.
    fn scratch() -> (CalendarHandler, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "yos-calendar-surface-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        (CalendarHandler::over(dir.clone()), dir)
    }

    /// A machine at `ceiling`, in `mode`, with no grant spent — the authority pinned per case
    /// rather than inherited from whatever files the machine running the tests has.
    fn at(ceiling: &str, mode: &str) -> Authority {
        Authority { ceiling: ceiling.into(), mode: gate::Mode::named(mode), granted: false }
    }

    /// `app.act` for `action` with `args`, and a grant id beside it when the test says one was
    /// spent for the call.
    fn act_params(action: &str, args: serde_json::Value, grant: Option<&str>) -> serde_json::Value {
        let mut params = serde_json::json!({ "action": action, "args": args });
        if let Some(grant) = grant {
            params["grant"] = grant.into();
        }
        params
    }

    /// A point in the store's own date format, `days` from now. Days, not minutes: nothing in
    /// these tests may depend on how fast the machine is.
    fn days_from_now(days: i64) -> String {
        iso(&(chrono::Local::now().naive_local() + chrono::Duration::days(days)))
    }

    /// Grants the stand-in shell spent. `ok-*` holds, anything else is refused in its words.
    static SPENT: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    /// What each grant was spent against, as the shell would have been handed it.
    static SPENT_AGAINST: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());

    fn spend_through_a_stand_in_shell() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            gate::spend_grants_with(|id, _app, _action, args| {
                if !id.starts_with("ok-") {
                    return Err(format!("no approval request `{id}`."));
                }
                SPENT.lock().unwrap_or_else(|e| e.into_inner()).push(id.to_string());
                SPENT_AGAINST.lock().unwrap_or_else(|e| e.into_inner()).push((id.to_string(), args.to_string()));
                Ok(())
            });
        });
    }

    /// Add an event a few days out, the way an ask-mode caller would, and hand back the whole
    /// envelope: its `result` is the event as stored, with the id the store gave it.
    fn an_event(handler: &CalendarHandler, title: &str) -> serde_json::Value {
        handler
            .act(
                &act_params(
                    "add_event",
                    serde_json::json!({ "title": title, "start": days_from_now(2), "end": days_from_now(3) }),
                    None,
                ),
                at("sensitive", "ask"),
            )
            .expect("add_event is standard: it runs unasked in ask mode")
    }

    /// `add_event` and `update_event` are `standard` and `list_events` is `safe` — the grades
    /// `docs/sdk/grades.md` rules for the Calendar app's own actions, on the same store — and
    /// `describe` shows exactly what `act` enforces.
    #[test]
    fn the_published_grades_are_what_describe_shows() {
        assert_eq!(published_grade("list_events"), Some("safe"));
        assert_eq!(published_grade("add_event"), Some("standard"));
        assert_eq!(published_grade("update_event"), Some("standard"));

        let (handler, dir) = scratch();
        let described = handler.handle("app.describe", serde_json::json!({})).expect("describe");
        let actions = described["actions"].as_array().expect("actions");
        assert_eq!(actions.len(), calendar_actions().len());
        for a in actions {
            let name = a["name"].as_str().unwrap();
            assert_eq!(a["permission"].as_str(), published_grade(name), "{name}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The whole point of #179, end to end inside the store: an event added through `app.act`
    /// is on the calendar — listed by `list_events`, counted by the next `describe` — and the
    /// act answers with the state after it, so nobody has to ask twice. An in-process act has
    /// no caller to record, and inventing one is not on (#201).
    #[test]
    fn an_event_added_through_the_surface_is_on_the_calendar() {
        let (handler, dir) = scratch();
        let answer = an_event(&handler, "Standup");
        assert_eq!(answer["accepted"], true);
        assert_eq!(answer["settled"], true);
        assert_eq!(answer["result"]["title"], "Standup");
        assert!(
            answer["result"].get("creator").map(|c| c.is_null()).unwrap_or(true),
            "no caller, no creator record: {}",
            answer["result"]
        );
        assert!(answer["summary"].as_str().unwrap().contains("1 events stored"), "{answer}");

        let listed = handler
            .act(&act_params("list_events", serde_json::json!({}), None), at("sensitive", "ask"))
            .expect("a safe read needs nothing");
        assert_eq!(listed["result"].as_array().unwrap().len(), 1);
        assert_eq!(listed["result"][0]["title"], "Standup");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The reminder lead #78 added travels through this door as through the window's: a stated
    /// lead is stored on the event, an unstated one keeps the contract's default, and a lead
    /// outside the cap is refused at the door in a sentence that names the range — before
    /// anything is stored.
    #[test]
    fn a_reminder_lead_travels_with_the_event() {
        let (handler, dir) = scratch();
        let said = handler
            .act(
                &act_params(
                    "add_event",
                    serde_json::json!({
                        "title": "Standup",
                        "start": days_from_now(2),
                        "end": days_from_now(3),
                        "reminder_minutes": 30,
                    }),
                    None,
                ),
                at("sensitive", "ask"),
            )
            .expect("a stated lead is stored");
        assert_eq!(said["result"]["reminder_minutes"], 30);

        let unstated = an_event(&handler, "Unsaid");
        assert_eq!(
            unstated["result"]["reminder_minutes"],
            DEFAULT_REMINDER_MINUTES,
            "an unstated lead keeps the default the contract promises"
        );

        for bad in [-5, i64::from(MAX_REMINDER_MINUTES) + 1] {
            let err = handler
                .act(
                    &act_params(
                        "add_event",
                        serde_json::json!({
                            "title": "Nope",
                            "start": days_from_now(2),
                            "end": days_from_now(3),
                            "reminder_minutes": bad,
                        }),
                        None,
                    ),
                    at("sensitive", "ask"),
                )
                .unwrap_err();
            assert_eq!(
                err.message,
                format!("`reminder_minutes` must be between 0 and {MAX_REMINDER_MINUTES}, and was {bad}")
            );
        }
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2, "a refused lead stored nothing");

        // An update moves the lead, and one that does not say keeps what is stored.
        let id = said["result"]["id"].clone();
        let moved = handler
            .act(
                &act_params("update_event", serde_json::json!({ "id": id, "reminder_minutes": 5 }), None),
                at("sensitive", "ask"),
            )
            .expect("a standard edit");
        assert_eq!(moved["result"]["reminder_minutes"], 5);
        let kept = handler
            .act(
                &act_params("update_event", serde_json::json!({ "id": id, "title": "Renamed" }), None),
                at("sensitive", "ask"),
            )
            .expect("a standard edit");
        assert_eq!(kept["result"]["reminder_minutes"], 5, "an unstated lead keeps what it had");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The default listing is the week `describe` reports on: an event two days out is in it
    /// whether or not the caller says anything, and a stated range that misses it comes back
    /// empty — the store's overlap rule, applied as declared.
    #[test]
    fn list_events_answers_a_range_and_otherwise_shows_the_week() {
        let (handler, dir) = scratch();
        an_event(&handler, "Standup");
        let week = handler
            .act(&act_params("list_events", serde_json::json!({}), None), at("safe", "plan"))
            .unwrap();
        assert_eq!(week["result"].as_array().unwrap().len(), 1);
        let far = handler
            .act(
                &act_params(
                    "list_events",
                    serde_json::json!({ "start_date": days_from_now(10), "end_date": days_from_now(20) }),
                    None,
                ),
                at("safe", "plan"),
            )
            .unwrap();
        assert!(far["result"].as_array().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// `standard` needs no grant in any mode, plan included. Each mode gets its own calendar,
    /// and the proof is the same in all four: the event lands.
    #[test]
    fn add_event_reaches_its_handler_in_every_mode_without_a_grant() {
        for mode in ["plan", "ask", "auto", "bypass"] {
            let (handler, dir) = scratch();
            let answer = handler
                .act(
                    &act_params(
                        "add_event",
                        serde_json::json!({ "title": "Standup", "start": days_from_now(2), "end": days_from_now(3) }),
                        None,
                    ),
                    at("sensitive", mode),
                )
                .unwrap_or_else(|e| panic!("{mode}: {}", e.message));
            assert_eq!(answer["accepted"], true, "{mode}");
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// The ceiling binds this door as it binds every app's: a machine set to `safe` refuses
    /// `add_event` on the grade alone, grant or none — before anything is written, and a grant
    /// it refused was never offered to the shell (#154).
    #[test]
    fn a_ceiling_of_safe_refuses_add_event_whatever_the_grant() {
        spend_through_a_stand_in_shell();
        for grant in [None, Some("ok-179-calendar")] {
            let (handler, dir) = scratch();
            let err = handler
                .act(
                    &act_params(
                        "add_event",
                        serde_json::json!({ "title": "Standup", "start": days_from_now(2), "end": days_from_now(3) }),
                        grant,
                    ),
                    at("safe", "bypass"),
                )
                .unwrap_err();
            assert!(
                err.message.starts_with("CEILING: calendar.add_event is graded `standard`"),
                "grant={grant:?}: {}",
                err.message
            );
            assert_eq!(err.code, -32602);
            assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "grant={grant:?}: an event was stored anyway");
            let _ = std::fs::remove_dir_all(dir);
        }
        let spent = SPENT.lock().unwrap_or_else(|e| e.into_inner());
        assert!(!spent.iter().any(|id| id == "ok-179-calendar"), "spent above the ceiling: {spent:?}");
    }

    /// As on a window: a grant that rides on a call is spent past the ceiling, and one the
    /// shell refuses ends the call in the shell's words — before anything is stored.
    #[test]
    fn a_grant_that_does_not_hold_ends_the_call() {
        spend_through_a_stand_in_shell();
        let (handler, dir) = scratch();
        let err = handler
            .act(
                &act_params(
                    "add_event",
                    serde_json::json!({ "title": "Standup", "start": days_from_now(2), "end": days_from_now(3) }),
                    Some("made-up"),
                ),
                at("sensitive", "ask"),
            )
            .unwrap_err();
        assert!(
            err.message.starts_with("GRANT: `made-up` does not authorise calendar.add_event"),
            "{}",
            err.message
        );
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// An agent token travels beside `args`, never among them: one a caller put among them is
    /// taken out before the grant is spent, so the shell is handed the arguments alone. The
    /// stale revision is here to stop the call at the guard, after the spend, without touching
    /// the store.
    #[test]
    fn an_agent_token_among_the_arguments_is_not_what_a_grant_is_bound_to() {
        spend_through_a_stand_in_shell();
        let (handler, dir) = scratch();
        let params = serde_json::json!({
            "action": "list_events",
            "args": { "agent_token": "smuggled" },
            "agent_token": "tok-7f3a",
            "grant": "ok-179-token",
            "expect_revision": "0000000000000000",
        });
        let err = handler.act(&params, at("sensitive", "ask")).unwrap_err();
        assert!(err.message.starts_with("STALE: this app is at revision "), "{}", err.message);
        let against = SPENT_AGAINST.lock().unwrap_or_else(|e| e.into_inner());
        let (_, args) = against.iter().find(|(id, _)| id == "ok-179-token").expect("the grant was spent");
        assert_eq!(args, "{}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// `update_event` edits the stored event and nothing else: fields left out keep what they
    /// had, and the creator record survives (#201) — an update cannot hand somebody else's
    /// event to a new owner, which is what a later delete-unasked would key off. An id that is
    /// not here is refused in the store's words.
    #[test]
    fn update_event_edits_a_stored_event_and_keeps_its_owner() {
        let (handler, dir) = scratch();
        let added = an_event(&handler, "Standup");
        let (id, started) = (added["result"]["id"].clone(), added["result"]["start"].clone());
        let updated = handler
            .act(
                &act_params("update_event", serde_json::json!({ "id": id, "title": "Renamed" }), None),
                at("sensitive", "ask"),
            )
            .expect("update_event is standard");
        assert_eq!(updated["result"]["title"], "Renamed");
        assert_eq!(updated["result"]["start"], started, "a field left out kept what it had");

        // An event stored with an owner keeps it through a surface edit.
        handler
            .store
            .create(&CreateEventParams {
                title: "Somebody else's".into(),
                start: days_from_now(4),
                end: days_from_now(5),
                description: String::new(),
                location: None,
                color: String::new(),
                is_all_day: false,
                attendees: Vec::new(),
                creator: Some("agent pi:owned".into()),
                reminder_minutes: None,
            })
            .expect("seeded");
        let listed = handler
            .act(&act_params("list_events", serde_json::json!({}), None), at("sensitive", "ask"))
            .unwrap();
        let owned = listed["result"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["creator"] == serde_json::json!("agent pi:owned"))
            .expect("the seeded creator record");
        handler
            .act(
                &act_params("update_event", serde_json::json!({ "id": owned["id"], "location": "Room 2" }), None),
                at("sensitive", "ask"),
            )
            .expect("a standard edit");
        let listed = handler
            .act(&act_params("list_events", serde_json::json!({}), None), at("sensitive", "ask"))
            .unwrap();
        assert_eq!(
            listed["result"].as_array().unwrap().iter().find(|e| e["id"] == owned["id"]).unwrap()["creator"],
            "agent pi:owned",
            "an update re-assigned the event"
        );

        let err = handler
            .act(
                &act_params("update_event", serde_json::json!({ "id": "no-such-id" }), None),
                at("sensitive", "ask"),
            )
            .unwrap_err();
        assert_eq!(err.code, -32602);
        assert_eq!(err.message, "No event here with id no-such-id");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// An id is a name inside the store's own folder, never a path. `update_event` reads with
    /// the argument's id and writes to a path built from the id stored inside the file it read,
    /// so a crafted id reached any `.json` the person owns — the bug notes-service had and
    /// fixed (#161, #320), back on the headless door #179 opened. Both the argument's id and
    /// the id read back from a file are refused before the filesystem is touched, and an id
    /// the service itself handed out still round-trips.
    #[test]
    fn a_crafted_id_cannot_reach_past_the_calendar_dir() {
        let (handler, dir) = scratch();
        // A sentinel outside the store's folder, planted as an event whose own id points back
        // out: pre-fix, an update read it and then wrote over it through that stored id. Named
        // after this test's own scratch dir, which no other test uses.
        let outside =
            dir.with_file_name(format!("{}-outside.json", dir.file_name().unwrap().to_string_lossy()));
        let traversal = format!("../{}", outside.file_stem().unwrap().to_string_lossy());
        let planted = serde_json::json!({
            "id": traversal,
            "title": "Sentinel",
            "description": "",
            "start": days_from_now(2),
            "end": days_from_now(3),
            "is_all_day": false,
            "location": null,
            "attendees": [],
            "recurrence": null,
            "calendar_id": "default",
            "remote_id": null,
        });
        std::fs::write(&outside, serde_json::to_string_pretty(&planted).unwrap()).unwrap();
        let before = std::fs::read_to_string(&outside).unwrap();

        for id in [traversal.as_str(), "/tmp/x"] {
            let err = handler
                .act(
                    &act_params("update_event", serde_json::json!({ "id": id, "title": "Taken" }), None),
                    at("sensitive", "ask"),
                )
                .unwrap_err();
            assert_eq!(err.code, -32602);
            assert_eq!(
                err.message,
                format!("`{id}` is not an event id; ids are the names list_events reports, never paths"),
                "a path is refused as the path it is, not reported missing"
            );
        }
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), before, "the file outside survived");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "a refused update stored nothing");

        // The doors the surface does not publish hold the same rule at the store itself: the
        // raw method's read answers "not here" without opening anything, and its delete
        // refuses rather than removing what is outside.
        assert!(handler.store.get(&traversal).is_none());
        let err = handler.store.delete(&traversal).unwrap_err();
        assert_eq!(err.code, -32602);
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), before, "a refused delete removed nothing");

        // And an id the service handed out still round-trips add → update → list.
        let added = an_event(&handler, "Real");
        let id = added["result"]["id"].clone();
        handler
            .act(
                &act_params("update_event", serde_json::json!({ "id": id, "title": "Renamed" }), None),
                at("sensitive", "ask"),
            )
            .expect("a service-issued id is a valid one");
        let listed = handler
            .act(&act_params("list_events", serde_json::json!({}), None), at("sensitive", "ask"))
            .expect("a safe read");
        let events = listed["result"].as_array().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["title"], "Renamed");

        let _ = std::fs::remove_file(&outside);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Every action `describe` offers reaches a handler past the gate, and an action it does
    /// not offer — including the delete this surface deliberately does not publish — is
    /// answered as that before any grant is looked at: -32602, in the dispatch's words.
    #[test]
    fn every_published_action_has_a_handler() {
        let (handler, dir) = scratch();
        for spec in calendar_actions() {
            let outcome = handler.act(
                &act_params(&spec.name, serde_json::json!({}), None),
                at("dangerous", "bypass"),
            );
            if let Err(err) = outcome {
                // `list_events` answers; the two that need arguments refuse for them. Neither
                // answer may be the gate's or the dispatch's "no such action".
                for prefix in ["CEILING:", "GRANT:", "STALE:", "unknown action"] {
                    assert!(!err.message.starts_with(prefix), "{}: {}", spec.name, err.message);
                }
            }
        }
        let err = handler
            .act(
                &act_params("delete_event", serde_json::json!({ "id": "x" }), Some("made-up")),
                at("dangerous", "ask"),
            )
            .unwrap_err();
        assert_eq!(err.code, -32602);
        assert_eq!(err.message, "unknown action `delete_event`; this app offers: list_events, add_event, update_event");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Arguments are checked on this door as on every app's — and a caller cannot declare its
    /// own creator: `creator` is not an argument `add_event` takes, so the #201 record is only
    /// ever what the machine establishes.
    #[test]
    fn the_arguments_are_checked_as_an_apps_are() {
        let (handler, dir) = scratch();
        // A number converts to a string without loss — `title: 5` becomes "5", by the SDK's own
        // rule for models. A boolean cannot be a title: the type refusal names it.
        let err = handler
            .act(
                &act_params("add_event", serde_json::json!({ "title": true, "start": days_from_now(2), "end": days_from_now(3) }), None),
                at("sensitive", "ask"),
            )
            .unwrap_err();
        assert_eq!(err.code, -32602);
        assert_eq!(err.message, "`add_event` argument `title` must be a string, and a boolean arrived");

        // A complete, otherwise-valid add that also tries to name its own creator: refused for
        // the undeclared name — the record is only ever what the machine establishes (#201).
        let err = handler
            .act(
                &act_params(
                    "add_event",
                    serde_json::json!({
                        "title": "x",
                        "start": days_from_now(2),
                        "end": days_from_now(3),
                        "creator": "me",
                    }),
                    None,
                ),
                at("sensitive", "ask"),
            )
            .unwrap_err();
        assert_eq!(
            err.message,
            "`add_event` has no argument `creator`; it takes: title, start, end, description, \
             location, all_day, reminder_minutes"
        );

        let err = handler
            .act(&act_params("add_event", serde_json::json!({}), None), at("sensitive", "ask"))
            .unwrap_err();
        assert_eq!(err.message, "`add_event` needs argument `title`");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Stale is refused before the store is written — and the store keeps nothing: an act
    /// decided on a state this calendar has left does not happen.
    #[test]
    fn an_act_decided_on_an_old_revision_stores_nothing() {
        let (handler, dir) = scratch();
        let params = serde_json::json!({
            "action": "add_event",
            "args": { "title": "Standup", "start": days_from_now(2), "end": days_from_now(3) },
            "expect_revision": "0000000000000000",
        });
        let err = handler.act(&params, at("sensitive", "ask")).unwrap_err();
        assert!(err.message.starts_with("STALE: this app is at revision "), "{}", err.message);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The surface declares nothing the dispatch cannot check.
    #[test]
    fn the_surface_is_declared_soundly() {
        let (handler, dir) = scratch();
        assert!(handler.surface.registry().problems().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
