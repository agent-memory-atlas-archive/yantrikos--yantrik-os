//! The toy app on YantrikApp (design/app-framework-2026-10-21.md): what every app is made of,
//! small enough to read in one sitting.
//!
//! - the model is plain Rust, changed only by `update`;
//! - the button and the Mind's `add` are the same command, `Add`;
//! - `Fetch` starts a job off the UI thread, and `Fetched` carries its result back; the Mind's
//!   `fetch` is answered when it lands;
//! - a timer ticks every four seconds through the same path;
//! - the step the button adds is a setting: saved through `fx.save_settings`, read back at the
//!   next start, adopted when the file is edited outside, held when the file is unusable;
//! - `render` is the only code that writes the window, `describe` the only view the Mind reads.
//!
//! `main.rs` is three lines. The tests run it headless through `yantrik-testing`.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;
use yantrik_app::app::{App, Boot, Changed, Event, Startup, WindowId};
use yantrik_app::control::{Action, Param, View};
use yantrik_app::cx::Cx;
use yantrik_app::effects::Effects;
use yantrik_app::intents::{Agents, Intent, Intents, Settle};
use yantrik_app::jobs::{Concurrency, JobKey, Scope};
use yantrik_app::refused::Refused;
use yantrik_app::settings::{Health, Settings};
use yantrik_app::timers::TimerId;

slint::include_modules!();
yantrik_app::window!(HelloApp);

pub const PART_COUNT: &str = "count";
pub const PART_FETCH: &str = "fetch";
pub const PART_NOTE: &str = "note";
pub const TICK: TimerId = TimerId("hello.tick");

/// What Hello remembers between runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Prefs {
    /// How much the button adds.
    pub step: i32,
}

impl Default for Prefs {
    fn default() -> Prefs {
        Prefs { step: 1 }
    }
}

impl Settings for Prefs {
    const SCHEMA: u32 = 1;
}

#[derive(Debug, Default)]
pub struct Model {
    pub count: i32,
    pub fetched: Option<String>,
    pub ticks: u32,
    pub prefs: Prefs,
    /// A line about the settings the person should see, if there is one.
    pub note: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Command {
    Add(i32),
    /// The button: adds the step.
    AddStep,
    Fetch,
    Fetched(Result<String, String>),
    Tick,
    /// A new step, saved.
    SetStep(i32),
    /// The step as the file says now (edited outside this app).
    StepFromFile(Prefs),
    /// The settings file could not be saved, or could; the note says so.
    Note(Option<String>),
}

pub struct Hello;

impl App for Hello {
    type Ui = HelloApp;
    type Model = Model;
    type Command = Command;
    type Settings = Prefs;
    const ID: &'static str = "hello-app";

    fn model(boot: &Boot<Prefs>) -> Result<Model, Startup> {
        let note = match &boot.settings_health {
            Health::Fine => None,
            Health::Recovery(e) => Some(format!("{e}; using the defaults")),
        };
        Ok(Model {
            prefs: boot.settings.clone(),
            note,
            ..Model::default()
        })
    }

