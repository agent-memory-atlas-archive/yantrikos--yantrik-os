//! Yantrik Weather, on YantrikApp (design/app-framework-2026-10-21.md, step 6: behaviour
//! unchanged).
//!
//! The forecast comes from the weather service through `yantrik_services` (one `forecast`
//! call), or straight from Open-Meteo when the service does not answer; places and the unit
//! live in the framework's settings store at the file the app always used; the agent rail is the
//! framework's; the four actions are intents, graded as they were; and `main.rs` is three lines.

use std::time::Duration;

use yantrik_app::app::{App, Boot, Changed, Event, Startup, WindowId};
use yantrik_app::control::View;
use yantrik_app::cx::Cx;
use yantrik_app::effects::Effects;
use yantrik_app::intents::Intents;
use yantrik_app::rail::{Rail, RailCommand};
use yantrik_app::timers::TimerId;
use yantrik_app::Refused;

mod describe;
mod direct;
mod forecast;
mod geocode;
mod intents;
mod model;
mod places;
mod prefs;
mod rail;
mod update;
mod view;

pub use forecast::{Reading, Source};
pub use model::{Command, Model};
pub use prefs::{Prefs, SavedLocation};

slint::include_modules!();
yantrik_app::window!(WeatherApp);
yantrik_app::rail_window!(WeatherApp);

/// The reading is fetched again this often.
pub const REFETCH: TimerId = TimerId("weather.refetch");
pub const REFETCH_EVERY: Duration = Duration::from_secs(30 * 60);
/// The "Updated … ago" corner is re-said this often.
pub const UPDATED_TICK: TimerId = TimerId("weather.updated");
pub const UPDATED_EVERY: Duration = Duration::from_secs(30);

pub struct Weather;

impl App for Weather {
    type Ui = WeatherApp;
    type Model = Model;
    type Command = Command;
    type Settings = Prefs;
    const ID: &'static str = "weather";

    fn model(boot: &Boot<Prefs>) -> Result<Model, Startup> {
        Ok(Model {
            prefs: boot.settings.clone().normalised(),
            reading: Reading::default(),
            loading: false,
            source: Source::Nothing,
            degraded: None,
            notice: match &boot.settings_health {
                yantrik_app::Health::Fine => String::new(),
                yantrik_app::Health::Recovery(e) => {
                    format!("The saved places could not be read ({e}); using the defaults.")
                }
            },
            last_fetch: None,
            ai: Default::default(),
            rail: Rail::new(boot.mind.clone()),
            weather: boot.services.weather.clone(),
            mind: boot.mind.clone(),
            fall_back: !cfg!(test),
            config_file: boot.paths.config_file().display().to_string(),
        })
    }

    fn wire(cx: &Cx<Self>) {
        let ui = cx.ui();
        ui.on_refresh_pressed(cx.cmd(|| Command::Refresh));
        ui.on_weather_select_location(cx.cmd1(|idx: i32| Command::SelectLocation(idx.max(0) as usize)));
        ui.on_weather_add_location(cx.cmd1(|name: slint::SharedString| Command::AddLocation(name.to_string())));
        ui.on_weather_remove_location(cx.cmd1(|idx: i32| Command::RemoveLocation(idx.max(0) as usize)));
        // The switch has already flipped the in-out property; the command keeps or reverts it.
        let toggled = cx.clone();
        ui.on_weather_toggle_units(move || {
            let want = toggled.ui().get_weather_use_fahrenheit();
            toggled.run(Command::SetUnits(want));
        });
        ui.on_ai_explain_pressed(cx.cmd(|| Command::Explain));
        ui.on_ai_dismiss(cx.cmd(|| Command::DismissAi));
        // No `back-pressed`: there is no parent screen in a window of its own. The rail's
        // context rows are plain rows, not buttons (`context-opens` stays false).
        drop(ui);
        yantrik_app::rail::wire(cx);
    }

    fn start(_: &Model, fx: &mut Effects<Self>) {
        fx.every(REFETCH, REFETCH_EVERY, Command::Refresh);
        fx.every(UPDATED_TICK, UPDATED_EVERY, Command::TickUpdated);
        fx.then(Command::Refresh);
    }

    fn update(m: &mut Model, command: Command, fx: &mut Effects<Self>) -> Result<(), Refused> {
        update::update(m, command, fx)
    }

    fn render(ui: &WeatherApp, _: WindowId, m: &Model, changed: &Changed) {
        view::render(ui, m, changed);
    }

    fn describe(m: &Model) -> View {
        describe::view(m)
    }

    fn intents(table: &mut Intents<Self>) {
        intents::table(table);
    }

    fn event(event: Event<Prefs>, _: &Model) -> Option<Command> {
        match event {
            Event::SettingsChangedOutside(prefs) => Some(Command::PrefsFromFile(prefs)),
            Event::SettingsNotSaved(why) => Some(Command::Note(format!("Could not keep the change: {why}"))),
            _ => None,
        }
    }

