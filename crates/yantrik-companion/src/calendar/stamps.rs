//! Times, in the one form the calendar store keeps them in.
//!
//! The store parses `YYYY-MM-DDTHH:MM:SS`, or a bare `YYYY-MM-DD` as the start of that day, and
//! refuses anything else outright. Google answers RFC 3339 with an offset — `2026-03-10T14:00:00
//! +05:30` — and the companion's old SQLite rows hold whatever a model once typed at it. Handing
//! either straight to the service is a refusal, so a sync that did not convert would store
//! nothing and say it had synced.
//!
//! All-day events need the second function. Google's `end.date` is the day *after* the last one,
//! which read as a timed event is a day longer than the event is.

use chrono::{Local, NaiveDate, NaiveDateTime, TimeZone};

/// One timestamp in the store's form, or `None` if it is not a time at all.
///
/// An offset is resolved into this machine's local time rather than dropped: the store keeps
/// naive local stamps, which is what the Calendar app writes and what its month grid reads, so
/// keeping `+05:30`'s digits and throwing away the offset would move the event by the offset.
pub fn to_store_stamp(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(raw) {
        return Some(format(dt.with_timezone(&Local).naive_local()));
    }
    for shape in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M:%S"] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(raw, shape) {
            return Some(format(dt));
        }
    }
    if let Ok(d) = NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        return Some(format(d.and_hms_opt(0, 0, 0)?));
    }
    None
}

/// The start and end an all-day event is stored under, from the two dates a remote calendar gives.
///
/// `end_exclusive` is the day after the last one the event covers, which is how Google says it.
/// A one-day event arrives as 10th to 11th and is stored as the whole of the 10th.
pub fn all_day_bounds(start_date: &str, end_exclusive: &str) -> Option<(String, String)> {
    let first = NaiveDate::parse_from_str(start_date.trim(), "%Y-%m-%d").ok()?;
    let last = NaiveDate::parse_from_str(end_exclusive.trim(), "%Y-%m-%d")
        .ok()
        .and_then(|d| d.pred_opt())
        .filter(|d| *d >= first)
        .unwrap_or(first);
    Some((
        format(first.and_hms_opt(0, 0, 0)?),
        format(last.and_hms_opt(23, 59, 59)?),
    ))
}

/// The whole of one day, as the store keeps it. What "today" means to a calendar tool.
pub fn day_bounds(day: NaiveDate) -> Option<(String, String)> {
    Some((
        format(day.and_hms_opt(0, 0, 0)?),
        format(day.and_hms_opt(23, 59, 59)?),
    ))
}

/// A stored stamp as an instant a remote calendar will accept.
///
/// The store keeps naive local times, which is what the Calendar app writes. Google refuses a
/// `dateTime` with no offset unless it is told a time zone in the same breath, so the offset is
/// put on here rather than hoping the far end guesses this machine's.
pub fn to_rfc3339_local(stored: &str) -> Option<String> {
    let naive = NaiveDateTime::parse_from_str(stored.trim(), "%Y-%m-%dT%H:%M:%S").ok()?;
    Some(Local.from_local_datetime(&naive).earliest()?.to_rfc3339())
}

/// The day after this one, as a date. Google's all-day `end` is exclusive.
pub fn day_after(stored: &str) -> Option<String> {
    let day = NaiveDate::parse_from_str(stored.trim().split('T').next()?, "%Y-%m-%d").ok()?;
    Some(day.succ_opt()?.format("%Y-%m-%d").to_string())
}

/// The date part of a stored stamp.
pub fn date_of(stored: &str) -> &str {
    stored.split('T').next().unwrap_or(stored)
}

/// This machine's today.
///
/// Local, not UTC. The calendar tools asked UTC for the date and then compared it against events
/// the Calendar app had written in local time, so east of Greenwich after midnight UTC the mind
/// and the app disagreed about which day it was.
pub fn today() -> NaiveDate {
    Local.from_utc_datetime(&chrono::Utc::now().naive_utc()).date_naive()
}

fn format(dt: NaiveDateTime) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_naive_stamp_is_already_in_the_stores_form() {
        assert_eq!(
            to_store_stamp("2026-03-10T14:00:00").as_deref(),
            Some("2026-03-10T14:00:00")
        );
    }

    #[test]
    fn an_offset_is_resolved_rather_than_dropped() {
        // Whatever this machine's zone is, the instant is preserved: the same moment expressed
        // with a different offset has to land on the same stored stamp.
        let a = to_store_stamp("2026-03-10T14:00:00+05:30").unwrap();
        let b = to_store_stamp("2026-03-10T08:30:00+00:00").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn a_bare_date_is_the_start_of_that_day() {
        assert_eq!(to_store_stamp("2026-03-10").as_deref(), Some("2026-03-10T00:00:00"));
    }

    #[test]
    fn something_that_is_not_a_time_is_refused_here_rather_than_by_the_service() {
        assert!(to_store_stamp("next tuesday").is_none());
        assert!(to_store_stamp("").is_none());
        assert!(to_store_stamp("2026-03-10T24:30:00").is_none());
    }

    #[test]
    fn an_all_day_event_ends_on_its_last_day_not_the_one_after() {
        assert_eq!(
            all_day_bounds("2026-03-10", "2026-03-11"),
            Some(("2026-03-10T00:00:00".into(), "2026-03-10T23:59:59".into()))
        );
        assert_eq!(
            all_day_bounds("2026-03-10", "2026-03-13"),
            Some(("2026-03-10T00:00:00".into(), "2026-03-12T23:59:59".into()))
        );
        // An end that is not after the start is a calendar being odd, not a reason to store a
        // backwards event the service would refuse.
        assert_eq!(
            all_day_bounds("2026-03-10", "2026-03-10"),
            Some(("2026-03-10T00:00:00".into(), "2026-03-10T23:59:59".into()))
        );
    }
}
