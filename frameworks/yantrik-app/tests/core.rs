//! The core's contract (design/app-framework-2026-10-21.md, §§3–4), on a toy app with a
//! headless window: a refused command changes nothing and runs no effect; `then` runs after the
//! render, in order; a job's result is applied only while the core waits on it; timers fire on
//! the virtual clock; a close request ends the process; a post after shutdown is dropped. And
//! the intents through the real dispatch: every terminal outcome of an explicit settlement.

use std::time::Duration;

use serde_json::json;
use yantrik_app::app::{App, Boot, Changed, Event, ExitReason, Startup, WindowId};
use yantrik_app::control::{Action, Param, View};
use yantrik_app::cx::{Cx, Inbound};
use yantrik_app::effects::Effects;
use yantrik_app::intents::{Agents, Intent, Intents, Settle};
use yantrik_app::jobs::{Concurrency, JobKey, Scope};
use yantrik_app::queue::Dropped;
use yantrik_app::refused::Refused;
use yantrik_app::timers::TimerId;
use yantrik_testing::{Call, TestApp};

slint::slint! {
    export component CounterWin inherits Window {
        in-out property <int> count: 0;
        in-out property <int> renders: 0;
        callback bump();
    }
}

impl yantrik_app::Window for CounterWin {
    fn make() -> Result<Self, slint::PlatformError> {
        CounterWin::new()
    }
    fn apply_theme(&self, _: &yantrik_app::theme::ThemeSettings) {}
    fn connect_chrome(&self) {}
}

#[derive(Default)]
struct Model {
    n: i32,
    log: Vec<String>,
    locked: bool,
}

#[derive(Debug, Clone)]
enum Command {
    Add(i32),
    /// Refused after asking for an effect: neither the change nor the effect may happen.
    Refuse,
    /// Adds `n` through `then`, after its own render.
    Later(i32),
    /// A keyed job that replaces the previous one and returns its tag.
    Fetch(i32),
    Fetched(i32),
    /// A job that panics.
    Boom,
    /// Settles its invocation at once, with a refusal.
    SettleNo,
    Lock,
    Tick,
}

struct Counter;

impl App for Counter {
    type Ui = CounterWin;
    type Model = Model;
    type Command = Command;
    type Settings = ();
    const ID: &'static str = "counter-test";

    fn model(_: &Boot) -> Result<Model, Startup> {
        Ok(Model::default())
    }

    fn wire(cx: &Cx<Self>) {
        cx.ui().on_bump(cx.cmd(|| Command::Add(1)));
    }

    fn start(_: &Model, fx: &mut Effects<Self>) {
        fx.every(TimerId("tick"), Duration::from_secs(4), Command::Tick);
    }

    fn update(m: &mut Model, command: Command, fx: &mut Effects<Self>) -> Result<(), Refused> {
        match command {
            Command::Add(k) => {
                m.n += k;
                m.log.push(format!("add {k}"));
                fx.changed("count");
            }
            Command::Refuse => {
                fx.then(Command::Add(100));
                return Err(Refused::new("no", "Not now."));
            }
            Command::Later(k) => {
                m.log.push("later".into());
                fx.then(Command::Add(k));
                fx.changed("count");
            }
            Command::Fetch(tag) => {
                fx.spawn_keyed(
                    JobKey {
                        feature: "counter",
                        scope: Scope::App,
                    },
                    Concurrency::ReplacePrevious,
                    move |_| Command::Fetched(tag),
                );
            }
            Command::Fetched(tag) => {
                m.log.push(format!("fetched {tag}"));
                m.n = tag;
            }
            Command::Boom => {
                fx.spawn(|_| panic!("the fetch exploded"));
            }
            Command::SettleNo => fx.settle(Err(Refused::new("no", "Settled: no."))),
            Command::Lock => m.locked = true,
            Command::Tick => m.log.push("tick".into()),
        }
        Ok(())
    }

    fn render(ui: &CounterWin, _: WindowId, m: &Model, changed: &Changed) {
        ui.set_renders(ui.get_renders() + 1);
        if changed.has("count") {
            ui.set_count(m.n);
        }
    }

    fn describe(m: &Model) -> View {
        View::new(format!("count {}", m.n)).with("count", m.n)
    }

