//! The only code that writes the window: the model, drawn, for the parts that changed.

use slint::{ModelRc, SharedString, VecModel};
use yantrik_app::app::Changed;
use yantrik_app::rail::{self, RailView as _};

use crate::forecast::hourly_source;
use crate::model::{Model, PART_AI, PART_NOTICE, PART_PLACES, PART_READING, PART_UPDATED};
use crate::{WeatherAlert, WeatherApp, WeatherCurrent, WeatherDaily, WeatherHourly, WeatherSavedLocation};

pub fn render(ui: &WeatherApp, m: &Model, changed: &Changed) {
    if changed.has(PART_READING) {
        reading(ui, m);
    }
    if changed.has(PART_PLACES) {
        let active = m.prefs.active;
        let rows: Vec<WeatherSavedLocation> = m
            .prefs
            .locations
            .iter()
            .enumerate()
            .map(|(i, loc)| WeatherSavedLocation {
                name: SharedString::from(&loc.name),
                lat: loc.lat as f32,
                lon: loc.lon as f32,
                is_active: i == active,
            })
            .collect();
        ui.set_weather_saved_locations(ModelRc::new(VecModel::from(rows)));
        if ui.get_weather_use_fahrenheit() != m.prefs.fahrenheit {
            ui.set_weather_use_fahrenheit(m.prefs.fahrenheit);
        }
    }
    if changed.has(PART_NOTICE) {
        ui.set_notice(m.notice.as_str().into());
    }
    if changed.has(PART_UPDATED) {
        ui.set_weather_last_updated(m.last_updated_text().into());
    }
    if changed.has(PART_AI) {
        ui.set_ai_is_working(m.ai.working);
        ui.set_ai_response(m.ai.response.as_str().into());
        if !m.ai.panel_open && ui.get_ai_panel_open() {
            ui.set_ai_panel_open(false);
        }
    }
    if changed.has(rail::PART) {
        ui.render_rail(&m.rail);
    }
}

fn reading(ui: &WeatherApp, m: &Model) {
    let r = &m.reading;
    let current = match &r.current {
        Some(c) => WeatherCurrent {
            temperature: c.temperature.as_str().into(),
            feels_like: c.feels_like.as_str().into(),
            condition: c.condition.as_str().into(),
            icon: c.icon.as_str().into(),
            location: c.location.as_str().into(),
            humidity: c.humidity.as_str().into(),
            wind_speed: c.wind_speed.as_str().into(),
            wind_direction: c.wind_direction.as_str().into(),
            uv_index: c.uv_index.as_str().into(),
            visibility: c.visibility.as_str().into(),
            pressure: c.pressure.as_str().into(),
            cloud_cover: c.cloud_cover.as_str().into(),
            dew_point: c.dew_point.as_str().into(),
            sunrise: c.sunrise.as_str().into(),
            sunset: c.sunset.as_str().into(),
            is_day: c.is_day,
            is_loading: m.loading,
            error_text: r.error.clone().unwrap_or_default().into(),
        },
        None => WeatherCurrent {
            location: m.active().name.as_str().into(),
            is_loading: m.loading,
            error_text: r.error.clone().unwrap_or_default().into(),
            ..ui.get_current()
        },
    };
    let location = current.location.trim().to_string();
    ui.set_hourly_tz_label(if location.is_empty() {
        SharedString::new()
    } else {
        format!("{location} time").into()
    });
    ui.set_hourly_source(hourly_source(&location).into());
    ui.set_current(current);

    if r.current.is_some() {
        let hourly: Vec<WeatherHourly> = r
            .hourly
            .iter()
            .map(|h| WeatherHourly {
                time: h.time.as_str().into(),
                icon: h.icon.as_str().into(),
                temp: h.temp.as_str().into(),
                precip: h.precip.as_str().into(),
                is_current: h.is_current,
                temp_value: h.temp_value,
                t_min: h.t_min,
                t_max: h.t_max,
            })
            .collect();
        ui.set_hourly(ModelRc::new(VecModel::from(hourly)));
        let daily: Vec<WeatherDaily> = r
            .daily
            .iter()
            .map(|d| WeatherDaily {
                day_name: d.day_name.as_str().into(),
                icon: d.icon.as_str().into(),
                high: d.high.as_str().into(),
                low: d.low.as_str().into(),
                precip_chance: d.precip_chance.as_str().into(),
                high_value: d.high_value,
                low_value: d.low_value,
                temp_range_min: d.temp_range_min,
                temp_range_max: d.temp_range_max,
            })
            .collect();
        ui.set_daily(ModelRc::new(VecModel::from(daily)));
        let alerts: Vec<WeatherAlert> = r
            .alerts
            .iter()
            .map(|a| WeatherAlert {
                title: a.title.as_str().into(),
                description: a.description.as_str().into(),
                severity: a.severity,
                icon: a.icon.as_str().into(),
            })
            .collect();
        match r.severe() {
            Some(alert) => {
                ui.set_weather_severe_alert_active(true);
                ui.set_weather_severe_alert_text(alert.description.as_str().into());
                ui.set_weather_severe_alert_level(
                    match alert.severity {
                        0 => "watch",
                        1 => "warning",
                        _ => "emergency",
                    }
                    .into(),
                );
            }
            None => {
                ui.set_weather_severe_alert_active(false);
                ui.set_weather_severe_alert_text(SharedString::default());
                ui.set_weather_severe_alert_level(SharedString::default());
            }
        }
        ui.set_weather_alerts(ModelRc::new(VecModel::from(alerts)));
        ui.set_weather_aqi_value(r.aqi.value.as_str().into());
        ui.set_weather_aqi_label(r.aqi.label.as_str().into());
        ui.set_weather_aqi_level(r.aqi.level);
    }
}
