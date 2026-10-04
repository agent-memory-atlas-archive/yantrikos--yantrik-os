//! The last process sample, so `describe` answers without taking one.
//!
//! A process's CPU share is a fact about an interval, and the table's interval is half a second
//! (`CPU_SAMPLE_WINDOW`). `describe` took a fresh sample on every call, so every read of this
//! surface slept for that half second and `yos check` timed it at about 545 ms — over the 500 ms
//! a caller should wait for a read. The window polls `sysmon.processes` every two seconds while it
//! is open, so a sample is usually a moment old already; `describe` answers from it and says how
//! old it is, and only a caller with no recent sample at all waits for one.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use yantrik_ipc_contracts::system_monitor::ProcessInfo;

/// How many of the busiest processes `describe` names.
pub const SHOWN: usize = 5;

/// A sample this young is as current as a new one would be: the window samples this often.
const CURRENT_FOR: Duration = Duration::from_secs(2);

/// Past this, a sample is no longer "what the machine is doing", and the caller waits for a new
/// one rather than being told about a machine as it was.
const STALE_AFTER: Duration = Duration::from_secs(30);

static LAST: Mutex<Option<(Instant, Vec<ProcessInfo>)>> = Mutex::new(None);
static REFRESHING: AtomicBool = AtomicBool::new(false);

/// What a `describe` does with a sample of this age (`None`: there is none).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Plan {
    Serve,
    /// Answer from it now, and take a new one behind the answer for the next caller.
    ServeAndRefresh,
    SampleNow,
}

fn plan(age: Option<Duration>) -> Plan {
    match age {
        Some(age) if age <= CURRENT_FOR => Plan::Serve,
        Some(age) if age <= STALE_AFTER => Plan::ServeAndRefresh,
        _ => Plan::SampleNow,
    }
}

/// Keep the busiest few of a full sample, whatever order its caller asked for.
pub fn remember(procs: &[ProcessInfo]) {
    let mut top = procs.to_vec();
    top.sort_by(|a, b| b.cpu_percent.total_cmp(&a.cpu_percent));
    top.truncate(SHOWN);
    if let Ok(mut last) = LAST.lock() {
        *last = Some((Instant::now(), top));
    }
}

/// The busiest processes, and how old the sample they come from is. `sample` takes a new one
/// (and, through `read_processes`, remembers it).
pub fn busiest(sample: fn() -> Vec<ProcessInfo>) -> (Vec<ProcessInfo>, Duration) {
    let last = LAST.lock().ok().and_then(|last| last.clone());
    match (plan(last.as_ref().map(|(at, _)| at.elapsed())), last) {
        (Plan::Serve, Some((at, top))) => (top, at.elapsed()),
        (Plan::ServeAndRefresh, Some((at, top))) => {
            // One refresh at a time: a burst of reads must not start a burst of samplers.
            if !REFRESHING.swap(true, Ordering::AcqRel) {
                std::thread::spawn(move || {
                    sample();
                    REFRESHING.store(false, Ordering::Release);
                });
            }
            (top, at.elapsed())
        }
        _ => {
            let mut top = sample();
            top.truncate(SHOWN);
            (top, Duration::ZERO)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sample_the_window_just_took_is_served_as_it_is() {
        assert_eq!(plan(Some(Duration::from_millis(400))), Plan::Serve);
    }

    #[test]
    fn a_sample_seconds_old_is_served_and_replaced_behind_the_answer() {
        assert_eq!(plan(Some(Duration::from_secs(10))), Plan::ServeAndRefresh);
    }

    #[test]
    fn no_sample_or_an_old_one_is_waited_for() {
        assert_eq!(plan(None), Plan::SampleNow);
        assert_eq!(plan(Some(Duration::from_secs(31))), Plan::SampleNow);
    }
}
