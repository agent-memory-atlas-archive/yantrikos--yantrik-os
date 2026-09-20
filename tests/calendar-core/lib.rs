//! The calendar's storage rules, tested without a socket or a desktop.
//!
//! The bugs these cover were all invisible from either side on its own: the app asked for
//! `start`/`end` where the service required `start_date`/`end_date`, and sent `event_id` where
//! it read `id`, so listing and deleting failed on every machine while both files looked right.
//! The payload tests below are the ones that would have caught it — they send what the app
//! sends and read it the way the service reads it.

#[path = "../../services/calendar-service/src/store.rs"]
pub mod store;

/// The week and day views' arithmetic, from the app side of the same wire.
///
/// It is here rather than beside the app because it is the half of the calendar that has no
/// Slint in it: given the events, the selected date and the view, it says which column an event
/// is in, how tall it is drawn and what the seven column headers read. Week and Day were an
/// empty drawing until today -- five `in` properties nothing anywhere ever set -- so every case
/// below is a case that had never been exercised at all.
#[path = "../../apps/calendar/src/views.rs"]
pub mod views;

#[cfg(test)]
mod tests {
    use super::store::EventStore;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use yantrik_ipc_contracts::calendar::{
        Attendee, AttendeeStatus, CreateEventParams, DeleteEventParams, EventsParams,
        UpdateEventParams, UpsertRemoteEventParams,
    };

