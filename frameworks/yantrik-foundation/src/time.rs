//! How long ago something happened, in the one way the OS says it.
//!
//! Before this module the OS said it nine ways: "4m ago" in the Mind panel, "4 min ago" in Files,
//! "95 min ago" in Weather, "10s ago" in the Agents workroom. Now every surface says the same:
//!
//! | elapsed            | said          |
//! |--------------------|---------------|
//! | under a minute     | `just now`    |
//! | under an hour      | `4 min ago`   |
//! | under a day        | `3 h ago`     |
//! | under 30 days      | `2 d ago`     |
//! | under a year       | `5 mo ago`    |
//! | a year or more     | `2 y ago`     |
//!
//! Units are words a person reads aloud, whole numbers only, always rounded down: a thing saved
//! 59 minutes ago was not saved "1 h ago". A time ahead of the clock (a clock that moved) is
//! "just now", never a negative age.

use std::time::{Duration, SystemTime};

const MIN: u64 = 60;
const HOUR: u64 = 60 * MIN;
const DAY: u64 = 24 * HOUR;
const MONTH: u64 = 30 * DAY;
const YEAR: u64 = 365 * DAY;

/// How long ago, for something `elapsed` in the past.
pub fn ago(elapsed: Duration) -> String {
    match elapsed.as_secs() {
        s if s < MIN => "just now".into(),
        s if s < HOUR => format!("{} min ago", s / MIN),
        s if s < DAY => format!("{} h ago", s / HOUR),
        s if s < MONTH => format!("{} d ago", s / DAY),
        s if s < YEAR => format!("{} mo ago", s / MONTH),
        s => format!("{} y ago", s / YEAR),
    }
}

/// [`ago`] for `then`, seen at `now`. A `then` after `now` is "just now".
pub fn ago_between(then: SystemTime, now: SystemTime) -> String {
    ago(now.duration_since(then).unwrap_or_default())
}

/// [`ago`] for an age in seconds, as unix timestamps are subtracted. A negative or non-finite age
/// is "just now".
pub fn ago_secs(secs: f64) -> String {
    let secs = if secs.is_finite() && secs > 0.0 {
        secs
    } else {
        0.0
    };
    ago(Duration::from_secs_f64(secs))
}

/// [`ago`] for a unix timestamp in seconds, seen now.
pub fn ago_since_unix(then_secs: f64) -> String {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    ago_secs(now - then_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> String {
        ago(Duration::from_secs(s))
    }

    #[test]
    fn every_band_reads_as_words() {
        assert_eq!(secs(0), "just now");
        assert_eq!(secs(59), "just now");
        assert_eq!(secs(60), "1 min ago");
        assert_eq!(secs(4 * MIN + 30), "4 min ago");
        assert_eq!(
            secs(HOUR - 1),
            "59 min ago",
            "rounded down, never up to an hour"
        );
        assert_eq!(secs(HOUR), "1 h ago");
        assert_eq!(secs(3 * HOUR + 10), "3 h ago");
        assert_eq!(secs(DAY), "1 d ago");
        assert_eq!(secs(29 * DAY), "29 d ago");
        assert_eq!(secs(MONTH), "1 mo ago");
        assert_eq!(secs(YEAR - 1), "12 mo ago");
        assert_eq!(secs(2 * YEAR), "2 y ago");
    }

    #[test]
    fn a_time_ahead_of_the_clock_is_just_now() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        assert_eq!(ago_between(now + Duration::from_secs(500), now), "just now");
        assert_eq!(
            ago_between(now - Duration::from_secs(7_200), now),
            "2 h ago"
        );
        assert_eq!(ago_secs(-30.0), "just now");
        assert_eq!(ago_secs(f64::NAN), "just now");
        assert_eq!(ago_secs(f64::INFINITY), "just now");
        assert_eq!(ago_secs(125.9), "2 min ago");
    }

    #[test]
    fn now_is_just_now() {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        assert_eq!(ago_since_unix(now), "just now");
        assert_eq!(ago_since_unix(now - 3.0 * DAY as f64 - 5.0), "3 d ago");
    }
}
