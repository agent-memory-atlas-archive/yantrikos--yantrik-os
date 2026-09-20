//! The calendar's storage rules, tested without a socket or a desktop.
//!
//! The bugs these cover were all invisible from either side on its own: the app asked for
//! `start`/`end` where the service required `start_date`/`end_date`, and sent `event_id` where
//! it read `id`, so listing and deleting failed on every machine while both files looked right.
//! The payload tests below are the ones that would have caught it — they send what the app
//! sends and read it the way the service reads it.

#[path = "../../services/calendar-service/src/store.rs"]
pub mod store;

#[cfg(test)]
mod tests {
    use super::store::EventStore;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use yantrik_ipc_contracts::calendar::{
        CreateEventParams, DeleteEventParams, EventsParams, UpdateEventParams,
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