    static ID: AtomicUsize = AtomicUsize::new(0);

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "calendar-test-{}-{}",
                std::process::id(),
                ID.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&p);
            Self(p)
        }
        fn store(&self) -> EventStore {
            EventStore::new(self.0.clone())
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn create(title: &str, start: &str, end: &str) -> CreateEventParams {
        CreateEventParams {
            title: title.into(),
            start: start.into(),
            end: end.into(),
            description: String::new(),
            location: None,
            color: String::new(),
            is_all_day: false,
            attendees: Vec::new(),
        }
    }

    fn month(year: i32, m: u32, last: u32) -> EventsParams {
        EventsParams {
            start_date: format!("{year:04}-{m:02}-01T00:00:00"),
            end_date: format!("{year:04}-{m:02}-{last:02}T23:59:59"),
        }
    }

    // ── The wire, as both ends speak it ──────────────────────────────

    #[test]
    fn the_listing_request_the_app_sends_is_the_one_the_service_reads() {
        // What the app puts on the wire, serialized exactly as it sends it.
        let sent = serde_json::to_value(month(2026, 9, 30)).unwrap();
        // What the service does with it. Before the contract types this failed, because the
        // app wrote `start`/`end` and the service required `start_date`/`end_date` — and the
        // failure surfaced as an empty month rather than an error.
        let read: EventsParams = serde_json::from_value(sent.clone()).unwrap();
        assert_eq!(read.start_date, "2026-09-01T00:00:00");
        assert_eq!(read.end_date, "2026-09-30T23:59:59");
        assert!(sent.get("start").is_none(), "the old, unread name is gone");
    }

    #[test]
    fn the_delete_request_names_the_event_the_way_the_service_looks_it_up() {
        let sent = serde_json::to_value(DeleteEventParams { id: "abc".into() }).unwrap();
        let read: DeleteEventParams = serde_json::from_value(sent.clone()).unwrap();
        assert_eq!(read.id, "abc");
        assert!(sent.get("event_id").is_none(), "the old, unread name is gone");
    }

    #[test]
    fn optional_fields_may_be_left_out_of_a_create() {
        let bare = serde_json::json!({
            "title": "Dentist", "start": "2026-09-22T10:00:00", "end": "2026-09-22T11:00:00"
        });
        let read: CreateEventParams = serde_json::from_value(bare).unwrap();
        assert_eq!(read.description, "");
        assert_eq!(read.location, None);
    }

    // ── Storing, finding and removing ────────────────────────────────

    #[test]
    fn an_event_that_was_stored_is_in_the_month_it_falls_in() {
        let f = Fixture::new();
        let store = f.store();
        // The directory does not exist yet: a calendar has to take its first appointment.
        let saved = store
            .create(&create("Launch review", "2026-09-22T14:00:00", "2026-09-22T15:00:00"))
            .expect("first event on a fresh machine");
        let listed = store.list(&month(2026, 9, 30)).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, saved.id);
        assert_eq!(listed[0].title, "Launch review");
        // And it is not in the month next door.
        assert!(store.list(&month(2026, 10, 31)).unwrap().is_empty());
    }

    #[test]
    fn an_event_on_the_last_day_of_the_month_is_in_that_month() {
        let f = Fixture::new();
        let store = f.store();
        store
            .create(&create("Month end", "2026-09-30T18:00:00", "2026-09-30T19:00:00"))
            .unwrap();
        assert_eq!(store.list(&month(2026, 9, 30)).unwrap().len(), 1);
    }

    #[test]
    fn an_event_spanning_the_first_of_the_month_is_listed_in_both() {
        let f = Fixture::new();
        let store = f.store();
        store
            .create(&create("Overnight", "2026-08-31T22:00:00", "2026-09-01T06:00:00"))
            .unwrap();
        assert_eq!(store.list(&month(2026, 8, 31)).unwrap().len(), 1, "the month it starts in");
        assert_eq!(store.list(&month(2026, 9, 30)).unwrap().len(), 1, "and the one it ends in");
    }

    #[test]
    fn a_stored_event_survives_a_new_store_over_the_same_directory() {
        let f = Fixture::new();
        f.store()
            .create(&create("Standup", "2026-09-21T09:00:00", "2026-09-21T09:15:00"))
            .unwrap();
        // A restart of the service, or of the machine.
        let listed = f.store().list(&month(2026, 9, 30)).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].title, "Standup");
    }

    #[test]
    fn listing_an_empty_machine_is_empty_not_an_error() {
        let f = Fixture::new();
        assert!(f.store().list(&month(2026, 9, 30)).unwrap().is_empty());
    }

    #[test]
    fn events_come_back_in_the_order_they_happen() {
        let f = Fixture::new();
        let store = f.store();
        store.create(&create("Second", "2026-09-22T15:00:00", "2026-09-22T16:00:00")).unwrap();
        store.create(&create("First", "2026-09-22T09:00:00", "2026-09-22T10:00:00")).unwrap();
        let listed = store.list(&month(2026, 9, 30)).unwrap();
        assert_eq!(
            listed.iter().map(|e| e.title.as_str()).collect::<Vec<_>>(),
            vec!["First", "Second"]
        );
    }

    #[test]
    fn delete_removes_only_the_event_named() {
        let f = Fixture::new();
        let store = f.store();
        let keep = store.create(&create("Keep", "2026-09-22T09:00:00", "2026-09-22T10:00:00")).unwrap();
        let drop_it = store.create(&create("Drop", "2026-09-22T11:00:00", "2026-09-22T12:00:00")).unwrap();
        store.delete(&drop_it.id).unwrap();
        let listed = store.list(&month(2026, 9, 30)).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, keep.id);
    }

    #[test]
    fn deleting_something_that_was_never_here_is_an_error() {
        let f = Fixture::new();
        // It reported success before, which told a caller a state change had happened when
        // nothing had.
        assert!(f.store().delete("01a0-not-a-real-event").is_err());
    }

    #[test]
    fn an_update_keeps_the_fields_it_was_not_given() {
        let f = Fixture::new();
        let store = f.store();
        let mut params = create("Review", "2026-09-22T14:00:00", "2026-09-22T15:00:00");
        params.description = "bring the release notes".into();
        let saved = store.create(&params).unwrap();

        let updated = store
            .update(&UpdateEventParams {
                id: saved.id.clone(),
                title: Some("Release review".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(updated.title, "Release review");
        assert_eq!(updated.description, "bring the release notes");
        assert_eq!(updated.start, "2026-09-22T14:00:00");
    }

    #[test]
    fn updating_an_event_that_is_not_here_is_an_error() {
        let f = Fixture::new();
        assert!(f
            .store()
            .update(&UpdateEventParams { id: "nope".into(), ..Default::default() })
            .is_err());
    }

    // ── One calendar, including the events that came from another one ─

    fn upsert(remote_id: &str, title: &str, start: &str, end: &str) -> UpsertRemoteEventParams {
        UpsertRemoteEventParams {
            remote_id: remote_id.into(),
            title: title.into(),
            start: start.into(),
            end: end.into(),
            description: String::new(),
            location: None,
            is_all_day: false,
            attendees: Vec::new(),
        }
    }

    #[test]
    fn syncing_the_same_remote_event_twice_stores_it_once() {
        // The companion's Google sync had no idempotent way into this store, so it kept its own
        // SQLite table instead and the Calendar app never saw a synced event at all. The key is
        // the id the event has at the far end.
        let f = Fixture::new();
        let store = f.store();
        let first = store
            .upsert_remote(&upsert("goog-1", "Design review", "2026-09-22T14:00:00", "2026-09-22T15:00:00"))
            .unwrap();
        let second = store
            .upsert_remote(&upsert("goog-1", "Design review", "2026-09-22T14:00:00", "2026-09-22T15:00:00"))
            .unwrap();

        assert_eq!(first.id, second.id, "the same remote event keeps the id this store gave it");
        assert_eq!(store.list(&month(2026, 9, 30)).unwrap().len(), 1, "and there is one of it");
        assert_eq!(second.remote_id.as_deref(), Some("goog-1"));
    }

    #[test]
    fn a_remote_event_that_changed_is_edited_rather_than_duplicated() {
        let f = Fixture::new();
        let store = f.store();
        let first = store
            .upsert_remote(&upsert("goog-1", "Design review", "2026-09-22T14:00:00", "2026-09-22T15:00:00"))
            .unwrap();
        let moved = store
            .upsert_remote(&upsert("goog-1", "Design review", "2026-09-22T16:00:00", "2026-09-22T17:00:00"))
            .unwrap();

        assert_eq!(moved.id, first.id);
        assert_eq!(moved.start, "2026-09-22T16:00:00");
        let listed = store.list(&month(2026, 9, 30)).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].start, "2026-09-22T16:00:00");
    }

    #[test]
    fn two_different_remote_events_are_two_events() {
        let f = Fixture::new();
        let store = f.store();
        store.upsert_remote(&upsert("goog-1", "One", "2026-09-22T09:00:00", "2026-09-22T10:00:00")).unwrap();
        store.upsert_remote(&upsert("goog-2", "Two", "2026-09-22T11:00:00", "2026-09-22T12:00:00")).unwrap();
        assert_eq!(store.list(&month(2026, 9, 30)).unwrap().len(), 2);
    }

    #[test]
    fn a_synced_event_is_listed_like_any_other_so_the_app_can_show_it() {
        let f = Fixture::new();
        let store = f.store();
        store
            .upsert_remote(&upsert("goog-1", "Design review", "2026-09-22T14:00:00", "2026-09-22T15:00:00"))
            .unwrap();
        let listed = store.list(&month(2026, 9, 30)).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].title, "Design review");
    }

    #[test]
    fn editing_a_remote_event_does_not_orphan_it_from_the_calendar_it_came_from() {
        // If an edit dropped the remote id, the next sync would see an event it had never been
        // told about and store a second copy beside this one.
        let f = Fixture::new();
        let store = f.store();
        let synced = store
            .upsert_remote(&upsert("goog-1", "Design review", "2026-09-22T14:00:00", "2026-09-22T15:00:00"))
            .unwrap();

        let edited = store
            .update(&UpdateEventParams {
                id: synced.id.clone(),
                title: Some("Design review (moved)".into()),
                start: Some("2026-09-22T16:00:00".into()),
                end: Some("2026-09-22T17:00:00".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(edited.remote_id.as_deref(), Some("goog-1"));

        // And a re-sync still finds it rather than storing a second one.
        store
            .upsert_remote(&upsert("goog-1", "Design review", "2026-09-22T16:00:00", "2026-09-22T17:00:00"))
            .unwrap();
        assert_eq!(store.list(&month(2026, 9, 30)).unwrap().len(), 1);
    }

    #[test]
    fn an_event_made_here_can_be_told_which_remote_event_it_became() {
        // The other direction: the mind creates an event, pushes it to Google, and records the id
        // it got there, so the next pull recognises it.
        let f = Fixture::new();
        let store = f.store();
        let made = store.create(&create("Retro", "2026-09-22T14:00:00", "2026-09-22T15:00:00")).unwrap();
        assert_eq!(made.remote_id, None);

        store
            .update(&UpdateEventParams {
                id: made.id.clone(),
                remote_id: Some("goog-9".into()),
                ..Default::default()
            })
            .unwrap();

        let resynced = store
            .upsert_remote(&upsert("goog-9", "Retro", "2026-09-22T14:00:00", "2026-09-22T15:00:00"))
            .unwrap();
        assert_eq!(resynced.id, made.id);
        assert_eq!(store.list(&month(2026, 9, 30)).unwrap().len(), 1);
    }

    #[test]
    fn an_upsert_without_a_remote_id_is_refused_rather_than_stored_unkeyed() {
        let f = Fixture::new();
        assert!(f
            .store()
            .upsert_remote(&upsert("  ", "Nameless", "2026-09-22T14:00:00", "2026-09-22T15:00:00"))
            .is_err());
    }

    // ── What a create can carry ──────────────────────────────────────

    #[test]
    fn an_all_day_event_is_stored_as_one() {
        // The mind's calendar tool accepted `all_day` long before this store had anywhere to put
        // it, so an all-day event asked for by the mind was kept as a timed one.
        let f = Fixture::new();
        let store = f.store();
        let mut params = create("Company holiday", "2026-09-23T00:00:00", "2026-09-23T23:59:59");
        params.is_all_day = true;
        let saved = store.create(&params).unwrap();
        assert!(saved.is_all_day);
        assert!(store.list(&month(2026, 9, 30)).unwrap()[0].is_all_day);
    }

    #[test]
    fn attendees_and_a_location_survive_the_round_trip() {
        let f = Fixture::new();
        let store = f.store();
        let mut params = create("Launch review", "2026-09-22T14:00:00", "2026-09-22T15:00:00");
        params.location = Some("Studio".into());
        params.attendees = vec![
            Attendee { name: "Ana".into(), email: "ana@example.com".into(), status: AttendeeStatus::Pending },
            Attendee { name: String::new(), email: "bo@example.com".into(), status: AttendeeStatus::Accepted },
        ];
        store.create(&params).unwrap();

        // Read back through a fresh store, which is the service restarting.
        let listed = f.store().list(&month(2026, 9, 30)).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].location.as_deref(), Some("Studio"));
        assert_eq!(listed[0].attendees.len(), 2);
        assert_eq!(listed[0].attendees[0].name, "Ana");
        assert_eq!(listed[0].attendees[1].email, "bo@example.com");
        assert_eq!(listed[0].attendees[1].status, AttendeeStatus::Accepted);
    }

    #[test]
    fn a_create_that_says_nothing_about_them_stores_neither() {
        let f = Fixture::new();
        let saved = f
            .store()
            .create(&create("Plain", "2026-09-22T14:00:00", "2026-09-22T15:00:00"))
            .unwrap();
        assert!(!saved.is_all_day);
        assert!(saved.attendees.is_empty());
    }

    #[test]
    fn an_update_can_change_the_new_fields_and_leaves_them_alone_when_it_does_not() {
        let f = Fixture::new();
        let store = f.store();
        let mut params = create("Review", "2026-09-22T14:00:00", "2026-09-22T15:00:00");
        params.attendees = vec![Attendee {
            name: "Ana".into(),
            email: "ana@example.com".into(),
            status: AttendeeStatus::Pending,
        }];
        let saved = store.create(&params).unwrap();

        let all_day = store
            .update(&UpdateEventParams {
                id: saved.id.clone(),
                is_all_day: Some(true),
                ..Default::default()
            })
            .unwrap();
        assert!(all_day.is_all_day);
        assert_eq!(all_day.attendees.len(), 1, "an update that said nothing about them kept them");

        let renamed = store
            .update(&UpdateEventParams {
                id: saved.id.clone(),
                title: Some("Release review".into()),
                ..Default::default()
            })
            .unwrap();
        assert!(renamed.is_all_day, "and kept the flag the previous update set");
    }

    #[test]
    fn the_create_request_the_app_sends_is_still_read_by_a_service_that_gained_fields() {
        // The new fields default, so the Calendar app's request — which carries neither — is
        // parsed exactly as it was before them.
        let bare = serde_json::json!({
            "title": "Dentist", "start": "2026-09-22T10:00:00", "end": "2026-09-22T11:00:00"
        });
        let read: CreateEventParams = serde_json::from_value(bare).unwrap();
        assert!(!read.is_all_day);
        assert!(read.attendees.is_empty());
    }

    // ── What the store refuses ───────────────────────────────────────

    #[test]
    fn an_event_needs_a_title() {
        let f = Fixture::new();
        assert!(f.store().create(&create("   ", "2026-09-22T14:00:00", "2026-09-22T15:00:00")).is_err());
    }

    #[test]
    fn a_time_that_is_not_a_time_is_refused_rather_than_stored() {
        let f = Fixture::new();
        let store = f.store();
        // The app's own default used to build this for anything after 23:00 — hour + 1 of 23.
        assert!(store.create(&create("Late", "2026-09-22T23:30:00", "2026-09-22T24:30:00")).is_err());
        assert!(store.list(&month(2026, 9, 30)).unwrap().is_empty(), "nothing half-written");
    }

    #[test]
    fn an_event_cannot_end_before_it_starts() {
        let f = Fixture::new();
        assert!(f
            .store()
            .create(&create("Backwards", "2026-09-22T15:00:00", "2026-09-22T14:00:00"))
            .is_err());
    }

    #[test]
    fn a_listing_with_an_unreadable_range_says_so() {
        let f = Fixture::new();
        let bad = EventsParams { start_date: "last tuesday".into(), end_date: "soon".into() };
        assert!(f.store().list(&bad).is_err());
    }

    #[test]
    fn files_that_are_not_events_are_ignored_rather_than_fatal() {
        let f = Fixture::new();
        let store = f.store();
        store.create(&create("Real", "2026-09-22T14:00:00", "2026-09-22T15:00:00")).unwrap();
        std::fs::write(f.0.join("notes.txt"), "not an event").unwrap();
        std::fs::write(f.0.join("broken.json"), "{ not json").unwrap();
        let listed = store.list(&month(2026, 9, 30)).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].title, "Real");
    }
}

