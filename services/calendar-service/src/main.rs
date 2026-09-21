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

mod reminders;
mod store;

use std::path::PathBuf;

use store::EventStore;
use yantrik_ipc_contracts::calendar::{
    method, CreateEventParams, DeleteEventParams, EventsParams, GetEventParams, UpdateEventParams,
    UpsertRemoteEventParams,
};
use yantrik_ipc_contracts::control_surface::{describe_json, View};
use yantrik_service_sdk::prelude::*;

fn main() {
    std::fs::create_dir_all(calendar_dir()).ok();
    yantrik_service_sdk::init_tracing("calendar");

    // Before the server blocks. The events are here, so the reminder for one is here too — see
    // `reminders`, which also says what it cannot promise.
    reminders::spawn(calendar_dir());

    ServiceBuilder::new("calendar")
        .handler(CalendarHandler { store: EventStore::new(calendar_dir()) })
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
    store: EventStore,
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
            // The one thing a caller could not ask this service was what it is doing. There is
            // no `app.act` beside it: every action this service has is one of the methods above,
            // and publishing a second name for each would be two ways into one store.
            "app.describe" => Ok(describe_json("calendar", &self.describe_view(), &[])),
            _ => Err(ServiceError {
                code: -1,
                message: format!("Unknown method: {method_name}"),
            }),
        }
    }
}

impl CalendarHandler {
    /// What this calendar holds, and the one thing about it that is easy to assume wrongly.
    ///
    /// The reminder note is the point of this view. `autostart = false` in this service's
    /// manifest means nothing is ticking on a machine where nothing has opened the calendar
    /// since boot — so an event can pass without being announced, and a caller reading this is
    /// told that rather than left to find out by missing a meeting.
    fn describe_view(&self) -> View {
        let revision = self.store.revision();
        let now = chrono::Local::now().naive_local();
        let upcoming = self
            .store
            .list(&EventsParams {
                start_date: now.format("%Y-%m-%dT%H:%M:%S").to_string(),
                end_date: (now + chrono::Duration::days(7))
                    .format("%Y-%m-%dT%H:%M:%S")
                    .to_string(),
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
            .with("store", calendar_dir().to_string_lossy().to_string())
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
                    "lead_minutes": reminders::LEAD.num_minutes(),
                    "running": true,
                    "note": "An event is announced through the notifications service ten \
                             minutes before it starts, once, and only while this service is \
                             running. This service is started on demand, so on a machine where \
                             nothing has opened the calendar since boot nothing is ticking and \
                             an event can pass unannounced. All-day events are not announced: \
                             there is no time of day to announce them at.",
                }),
            )
    }
}
