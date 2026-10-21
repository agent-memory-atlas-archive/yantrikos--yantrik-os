//! Private mode at a service's door (#924): `Surface::answer`, the dispatch weather-service,
//! system-monitor-service, notifications-service and calendar-service answer `app.describe` and
//! `app.act` through, reads `privacy.json` for every call, as the shell publishes it.
//!
//! Its own test binary, so it can own `HOME` without moving it under any other test.

use std::sync::{Mutex, MutexGuard, OnceLock};

use serde_json::{json, Value};
use yantrik_ipc_transport::privacy;
use yantrik_surface::{answer_later, Action, Surface, View, PRIVATE_SUMMARY};

const SENTINEL: &str = "Reminder: call Priya SENTINEL-924";

fn leaks(answer: &Value) -> bool {
    answer.to_string().contains("SENTINEL-924")
}

/// This binary's home, set once, and the lock that keeps one test at a time on the switch.
fn machine() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    static HOME: OnceLock<()> = OnceLock::new();
    let held = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    HOME.get_or_init(|| {
        let home = std::env::temp_dir().join(format!("yantrik-surface-private-{}", std::process::id()));
        std::fs::create_dir_all(&home).expect("a home of our own");
        std::env::set_var("HOME", home);
    });
    privacy::publish(false, 1).expect("privacy.json written");
    held
}

fn reminders() -> Surface {
    Surface::new("reminders")
        .describe(|| View::new(format!("Reminders — next: {SENTINEL}")).with("next", SENTINEL))
        .action(Action::new("snooze", "Snooze the next reminder").risk("safe"), |_| Ok(json!({ "snoozed": true })))
        .action(Action::new("sync", "Fetch reminders, off the worker").risk("safe"), |_| {
            // The person turns Private mode on while the work runs.
            let work = || {
                privacy::publish(true, 1).map_err(|e| e.to_string())?;
                Ok(json!({ "fetched": 1 }))
            };
            answer_later(work).map(|()| json!("replaced")).or_else(|work| work())
        })
}

fn answer(surface: &Surface, method: &str, params: Value) -> Value {
    surface.answer(method, &params, None).expect("a surface method").expect("answered")
}

#[test]
fn a_services_describe_and_act_follow_private_mode_from_the_next_call() {
    let _machine = machine();
    let surface = reminders();
    assert_eq!(answer(&surface, "app.describe", json!({}))["state"]["next"], SENTINEL);

    privacy::publish(true, 1).unwrap();
    let hidden = answer(&surface, "app.describe", json!({}));
    assert_eq!(hidden["private"], true);
    assert_eq!(hidden["summary"], PRIVATE_SUMMARY);
    assert_eq!(hidden["state"], json!({ "private": true }), "a service has no window and keeps nothing");
    assert!(!leaks(&hidden), "{hidden}");
    let acted = answer(&surface, "app.act", json!({ "action": "snooze", "args": {} }));
    assert_eq!(acted["result"], json!({ "snoozed": true }), "the person's own call still runs");
    assert_eq!(acted["private"], true);
    assert!(!leaks(&acted), "{acted}");

    privacy::publish(false, 1).unwrap();
    assert_eq!(answer(&surface, "app.describe", json!({}))["state"]["next"], SENTINEL);
}

#[test]
fn a_services_work_finished_later_is_read_back_under_private_mode_as_it_is_then() {
    let _machine = machine();
    let surface = reminders();
    let reply = answer(&surface, "app.act", json!({ "action": "sync", "args": {} }));
    assert_eq!(reply["result"], json!({ "fetched": 1 }), "{reply}");
    assert_eq!(reply["private"], true, "Private mode came on while the work ran: {reply}");
    assert!(!leaks(&reply), "{reply}");
    privacy::publish(false, 1).unwrap();
}