    fn intents(table: &mut Intents<Self>) {
        table.state_rule(|m| {
            if m.locked {
                Err(Refused::new("locked", "The counter is locked."))
            } else {
                Ok(())
            }
        });
        table.add(
            Intent::new(
                Action::new("add", "Add to the count").arg(Param::integer("by").default(1)),
                |a| {
                    Ok(Command::Add(
                        i32::try_from(a.integer("by")?)
                            .map_err(|_| Refused::new("too_big", "Too big."))?,
                    ))
                },
            )
            .reply(|m: &Model| json!({ "count": m.n })),
        );
        table.add(
            Intent::new(
                Action::new("fetch", "Fetch a tag").arg(Param::integer("tag")),
                |a| Ok(Command::Fetch(a.integer("tag")? as i32)),
            )
            .settles(Settle::Explicit {
                within: Duration::from_millis(300),
            })
            .reply(|m: &Model| json!({ "count": m.n, "log": m.log })),
        );
        table.add(
            Intent::new(Action::new("boom", "A job that fails"), |_| {
                Ok(Command::Boom)
            })
            .settles(Settle::Explicit {
                within: Duration::from_millis(300),
            }),
        );
        table.add(
            Intent::new(Action::new("settle_no", "Settles with a refusal"), |_| {
                Ok(Command::SettleNo)
            })
            .settles(Settle::Explicit {
                within: Duration::from_millis(300),
            }),
        );
        table.add(
            Intent::new(
                Action::new("kick", "Starts a fetch and answers at once")
                    .arg(Param::integer("tag")),
                |a| Ok(Command::Fetch(a.integer("tag")? as i32)),
            )
            .settles(Settle::Deferred),
        );
        table.add(
            Intent::new(
                Action::new("reset", "Back to zero").risk("dangerous"),
                |_| Ok(Command::Add(-1000)),
            )
            .agents(Agents::Refused("An agent does not reset the count.")),
        );
        table.add(Intent::new(Action::new("lock", "Lock the counter"), |_| {
            Ok(Command::Lock)
        }));
    }
}

#[test]
fn a_command_changes_the_model_and_renders_it() {
    let app = TestApp::<Counter>::start();
    assert_eq!(app.ui(|ui| ui.get_renders()), 1, "the first render");
    app.run(Command::Add(2));
    app.ui(|ui| ui.invoke_bump());
    assert_eq!(app.model(|m| m.n), 3);
    assert_eq!(app.ui(|ui| (ui.get_count(), ui.get_renders())), (3, 3));
}

#[test]
fn a_refused_command_changes_nothing_and_runs_no_effect() {
    let app = TestApp::<Counter>::start();
    app.run(Command::Refuse);
    assert_eq!(app.model(|m| m.n), 0, "the `then` it asked for did not run");
    assert_eq!(
        app.last_refusal().map(|r| r.sentence),
        Some("Not now.".into())
    );
    assert_eq!(app.ui(|ui| ui.get_renders()), 1, "nothing to redraw");
    app.run(Command::Add(1));
    assert!(
        app.last_refusal().is_none(),
        "an accepted command clears the refusal"
    );
}

#[test]
fn then_runs_after_the_render_in_order() {
    let app = TestApp::<Counter>::start();
    app.run(Command::Later(5));
    assert_eq!(app.model(|m| m.log.clone()), vec!["later", "add 5"]);
    assert_eq!(app.ui(|ui| ui.get_count()), 5);
}

#[test]
fn a_replaced_jobs_late_result_is_dropped() {
    let app = TestApp::<Counter>::start();
    app.run(Command::Fetch(1));
    app.run(Command::Fetch(2));
    assert_eq!(app.jobs_waiting(), 1, "the first fetch was replaced");
    app.run_jobs();
    assert_eq!(
        app.model(|m| m.log.clone()),
        vec!["fetched 2"],
        "the first job ran, its result was dropped"
    );
    assert_eq!(app.jobs_waiting(), 0);
}

#[test]
fn timers_fire_on_the_virtual_clock_and_stop_at_shutdown() {
    let app = TestApp::<Counter>::start();
    assert_eq!(app.timers_armed(), 1);
    app.advance(Duration::from_secs(9));
    assert_eq!(
        app.model(|m| m.log.iter().filter(|l| *l == "tick").count()),
        2
    );
    app.shutdown();
    assert_eq!(app.timers_armed(), 0);
}

#[test]
fn a_close_request_with_no_handler_ends_the_process() {
    let app = TestApp::<Counter>::start();
    app.post()
        .post(Inbound::Event(Event::CloseRequested(WindowId::PRIMARY)))
        .unwrap();
    assert!(
        app.exit_reason().is_none(),
        "nothing happens until the UI thread takes it"
    );
    app.tick();
    assert_eq!(app.exit_reason(), Some(ExitReason::LastWindowClosed));
}

#[test]
fn a_post_after_shutdown_is_dropped_harmlessly() {
    let app = TestApp::<Counter>::start();
    app.shutdown();
    assert_eq!(
        app.post().post(Inbound::Command(Command::Add(1))),
        Err(Dropped::Closed)
    );
    app.tick();
    assert_eq!(app.model(|m| m.n), 0);
}