    fn rail(command: RailCommand) -> Option<Command> {
        Some(Command::Rail(command))
    }
}

#[cfg(test)]
mod dead_control_tests {
    //! Nothing drawn does nothing (the repo's rule for every control).

    #[test]
    fn the_back_arrow_is_not_drawn_with_nowhere_to_go() {
        assert!(include_str!("../ui/app.slint").contains("show-back: false;"));
        assert!(!include_str!("lib.rs").split("#[cfg(test)]").next().unwrap().contains("on_back_pressed"));
    }

    #[test]
    fn the_rail_rows_are_not_offered_as_buttons() {
        assert!(!include_str!("../ui/weather.slint").contains("context-opens: true"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;
    use yantrik_app::mind::fake::FakeMind;
    use yantrik_app::mind::Reach;
    use yantrik_app::watch::Seen;
    use yantrik_app::{Paths, Services};
    use yantrik_ipc_contracts::weather::Location;
    use yantrik_services::weather::FakeWeather;
    use yantrik_services::ServiceError;
    use yantrik_testing::TestApp;

    fn forecast_at(temperature: f64) -> yantrik_ipc_contracts::weather::Forecast {
        let mut f = FakeWeather::empty_forecast();
        f.current.temperature = temperature;
        f.current.feels_like = temperature - 2.0;
        f.current.condition = "Overcast".into();
        f
    }

    fn fakes(weather: FakeWeather) -> (Arc<FakeWeather>, Arc<FakeMind>, Services) {
        let weather = Arc::new(weather);
        let mind = Arc::new(FakeMind::new());
        let services = Services::fakes().with_weather(weather.clone());
        (weather, mind, services)
    }

    fn start(weather: FakeWeather) -> (TestApp<Weather>, Arc<FakeWeather>, Arc<FakeMind>) {
        let (weather, mind, services) = fakes(weather);
        let app = TestApp::<Weather>::start_at_with(Paths::temporary(Weather::ID), Vec::new(), mind.clone(), services);
        (app, weather, mind)
    }

    #[test]
    fn a_refresh_reads_the_service_and_describe_says_what_the_screen_shows() {
        let (app, weather, _) = start(FakeWeather::new().forecasting(Ok(forecast_at(21.4))));
        assert!(app.model(|m| m.loading), "the first fetch starts at start");
        assert_eq!(app.describe()["summary"], json!("Weather — loading London"));
        app.run_jobs();
        assert_eq!(weather.asked().len(), 1);
        assert_eq!(app.ui(|ui| ui.get_current().temperature), "21°C");
        assert!(!app.ui(|ui| ui.get_current().is_loading));
        let state = app.describe()["state"].clone();
        assert_eq!(state["temperature"], json!("21°C"));
        assert_eq!(state["location"], json!("London"));
        assert_eq!(state["units"], json!("celsius"));
        assert_eq!(state["reading_from"], json!("weather-service"));
        assert_eq!(state["saved_locations"], json!([{ "name": "London", "active": true }]));
        assert!(state["config_file"].as_str().unwrap().ends_with("weather.json"));
        assert_eq!(app.describe()["summary"], json!("Weather — 21°C in London, Overcast, feels like 19°C"));
        assert_eq!(app.ui(|ui| ui.get_hourly_source()), "Open-Meteo · London");
        assert_eq!(app.ui(|ui| ui.get_weather_last_updated()), "Updated just now");
    }

    #[test]
    fn a_stale_reading_is_dropped_and_a_fetch_that_ends_without_one_says_so() {
        let (app, _, _) = start(FakeWeather::new().forecasting(Ok(forecast_at(10.0))));
        // A second refresh replaces the first; the first's result never lands.
        app.run(Command::Refresh);
        app.run_jobs();
        assert_eq!(app.ui(|ui| ui.get_current().temperature), "10°C");
        // A reading for another unit than the one chosen is stale, whatever delivered it.
        let fetched = crate::forecast::Fetched {
            reading: crate::forecast::from_forecast(forecast_at(30.0), "London", true),
            source: Source::Service,
            degraded: None,
        };
        app.run(Command::Fetched { place: SavedLocation::london(), fahrenheit: true, fetched });
        assert_eq!(app.ui(|ui| ui.get_current().temperature), "10°C", "dropped");
        // A fetch whose worker ended without a result clears the loading state and says so.
        app.run(Command::Refresh);
        app.run(Command::FetchFailed("The fetch did not finish; try again.".into()));
        assert!(!app.model(|m| m.loading));
        assert_eq!(app.describe()["summary"], json!("Weather — The fetch did not finish; try again."));
    }

    #[test]
    fn a_service_that_does_not_answer_is_said_and_not_hidden() {
        // No direct fallback under test: the failure is the reading's.
        let (app, _, _) = start(FakeWeather::new().forecasting(Err(ServiceError::Unreachable { service: "weather" })));
        app.run_jobs();
        assert_eq!(app.describe()["state"]["error"], json!("the weather service is not running"));
        assert_eq!(app.ui(|ui| ui.get_current().error_text), "the weather service is not running");
    }

    #[test]
    fn places_and_units_are_kept_in_the_settings_file_and_answer_the_intents() {
        let (app, _, _) = start(
            FakeWeather::new()
                .forecasting(Ok(forecast_at(5.0)))
                .knowing([Location { name: "Paris, France".into(), lat: 48.85, lon: 2.35 }]),
        );
        app.run_jobs();
        // add_location looks the place up in a job and answers when it is saved.
        let pending = app.act("add_location", json!({ "name": "par" })).unwrap();
        assert!(pending.is_owed());
        app.run_jobs();
        let reply = pending.settle().unwrap();
        assert_eq!(reply["added"], json!("Paris, France"));
        assert_eq!(reply["saved_locations"], json!(2));
        assert!(app.settings_pending());
        app.run_writes();
        let file = std::fs::read_to_string(app.paths().config_file()).unwrap();
        assert!(file.contains("Paris, France") && file.contains("\"schema\""));
        // The same place again is refused, in the card's words, and the notice says so.
        let pending = app.act("add_location", json!({ "name": "paris" })).unwrap();
        app.run_jobs();
        let e = pending.settle().unwrap_err();
        assert!(e.contains("already saved"), "{e}");
        assert!(app.model(|m| m.notice.contains("Could not add")));
        // Units, then showing a place by name.
        let reply = app.act("set_units", json!({ "units": "F" })).unwrap();
        assert_eq!(reply.envelope()["result"]["units"], json!("fahrenheit"));
        assert!(app.act("set_units", json!({ "units": "kelvin" })).unwrap_err().contains("not `kelvin`"));
        let reply = app.act("show_location", json!({ "name": "lon" })).unwrap();
        assert_eq!(reply.envelope()["result"]["showing"], json!("London"));
        assert!(app.act("show_location", json!({ "name": "berlin" })).unwrap_err().contains("there is: London, Paris, France"));
        // The last place cannot be removed.
        app.run(Command::RemoveLocation(1));
        app.run(Command::RemoveLocation(0));
        assert_eq!(app.last_refusal().unwrap().kind, "last_place");
        assert_eq!(app.model(|m| m.prefs.locations.len()), 1);
    }

    #[test]
    fn the_file_from_before_the_framework_is_read_and_kept() {
        let paths = Paths::temporary(Weather::ID);
        std::fs::create_dir_all(paths.config_file().parent().unwrap()).unwrap();
        std::fs::write(
            paths.config_file(),
            r#"{"fahrenheit":true,"active":1,"locations":[{"name":"London","lat":51.5,"lon":-0.1},{"name":"Oslo","lat":59.9,"lon":10.7}]}"#,
        )
        .unwrap();
        let (_, mind, services) = fakes(FakeWeather::new().forecasting(Ok(forecast_at(1.0))));
        let app = TestApp::<Weather>::start_at_with(paths.clone(), Vec::new(), mind, services);
        assert!(app.model(|m| m.prefs.fahrenheit));
        assert_eq!(app.model(|m| m.active().name), "Oslo");
        assert_eq!(app.model(|m| m.notice.clone()), "", "an import is not recovery");
        app.run_writes();
        assert!(std::fs::read_to_string(paths.config_file()).unwrap().contains("\"schema\""));
        assert!(paths.config_file().with_file_name("weather.json.legacy.json").exists());
    }

    #[test]
    fn the_rail_offers_the_one_question_once_there_is_a_reading() {
        let (app, _, mind) = start(FakeWeather::new().forecasting(Ok(forecast_at(12.0))));
        mind.set_reach(Reach::Ready);
        app.seen(Seen::Reach(Reach::Ready));
        assert_eq!(app.model(|m| m.rail.suggestions().len()), 0, "no reading yet");
        app.run_jobs();
        app.advance(Duration::from_millis(300));
        assert_eq!(app.model(|m| m.rail.suggestions().len()), 1);
        assert_eq!(app.model(|m| m.rail.context()[0].label.clone()), "12°C, Overcast");
        app.ui(|ui| ui.invoke_agent_suggestion_activated("advise".into()));
        app.run_jobs();
        let card = app.model(|m| m.rail.proposal().cloned().unwrap());
        assert_eq!(card.verb, "Close");
        assert!(card.applies.is_none(), "an answer to read, nothing to apply");
        assert!(mind.prompts()[0].starts_with("It is Overcast at 12°C"));
        // The header's AI Insights button asks the same question.
        app.run(Command::Explain);
        app.run_jobs();
        assert_eq!(mind.prompts().len(), 2);
        assert_eq!(mind.prompts()[1], mind.prompts()[0]);
    }
}
