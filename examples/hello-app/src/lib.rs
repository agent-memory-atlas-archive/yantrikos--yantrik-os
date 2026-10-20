//! The toy app on YantrikApp (design/app-framework-2026-10-21.md): what every app is made of,
//! small enough to read in one sitting.
//!
//! - the model is plain Rust, changed only by `update`;
//! - the button and the Mind's `add` are the same command, `Add`;
//! - `Fetch` starts a job off the UI thread, and `Fetched` carries its result back; the Mind's
//!   `fetch` is answered when it lands;
//! - a timer ticks every four seconds through the same path;
//! - `render` is the only code that writes the window, `describe` the only view the Mind reads.
//!
//! `main.rs` is three lines. The tests run it headless through `yantrik-testing`.

use std::time::Duration;

use serde_json::json;
use yantrik_app::app::{App, Boot, Changed, Startup, WindowId};
use yantrik_app::control::{Action, Param, View};
use yantrik_app::cx::Cx;
use yantrik_app::effects::Effects;
use yantrik_app::intents::{Agents, Intent, Intents, Settle};
use yantrik_app::jobs::{Concurrency, JobKey, Scope};
use yantrik_app::refused::Refused;
use yantrik_app::timers::TimerId;

slint::include_modules!();
yantrik_app::window!(HelloApp);

pub const PART_COUNT: &str = "count";
pub const PART_FETCH: &str = "fetch";
pub const TICK: TimerId = TimerId("hello.tick");

#[derive(Debug, Default)]
pub struct Model {
    pub count: i32,
    pub fetched: Option<String>,
    pub ticks: u32,
}

#[derive(Debug, Clone)]
pub enum Command {
    Add(i32),
    Fetch,
    Fetched(Result<String, String>),
    Tick,
    /// The count back to zero.
    Reset,
}

pub struct Hello;

impl App for Hello {
    type Ui = HelloApp;
    type Model = Model;
    type Command = Command;
    const ID: &'static str = "hello-app";

    fn model(_: &Boot) -> Result<Model, Startup> {
        Ok(Model::default())
    }

    fn wire(cx: &Cx<Self>) {
        cx.ui().on_add_one(cx.cmd(|| Command::Add(1)));
        cx.ui().on_fetch(cx.cmd(|| Command::Fetch));
    }

    fn start(_: &Model, fx: &mut Effects<Self>) {
        fx.every(TICK, Duration::from_secs(4), Command::Tick);
    }

    fn update(m: &mut Model, command: Command, fx: &mut Effects<Self>) -> Result<(), Refused> {
        match command {
            Command::Add(by) => {
                let next = m
                    .count
                    .checked_add(by)
                    .ok_or_else(|| Refused::new("overflow", "That would overflow the counter."))?;
                m.count = next;
                fx.changed(PART_COUNT);
            }
            Command::Fetch => {
                // One fetch at a time; a second press replaces the first, whose result is dropped.
                fx.spawn_keyed(
                    JobKey {
                        feature: "hello",
                        scope: Scope::App,
                    },
                    Concurrency::ReplacePrevious,
                    |cancel| {
                        std::thread::sleep(Duration::from_millis(50));
                        if cancel.is_set() {
                            return Command::Fetched(Err("cancelled".into()));
                        }
                        Command::Fetched(Ok("the weather is fine".into()))
                    },
                );
            }
            Command::Fetched(result) => {
                m.fetched = Some(result.unwrap_or_else(|e| format!("not fetched: {e}")));
                fx.changed(PART_FETCH);
            }
            Command::Tick => m.ticks += 1,
            Command::Reset => {
                m.count = 0;
                fx.changed(PART_COUNT);
            }
        }
        Ok(())
    }

    fn render(ui: &HelloApp, _: WindowId, m: &Model, changed: &Changed) {
        if changed.has(PART_COUNT) {
            ui.set_count(m.count);
        }
        if changed.has(PART_FETCH) {
            ui.set_fetched(m.fetched.clone().unwrap_or_default().into());
        }
    }

    fn describe(m: &Model) -> View {
        View::new(format!("Hello — count {}", m.count))
            .with("count", m.count)
            .with("fetched", m.fetched.clone().unwrap_or_default())
    }

    fn intents(table: &mut Intents<Self>) {
        table.add(
            Intent::new(
                Action::new("add", "Add to the count")
                    .arg(Param::integer("by").default(1).describe("How much to add")),
                |a| {
                    Ok(Command::Add(i32::try_from(a.integer("by")?).map_err(
                        |_| Refused::new("too_big", "That is too big."),
                    )?))
                },
            )
            .reply(|m: &Model| json!({ "count": m.count })),
        );
        table.add(
            Intent::new(
                Action::new("fetch", "Fetch the weather, and answer once it has arrived"),
                |_| Ok(Command::Fetch),
            )
            .settles(Settle::Explicit {
                within: Duration::from_secs(5),
            })
            .reply(|m: &Model| json!({ "fetched": m.fetched })),
        );
        table.add(
            Intent::new(
                Action::new("reset", "Set the count back to zero").risk("sensitive"),
                |_| Ok(Command::Reset),
            )
            .agents(Agents::Refused("An agent may count but not reset.")),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yantrik_testing::{Call, TestApp};

    #[test]
    fn the_button_the_mind_and_the_model_agree() {
        let app = TestApp::<Hello>::start();
        app.ui(|ui| ui.invoke_add_one());
        app.run(Command::Add(2));
        let answer = app.act("add", json!({"by": 3})).unwrap();
        assert_eq!(answer.envelope()["result"], json!({"count": 6}));
        assert_eq!(app.model(|m| m.count), 6);
        assert_eq!(app.ui(|ui| ui.get_count()), 6);
        assert_eq!(app.describe()["state"]["count"], json!(6));
    }

    #[test]
    fn an_overflow_is_refused_and_changes_nothing() {
        let app = TestApp::<Hello>::start();
        app.run(Command::Add(i32::MAX));
        let e = app.act("add", json!({"by": 1})).unwrap_err();
        assert_eq!(e, "That would overflow the counter.");
        assert_eq!(app.model(|m| m.count), i32::MAX);
    }

    #[test]
    fn a_fetch_is_answered_when_its_job_lands() {
        let app = TestApp::<Hello>::start();
        let pending = app.act("fetch", json!({})).unwrap();
        assert_eq!(
            app.ui(|ui| ui.get_fetched()),
            "",
            "nothing until the job runs"
        );
        app.run_jobs();
        assert_eq!(
            pending.settle().unwrap()["fetched"],
            json!("the weather is fine")
        );
        assert_eq!(app.ui(|ui| ui.get_fetched()), "the weather is fine");
    }

    #[test]
    fn an_agent_may_not_reset_but_the_person_may() {
        let app = TestApp::<Hello>::start();
        let e = app
            .act_as(Call::new("reset", json!({})).as_agent("agent-token"))
            .unwrap_err();
        assert_eq!(e, "An agent may count but not reset.");
        assert!(app.act("reset", json!({})).is_ok());
    }

    #[test]
    fn the_tick_is_a_command_on_the_clock() {
        let app = TestApp::<Hello>::start();
        app.advance(Duration::from_secs(13));
        assert_eq!(app.model(|m| m.ticks), 3);
    }
}