    fn wire(cx: &Cx<Self>) {
        cx.ui().on_add_one(cx.cmd(|| Command::AddStep));
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
            Command::AddStep => fx.then(Command::Add(m.prefs.step)),
            Command::SetStep(step) => {
                if step == 0 {
                    return Err(Refused::new("zero", "A step of zero would add nothing."));
                }
                m.prefs.step = step;
                fx.save_settings(m.prefs.clone());
            }
            Command::StepFromFile(prefs) => m.prefs = prefs,
            Command::Note(note) => {
                m.note = note;
                fx.changed(PART_NOTE);
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
        if changed.has(PART_NOTE) {
            ui.set_notice(m.note.clone().unwrap_or_default().into());
        }
    }

    fn describe(m: &Model) -> View {
        View::new(format!("Hello — count {}", m.count))
            .with("count", m.count)
            .with("step", m.prefs.step)
            .with("fetched", m.fetched.clone().unwrap_or_default())
    }

    fn event(event: Event<Prefs>, _: &Model) -> Option<Command> {
        match event {
            Event::SettingsChangedOutside(prefs) => Some(Command::StepFromFile(prefs)),
            Event::SettingsNotSaved(why) => Some(Command::Note(Some(why))),
            Event::SettingsSaved => Some(Command::Note(None)),
            // A conflict keeps this app's step (the default); a close closes.
            _ => None,
        }
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
                |_| Ok(Command::Add(i32::MIN / 2)),
            )
            .agents(Agents::Refused("An agent may count but not reset.")),
        );
        table.add(
            Intent::new(
                Action::new("set_step", "Set how much the button adds; remembered")
                    .arg(Param::integer("step").describe("The new step")),
                |a| {
                    Ok(Command::SetStep(i32::try_from(a.integer("step")?).map_err(
                        |_| Refused::new("too_big", "That is too big."),
                    )?))
                },
            )
            .reply(|m: &Model| json!({ "step": m.prefs.step })),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yantrik_app::paths::Paths;
    use yantrik_app::watch::Seen;
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

    #[test]
    fn a_setting_is_the_apps_at_once_and_on_disk_after_the_write() {
        let app = TestApp::<Hello>::start();
        let file = app.paths().config_file().to_path_buf();
        assert!(!file.exists(), "nothing is written until something is saved");
        assert_eq!(app.act("set_step", json!({"step": 5})).unwrap().envelope()["result"], json!({"step": 5}));
        app.ui(|ui| ui.invoke_add_one());
        assert_eq!(app.model(|m| m.count), 5, "the button adds the new step before the write lands");
        assert!(app.settings_pending());
        app.run_writes();
        assert!(!app.settings_pending());
        assert_eq!(app.committed_settings(), Prefs { step: 5 });
        let text = std::fs::read_to_string(&file).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["schema"], json!(1));
        assert_eq!(v["settings"]["step"], json!(5));
        assert_eq!(app.model(|m| m.note.clone()), None);
    }

    #[test]
    fn a_setting_survives_a_restart() {
        let paths = Paths::temporary(Hello::ID);
        {
            let app = TestApp::<Hello>::start_at(paths.clone(), Vec::new());
            app.run(Command::SetStep(7));
            // No `run_writes`: shutdown does the writes it owes, as the runner's would.
            assert!(app.shutdown());
        }
        let app = TestApp::<Hello>::start_at(paths, Vec::new());
        assert_eq!(app.model(|m| m.prefs.step), 7);
        assert_eq!(app.describe()["state"]["step"], json!(7));
    }

    #[test]
    fn an_edit_outside_the_app_is_adopted() {
        let app = TestApp::<Hello>::start();
        let file = app.paths().config_file().to_path_buf();
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, r#"{"schema":1,"settings":{"step":3}}"#).unwrap();
        app.seen(Seen::SettingsFile);
        assert_eq!(app.model(|m| m.prefs.step), 3);
        assert_eq!(app.committed_settings(), Prefs { step: 3 });
    }

    #[test]
    fn an_unusable_file_is_not_overwritten_until_the_person_says_so() {
        let paths = Paths::temporary(Hello::ID);
        let file = paths.config_file().to_path_buf();
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "{not json").unwrap();
        let app = TestApp::<Hello>::start_at(paths, Vec::new());
        assert_eq!(app.model(|m| m.prefs.step), 1, "the defaults");
        assert!(app.model(|m| m.note.clone()).unwrap().contains("not in a shape"));
        app.run(Command::SetStep(9));
        app.run_writes();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "{not json", "held, not overwritten");
        assert_eq!(app.model(|m| m.prefs.step), 9, "the app runs on the new value regardless");
        assert!(app.model(|m| m.note.clone()).unwrap().contains("kept until the file is replaced"));
    }

    #[test]
    fn the_theme_follows_the_shell_while_the_app_is_open() {
        use yantrik_app::theme::ThemeSettings;
        let app = TestApp::<Hello>::start();
        app.seen(Seen::Theme(ThemeSettings { dark: false, accent_index: 2 }));
        assert!(!app.ui(|ui| ui.global::<ThemeMode>().get_dark()));
        assert_eq!(app.ui(|ui| ui.global::<AccentPreset>().get_index()), 2);
    }
}
