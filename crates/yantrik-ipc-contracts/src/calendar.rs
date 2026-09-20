//! Calendar service contract — event CRUD, sync, scheduling.

use serde::{Deserialize, Serialize};
use crate::email::ServiceError;

/// The names of the calendar service's JSON-RPC methods.
///
/// Here rather than spelled out at each call site, because the two ends of this wire drifted
/// apart while both looked correct on their own page.
pub mod method {
    pub const EVENTS: &str = "calendar.events";
    pub const GET_EVENT: &str = "calendar.get_event";
    pub const CREATE_EVENT: &str = "calendar.create_event";
    pub const UPDATE_EVENT: &str = "calendar.update_event";
    pub const DELETE_EVENT: &str = "calendar.delete_event";
    /// Store a remote calendar's event under the id it already has out there.
    pub const UPSERT_REMOTE: &str = "calendar.upsert_remote";
}

/// Parameters for [`method::EVENTS`].
///
/// The request types below exist because this contract used to carry only the data types, and
/// each end wrote its own parameter names by hand. They disagreed: the app asked for `start` and
/// `end` where the service required `start_date` and `end_date`, so every listing failed and the
/// calendar could not show an event it had just stored; delete sent `event_id` where the service
/// read `id`. Both sides now build and parse the same struct, so a rename cannot land on one end
/// alone — it stops compiling on the other.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventsParams {
    /// Inclusive lower bound, `YYYY-MM-DD` or `YYYY-MM-DDTHH:MM:SS`.
    pub start_date: String,
    /// Inclusive upper bound, same formats. An event overlapping the range is included.
    pub end_date: String,
}

/// Parameters for [`method::GET_EVENT`].
///
/// One event by the id the store gave it. A caller that is about to change or remove an event
/// often has to know something about it first — whether it came from a remote calendar, and under
/// which id out there — and reading the whole month to find out is a worse question.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetEventParams {
    pub id: String,
}

/// Parameters for [`method::CREATE_EVENT`].
///
/// `is_all_day` and `attendees` arrived with the companion's own calendar tools, which accepted
/// both long before this contract carried them. The stored [`CalendarEvent`] has always had a
/// home for each; the parameters simply did not reach it, so an all-day event asked for by the
/// mind was stored as a timed one and its attendees were dropped without a word. Both default, so
/// a caller that does not send them — the Calendar app — is unaffected.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateEventParams {
    pub title: String,
    /// ISO 8601, `YYYY-MM-DDTHH:MM:SS`.
    pub start: String,
    /// ISO 8601. An event that ends before it starts is refused.
    pub end: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub is_all_day: bool,
    #[serde(default)]
    pub attendees: Vec<Attendee>,
}

/// Parameters for [`method::UPDATE_EVENT`]. Every field but `id` is optional; those left out
/// keep the value the stored event already has.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateEventParams {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_all_day: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attendees: Option<Vec<Attendee>>,
    /// The id this event has on the calendar it came from, once it has one.
    ///
    /// Set after pushing a locally made event out to Google, so the next sync recognises it as
    /// the event it already has rather than storing a second copy beside it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_id: Option<String>,
}

/// Parameters for [`method::UPSERT_REMOTE`] — one event from a remote calendar, keyed by the id
/// it has out there.
///
/// Sync had no idempotent way in. It wrote Google's events into a private SQLite table, which the
/// Calendar app could not see, and the only shape the service offered was create — so syncing the
/// same week twice would have stored every event twice. Keying on `remote_id` makes a re-sync a
/// no-op on unchanged events and an edit on changed ones, and puts them where the app looks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpsertRemoteEventParams {
    /// The event's id on the remote calendar. Never the store's own id.
    pub remote_id: String,
    pub title: String,
    pub start: String,
    pub end: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub is_all_day: bool,
    #[serde(default)]
    pub attendees: Vec<Attendee>,
}

/// Parameters for [`method::DELETE_EVENT`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteEventParams {
    pub id: String,
}

/// A calendar event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarEvent {
    pub id: String,
    pub title: String,
    pub description: String,
    pub start: String,       // ISO 8601
    pub end: String,         // ISO 8601
    pub is_all_day: bool,
    pub location: Option<String>,
    pub attendees: Vec<Attendee>,
    pub recurrence: Option<String>,
    pub calendar_id: String,
    pub remote_id: Option<String>,
}

/// An event attendee.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attendee {
    pub name: String,
    pub email: String,
    pub status: AttendeeStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttendeeStatus {
    Pending,
    Accepted,
    Declined,
    Tentative,
}

/// A day cell for month view rendering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DayCell {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub event_count: i32,
    pub is_today: bool,
    pub is_current_month: bool,
}

/// Calendar service operations.
pub trait CalendarService: Send + Sync {
    fn list_events(&self, calendar_id: &str, start: &str, end: &str) -> Result<Vec<CalendarEvent>, ServiceError>;
    fn get_event(&self, calendar_id: &str, event_id: &str) -> Result<CalendarEvent, ServiceError>;
    fn create_event(&self, calendar_id: &str, event: CalendarEvent) -> Result<CalendarEvent, ServiceError>;
    fn update_event(&self, calendar_id: &str, event: CalendarEvent) -> Result<CalendarEvent, ServiceError>;
    fn delete_event(&self, calendar_id: &str, event_id: &str) -> Result<(), ServiceError>;
    fn month_cells(&self, year: i32, month: u32) -> Result<Vec<DayCell>, ServiceError>;
    fn sync(&self, calendar_id: &str) -> Result<(), ServiceError>;
}
