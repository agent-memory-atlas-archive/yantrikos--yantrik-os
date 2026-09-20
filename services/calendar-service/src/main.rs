//! Calendar service — event CRUD via filesystem-backed JSON storage.
//!
//! Events stored as individual `.json` files in `~/.local/share/yantrik/calendar/`.
//!
//! Methods:
//!   calendar.events       { start_date, end_date }                              → Vec<CalendarEvent>
//!   calendar.create_event { title, start, end, description?, location?, color? } → CalendarEvent
//!   calendar.update_event { id, title?, start?, end?, description?, location? }  → CalendarEvent
//!   calendar.delete_event { id }                                                 → ()
//!
//! Those parameter names are not written out here any more. They come from
//! `yantrik_ipc_contracts::calendar`, which the calendar app builds its requests from, because
//! this list and the app's calls used to disagree while each looked right in its own file.

mod store;

use std::path::PathBuf;

use store::EventStore;
use yantrik_ipc_contracts::calendar::{
    method, CreateEventParams, DeleteEventParams, EventsParams, UpdateEventParams,
};
use yantrik_service_sdk::prelude::*;

fn main() {
    std::fs::create_dir_all(calendar_dir()).ok();

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
            _ => Err(ServiceError {
                code: -1,
                message: format!("Unknown method: {method_name}"),
            }),
        }
    }
}