#[cfg(test)]
mod view_tests {
    use super::views::{
        day_view, last_day_of_month, selected_date, start_and_end, timezone_label, visible_range,
        week_bounds, week_view, SourceEvent, ViewMode,
    };
    use chrono::NaiveDate;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).expect("a real date")
    }

    fn event(title: &str, start: &str, end: &str) -> SourceEvent {
        SourceEvent {
            title: title.into(),
            start: start.into(),
            end: end.into(),
            is_all_day: false,
            color_index: 0,
        }
    }

    fn all_day(title: &str, day: &str) -> SourceEvent {
        SourceEvent {
            title: title.into(),
            start: format!("{day}T00:00:00"),
            end: format!("{day}T23:59:59"),
            is_all_day: true,
            color_index: 0,
        }
    }

    // -- Where an event lands ----------------------------------------

    #[test]
    fn an_event_is_in_the_column_and_at_the_hour_it_was_stored_at() {
        // 2026-09-22 is a Tuesday, so the third column of a week that starts on Sunday.
        let week = week_view(
            &[event("Standup", "2026-09-22T09:30:00", "2026-09-22T09:45:00")],
            date(2026, 9, 22),
        );
        assert_eq!(week.events.len(), 1);
        let block = &week.events[0];
        assert_eq!(block.day_index, 2);
        assert_eq!(block.start_hour, 9);
        assert_eq!(block.start_min, 30);
    }

    #[test]
    fn a_blocks_height_is_the_minutes_between_its_start_and_its_end() {
        let week = week_view(
            &[event("Review", "2026-09-22T14:00:00", "2026-09-22T15:30:00")],
            date(2026, 9, 22),
        );
        assert_eq!(week.events[0].duration_min, 90);
    }

    #[test]
    fn the_day_view_puts_everything_in_the_one_column_in_order() {
        let day = day_view(
            &[
                event("Later", "2026-09-22T16:00:00", "2026-09-22T17:00:00"),
                event("Earlier", "2026-09-22T09:00:00", "2026-09-22T10:00:00"),
            ],
            date(2026, 9, 22),
        );
        let titles: Vec<&str> = day.events.iter().map(|e| e.title.as_str()).collect();
        assert_eq!(titles, ["Earlier", "Later"]);
        assert!(day.events.iter().all(|e| e.day_index == 0));
        assert_eq!(day.title, "Tuesday, 22 September 2026");
    }

    // -- Midnight ----------------------------------------------------

    #[test]
    fn an_event_running_past_midnight_is_clipped_at_the_day_and_continued_on_the_next() {
        let week = week_view(
            &[event("Deploy window", "2026-09-22T22:00:00", "2026-09-23T01:00:00")],
            date(2026, 9, 22),
        );
        assert_eq!(week.events.len(), 2, "one block per day it covers");

        let tuesday = &week.events[0];
        assert_eq!(tuesday.day_index, 2);
        assert_eq!((tuesday.start_hour, tuesday.start_min), (22, 0));
        assert_eq!(tuesday.duration_min, 120, "clipped at the end of Tuesday");

        let wednesday = &week.events[1];
        assert_eq!(wednesday.day_index, 3);
        assert_eq!((wednesday.start_hour, wednesday.start_min), (0, 0));
        assert_eq!(wednesday.duration_min, 60);
    }

    #[test]
    fn the_day_view_shows_only_the_part_of_a_spanning_event_that_is_on_that_day() {
        let day = day_view(
            &[event("Deploy window", "2026-09-22T22:00:00", "2026-09-23T01:00:00")],
            date(2026, 9, 23),
        );
        assert_eq!(day.events.len(), 1);
        assert_eq!((day.events[0].start_hour, day.events[0].start_min), (0, 0));
        assert_eq!(day.events[0].duration_min, 60);
    }

    #[test]
    fn an_event_ending_exactly_at_midnight_does_not_leave_an_empty_block_on_the_next_day() {
        let week = week_view(
            &[event("Evening", "2026-09-22T22:00:00", "2026-09-23T00:00:00")],
            date(2026, 9, 22),
        );
        assert_eq!(week.events.len(), 1);
        assert_eq!(week.events[0].duration_min, 120);
    }

    // -- What is not on the grid -------------------------------------

    #[test]
    fn an_event_outside_the_week_shown_is_not_in_it() {
        let events = [
            event("This week", "2026-09-22T09:00:00", "2026-09-22T10:00:00"),
            event("Next week", "2026-09-29T09:00:00", "2026-09-29T10:00:00"),
            event("Last week", "2026-09-15T09:00:00", "2026-09-15T10:00:00"),
        ];
        let week = week_view(&events, date(2026, 9, 22));
        let titles: Vec<&str> = week.events.iter().map(|e| e.title.as_str()).collect();
        assert_eq!(titles, ["This week"]);
    }

    #[test]
    fn an_all_day_event_is_not_given_an_hour_it_never_had() {
        let week = week_view(&[all_day("Company holiday", "2026-09-23")], date(2026, 9, 22));
        assert!(week.events.is_empty(), "nothing on the hour grid");
        assert_eq!(week.all_day[3], ["Company holiday"]);
        // The column header is the only place on a grid of hours that can say so.
        assert_eq!(week.labels[3], "Wed 23 \u{b7} 1 all day");

        let day = day_view(&[all_day("Company holiday", "2026-09-23")], date(2026, 9, 23));
        assert!(day.events.is_empty());
        assert_eq!(day.all_day, ["Company holiday"]);
        assert_eq!(day.title, "Wednesday, 23 September 2026 \u{b7} 1 all day");
    }

    #[test]
    fn an_event_whose_time_will_not_parse_is_skipped_rather_than_drawn_or_fatal() {
        let events = [
            event("Nonsense start", "next tuesday", "2026-09-22T10:00:00"),
            event("Nonsense end", "2026-09-22T09:00:00", "soon"),
            event("Backwards", "2026-09-22T15:00:00", "2026-09-22T14:00:00"),
            event("Empty", "", ""),
            event("Real", "2026-09-22T11:00:00", "2026-09-22T12:00:00"),
        ];
        let week = week_view(&events, date(2026, 9, 22));
        let titles: Vec<&str> = week.events.iter().map(|e| e.title.as_str()).collect();
        assert_eq!(titles, ["Real"]);

        let day = day_view(&events, date(2026, 9, 22));
        assert_eq!(day.events.len(), 1);
    }

    // -- The week, and where it starts -------------------------------

    #[test]
    fn the_week_starts_on_sunday_because_the_month_grid_does() {
        // calendar.slint draws Sun..Sat over the month, so a date's column has to be the same
        // number in both views or the same day is in two places.
        let (start, end) = week_bounds(date(2026, 9, 22));
        assert_eq!(start, date(2026, 9, 20));
        assert_eq!(end, date(2026, 9, 26));

        // A Sunday is the start of its own week, not the end of the one before.
        assert_eq!(week_bounds(date(2026, 9, 20)).0, date(2026, 9, 20));
    }

    #[test]
    fn the_column_headers_read_as_the_dates_of_the_week() {
        let week = week_view(&[], date(2026, 9, 22));
        assert_eq!(
            week.labels,
            ["Sun 20", "Mon 21", "Tue 22", "Wed 23", "Thu 24", "Fri 25", "Sat 26"]
        );
    }

    // -- A week that is in two months --------------------------------

    #[test]
    fn a_week_straddling_a_month_boundary_holds_events_from_both_months() {
        // 2026-10-01 is a Thursday, so its week runs Sun 27 September to Sat 3 October.
        let events = [
            event("September", "2026-09-29T09:00:00", "2026-09-29T10:00:00"),
            event("October", "2026-10-01T09:00:00", "2026-10-01T10:00:00"),
        ];
        let week = week_view(&events, date(2026, 10, 1));
        let placed: Vec<(&str, i32)> =
            week.events.iter().map(|e| (e.title.as_str(), e.day_index)).collect();
        assert_eq!(placed, [("September", 2), ("October", 4)]);
    }

    #[test]
    fn the_week_view_asks_the_store_for_the_days_the_week_needs_not_just_the_month() {
        // The app fetched exactly the month on screen, so the September half of this week was
        // never read and the week drew four empty columns without saying why.
        let (from, to) = visible_range(2026, 10, ViewMode::Week, 1);
        assert_eq!(from, date(2026, 9, 27), "back to the Sunday the week starts on");
        assert_eq!(to, date(2026, 10, 31), "and still the whole month for the month grid");

        // The last week of a month reaches the other way.
        let (from, to) = visible_range(2026, 9, ViewMode::Week, 30);
        assert_eq!(from, date(2026, 9, 1));
        assert_eq!(to, date(2026, 10, 3));

        // The month view asks for the month and no more.
        assert_eq!(
            visible_range(2026, 9, ViewMode::Month, 22),
            (date(2026, 9, 1), date(2026, 9, 30))
        );
    }

    // -- Which day the week and day views are about ------------------

    #[test]
    fn a_month_arrived_at_with_nothing_picked_opens_on_its_first_day() {
        assert_eq!(selected_date(2026, 9, 0), date(2026, 9, 1));
        assert_eq!(selected_date(2026, 9, -1), date(2026, 9, 1));
    }

    #[test]
    fn a_day_that_is_past_the_end_of_a_shorter_month_is_clamped_rather_than_lost() {
        assert_eq!(selected_date(2026, 9, 31), date(2026, 9, 30));
        assert_eq!(selected_date(2026, 2, 31), date(2026, 2, 28));
        assert_eq!(last_day_of_month(2024, 2), 29);
        assert_eq!(last_day_of_month(2026, 12), 31);
    }

    // -- Saving ------------------------------------------------------

    #[test]
    fn an_events_end_is_its_start_plus_how_long_it_runs() {
        let (start, end) = start_and_end("2026-09-22", "14:00", 90).unwrap();
        assert_eq!(start, "2026-09-22T14:00:00");
        assert_eq!(end, "2026-09-22T15:30:00");
    }

    #[test]
    fn a_late_event_ends_at_the_end_of_its_day_rather_than_at_a_time_that_does_not_exist() {
        // "23:30 plus an hour" was written as hour + 1 and produced T24:30:00, which the store
        // refuses outright -- so the evening appointment was simply not kept.
        let (_, end) = start_and_end("2026-09-22", "23:30", 60).unwrap();
        assert_eq!(end, "2026-09-22T23:59:00");
        let (_, end) = start_and_end("2026-09-22", "22:30", 120).unwrap();
        assert_eq!(end, "2026-09-22T23:59:00");
    }

    #[test]
    fn a_date_or_a_time_that_will_not_parse_is_refused_rather_than_sent_to_the_store() {
        assert!(start_and_end("next friday", "14:00", 60).is_none());
        assert!(start_and_end("2026-09-22", "half past two", 60).is_none());
    }

    #[test]
    fn the_timezone_strip_says_the_offset_it_can_actually_know() {
        assert_eq!(timezone_label(5 * 3600 + 1800), "Times are local, UTC+05:30");
        assert_eq!(timezone_label(0), "Times are local, UTC+00:00");
        assert_eq!(timezone_label(-(7 * 3600 + 1800)), "Times are local, UTC-07:30");
    }
}