// ── Intents, through the real dispatch ──────────────────────────────

#[test]
fn an_intent_is_the_same_command_the_button_runs_and_replies_from_the_model() {
    let app = TestApp::<Counter>::start();
    let answer = app.act("add", json!({"by": 4})).unwrap();
    assert!(!answer.is_owed());
    let envelope = answer.envelope().clone();
    assert_eq!(envelope["result"], json!({"count": 4}));
    assert_eq!(envelope["settled"], json!(true));
    assert_eq!(
        envelope["state"]["count"],
        json!(4),
        "the state beside the result is the committed one"
    );
    assert_eq!(app.ui(|ui| ui.get_count()), 4, "the window was rendered");
    assert_eq!(app.describe()["summary"], json!("count 4"));
}

#[test]
fn a_wrong_argument_an_agent_rule_and_a_state_rule_each_refuse_before_anything_runs() {
    let app = TestApp::<Counter>::start();
    let e = app.act("add", json!({"by": "four"})).unwrap_err();
    assert!(e.contains("by"), "{e}");
    let e = app
        .act_as(Call::new("reset", json!({})).as_agent("tok-1"))
        .unwrap_err();
    assert_eq!(e, "An agent does not reset the count.");
    assert!(app.act("reset", json!({})).is_ok(), "the person may");
    app.run(Command::Lock);
    let e = app.act("add", json!({})).unwrap_err();
    assert_eq!(e, "The counter is locked.");
    assert_eq!(app.model(|m| m.n), -1000, "only the person's reset ran");
}

#[test]
fn a_stale_revision_and_an_unapproved_dangerous_call_are_refused_by_the_dispatch() {
    let app = TestApp::<Counter>::start();
    let e = app
        .act_as(Call::new("add", json!({})).expecting("not-this-one"))
        .unwrap_err();
    assert!(e.starts_with("STALE:"), "{e}");
    let rev = app.revision();
    assert!(app
        .act_as(Call::new("add", json!({})).expecting(&rev))
        .is_ok());
    assert!(
        app.act_as(Call::new("reset", json!({})).in_mode("ask"))
            .is_err(),
        "dangerous, asked about, not granted"
    );
    assert!(app
        .act_as(Call::new("reset", json!({})).in_mode("ask").granted())
        .is_ok());
}

#[test]
fn an_explicit_settlement_answers_once_when_the_job_lands() {
    let app = TestApp::<Counter>::start();
    let pending = app.act("fetch", json!({"tag": 7})).unwrap();
    assert!(pending.is_owed());
    assert_eq!(app.settlements_open(), 1);
    app.run_jobs();
    let result = pending.settle().unwrap();
    assert_eq!(result["count"], json!(7));
    assert_eq!(
        result["log"],
        json!(["fetched 7"]),
        "the reply is read after the result committed"
    );
    assert_eq!(app.settlements_open(), 0);
}

#[test]
fn every_other_terminal_outcome_is_a_refusal_not_a_hang() {
    let app = TestApp::<Counter>::start();
    // The job panicked.
    let pending = app.act("boom", json!({})).unwrap();
    app.run_jobs();
    let e = pending.settle().unwrap_err();
    assert!(e.contains("the work failed"), "{e}");
    // The job was replaced by a newer request.
    let first = app.act("fetch", json!({"tag": 1})).unwrap();
    let second = app.act("fetch", json!({"tag": 2})).unwrap();
    app.run_jobs();
    assert!(first.settle().unwrap_err().contains("replaced"));
    assert_eq!(second.settle().unwrap()["count"], json!(2));
    // The app settled with a refusal of its own.
    let pending = app.act("settle_no", json!({})).unwrap();
    assert_eq!(pending.settle().unwrap_err(), "Settled: no.");
    // The deadline: the job is never run.
    let pending = app.act("fetch", json!({"tag": 9})).unwrap();
    let e = pending.settle().unwrap_err();
    assert!(e.contains("did not settle"), "{e}");
    // Shutdown with an answer owed.
    let pending = app.act("fetch", json!({"tag": 10})).unwrap();
    app.shutdown();
    assert_eq!(pending.settle().unwrap_err(), "the app is closing");
    assert_eq!(app.settlements_open(), 0);
}

#[test]
fn a_deferred_intent_answers_at_once_and_says_it_is_not_settled() {
    let app = TestApp::<Counter>::start();
    let answer = app.act("kick", json!({"tag": 3})).unwrap();
    assert!(!answer.is_owed());
    assert_eq!(answer.envelope()["settled"], json!(false));
    assert_eq!(app.jobs_waiting(), 1);
}
