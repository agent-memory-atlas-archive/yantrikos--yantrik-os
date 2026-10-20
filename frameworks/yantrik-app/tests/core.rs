//! The core's contract (design/app-framework-2026-10-21.md, §§3–4), on a toy app with a
//! headless window: a refused command changes nothing and runs no effect; `then` runs after the
//! render, in order; a job's result is applied only while the core waits on it; timers fire on
//! the virtual clock; a close request ends the process; a post after shutdown is dropped.

use std::time::Duration;

use yantrik_app::app::{App, Boot, Changed, Event, ExitReason, Startup, WindowId};
use yantrik_app::cx::{Cx, Inbound};
use yantrik_app::effects::Effects;
use yantrik_app::jobs::{Concurrency, JobKey, Scope};
use yantrik_app::queue::Dropped;
use yantrik_app::refused::Refused;
use yantrik_app::timers::TimerId;
use yantrik_testing::TestApp;

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
    Tick,
    /// A keyed job that is not started while one of its kind runs; `Busy` is applied instead.
    Guarded(i32),
    Busy,
    /// Arms a one-shot timer that fires `Chain` again: a timer's command arming a timer.
    Chain,
    /// Asks for itself again: a `then` cycle.
    Ping,
    /// Changes the model without saying which part: every part is redrawn.
    Quiet,
}

struct Counter;

impl App for Counter {
    type Ui = CounterWin;
    type Model = Model;
    type Command = Command;
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
            }
            Command::Tick => m.log.push("tick".into()),
            Command::Guarded(tag) => {
                fx.spawn_keyed_or(
                    JobKey {
                        feature: "guard",
                        scope: Scope::App,
                    },
                    Concurrency::RejectIfRunning,
                    Command::Busy,
                    move |_| Command::Fetched(tag),
                );
            }
            Command::Busy => m.log.push("busy".into()),
            Command::Chain => {
                m.log.push("chain".into());
                fx.after(TimerId("chain"), Duration::from_secs(1), Command::Chain);
            }
            Command::Ping => {
                m.log.push("ping".into());
                fx.then(Command::Ping);
            }
            Command::Quiet => m.n += 10,
        }
        Ok(())
    }

    fn render(ui: &CounterWin, _: WindowId, m: &Model, changed: &Changed) {
        ui.set_renders(ui.get_renders() + 1);
        if changed.has("count") {
            ui.set_count(m.n);
        }
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

/// §4: saturation cannot lose a job's outcome. The inbox is filled to its capacity with commands,
/// then the job lands: its result still arrives, and its key is free again.
#[test]
fn a_full_inbox_does_not_lose_a_jobs_result() {
    let app = TestApp::<Counter>::start();
    app.run(Command::Fetch(7));
    let post = app.post();
    for _ in 0..yantrik_app::queue::CAPACITY {
        post.post(Inbound::Command(Command::Add(0))).unwrap();
    }
    assert_eq!(
        post.post(Inbound::Command(Command::Add(0))),
        Err(Dropped::Full),
        "commands are refused at the bound"
    );
    app.run_jobs();
    assert!(app.model(|m| m.log.contains(&"fetched 7".to_string())));
    assert_eq!(app.jobs_waiting(), 0, "the key is free again");
    app.run(Command::Fetch(8));
    app.run_jobs();
    assert!(app.model(|m| m.log.contains(&"fetched 8".to_string())));
}

/// A timer's command that arms a timer: the clock is released before the command runs.
#[test]
fn a_timers_command_may_arm_a_timer() {
    let app = TestApp::<Counter>::start();
    app.run(Command::Chain);
    app.advance(Duration::from_millis(3500));
    assert_eq!(
        app.model(|m| m.log.iter().filter(|l| *l == "chain").count()),
        4,
        "the first, then one a second"
    );
}

/// A keyed job refused because one of its kind runs tells the app so, through its `failed`
/// command, as a panicked or refused job would.
#[test]
fn a_rejected_job_runs_its_failed_command() {
    let app = TestApp::<Counter>::start();
    app.run(Command::Guarded(1));
    app.run(Command::Guarded(2));
    assert_eq!(app.model(|m| m.log.clone()), vec!["busy"]);
    app.run_jobs();
    assert_eq!(app.model(|m| m.log.clone()), vec!["busy", "fetched 1"]);
}

/// A `then` cycle cannot freeze the UI thread: one drain applies a bounded number and the rest
/// wait for the next wake.
#[test]
fn a_then_cycle_yields_to_the_event_loop() {
    let app = TestApp::<Counter>::start();
    app.run(Command::Ping);
    let pings = app.model(|m| m.log.iter().filter(|l| *l == "ping").count());
    assert_eq!(pings, yantrik_app::cx::PENDING_PER_DRAIN + 1);
    assert_eq!(app.pending_commands(), 1, "the rest waits");
    app.tick();
    let pings = app.model(|m| m.log.iter().filter(|l| *l == "ping").count());
    assert_eq!(pings, 2 * yantrik_app::cx::PENDING_PER_DRAIN + 1);
}

/// An `update` that changes the model and does not say which part redraws every part.
#[test]
fn a_command_that_names_no_part_redraws_everything() {
    let app = TestApp::<Counter>::start();
    app.run(Command::Quiet);
    assert_eq!(app.ui(|ui| (ui.get_count(), ui.get_renders())), (10, 2));
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
