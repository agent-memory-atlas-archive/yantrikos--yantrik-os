//! Weather service — standalone process exposing Open-Meteo data via JSON-RPC.
//!
//! Methods:
//!   weather.current   { lat, lon, fahrenheit? }  → CurrentWeather
//!   weather.hourly    { lat, lon, hours?, fahrenheit? }  → Vec<HourlyForecast>
//!   weather.daily     { lat, lon, days?, fahrenheit? }   → Vec<DailyForecast>
//!   weather.alerts    { lat, lon, fahrenheit? }  → Vec<WeatherAlert>
//!   weather.air_quality { lat, lon }             → AirQuality
//!   weather.geocode   { query }                  → Location
//!   weather.suggest   { query }                  → Vec<LocationSuggestion>

mod machine_place;

use std::sync::Mutex;
use yantrik_ipc_contracts::control_surface::{act_json, describe_json, Action, Param, View};
use yantrik_ipc_contracts::weather::*;
use yantrik_service_sdk::prelude::*;

fn main() {
    ServiceBuilder::new("weather")
        .handler(WeatherHandler::default())
        .run();
}

/// The place a describe should report on, and the unit to report it in.
///
/// The service is otherwise stateless — every data call carries its own lat/lon — but
/// `app.describe {}` carries nothing, and "the weather" with no place is not an answer. So the
/// service remembers the last place it was asked about (the shell queries it for the status bar
/// and the weather app), and describe reports on that. It is the weather the user is actually
/// looking at, not a guess.
#[derive(Clone)]
struct LastPlace {
    location: Location,
    fahrenheit: bool,
}

#[derive(Default)]
struct WeatherHandler {
    last_place: Mutex<Option<LastPlace>>,
}

impl WeatherHandler {
    /// Record the place a data call was about, so a later describe has somewhere to report on.
    fn remember(&self, location: &Location, fahrenheit: bool) {
        if let Ok(mut guard) = self.last_place.lock() {
            *guard = Some(LastPlace { location: location.clone(), fahrenheit });
        }
    }
}

impl ServiceHandler for WeatherHandler {
    fn service_id(&self) -> &str {
        "weather"
    }

    fn handle(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, ServiceError> {
        match method {
            "weather.current" => {
                let loc = parse_location(&params)?;
                let fahrenheit = params["fahrenheit"].as_bool().unwrap_or(false);
                self.remember(&loc, fahrenheit);
                let result = fetch_current(&loc, fahrenheit)?;
                Ok(serde_json::to_value(result).unwrap())
            }
            "weather.hourly" => {
                let loc = parse_location(&params)?;
                let hours = params["hours"].as_u64().unwrap_or(24) as u32;
                let fahrenheit = params["fahrenheit"].as_bool().unwrap_or(false);
                self.remember(&loc, fahrenheit);
                let result = fetch_hourly(&loc, hours, fahrenheit)?;
                Ok(serde_json::to_value(result).unwrap())
            }
            "weather.daily" => {
                let loc = parse_location(&params)?;
                let days = params["days"].as_u64().unwrap_or(5) as u32;
                let fahrenheit = params["fahrenheit"].as_bool().unwrap_or(false);
                self.remember(&loc, fahrenheit);
                let result = fetch_daily(&loc, days, fahrenheit)?;
                Ok(serde_json::to_value(result).unwrap())
            }
            "weather.alerts" => {
                let loc = parse_location(&params)?;
                let fahrenheit = params["fahrenheit"].as_bool().unwrap_or(false);
                let result = fetch_alerts(&loc, fahrenheit)?;
                Ok(serde_json::to_value(result).unwrap())
            }
            "weather.air_quality" => {
                let loc = parse_location(&params)?;
                let result = fetch_air_quality(&loc)?;
                Ok(serde_json::to_value(result).unwrap())
            }
            "weather.suggest" => {
                let query = params["query"]
                    .as_str()
                    .ok_or_else(|| ServiceError {
                        code: -32602,
                        message: "Missing 'query' parameter".to_string(),
                    })?;
                let result = suggest(query, 5)?;
                Ok(serde_json::to_value(result).unwrap())
            }
            "weather.geocode" => {
                let query = params["query"]
                    .as_str()
                    .ok_or_else(|| ServiceError {
                        code: -32602,
                        message: "Missing 'query' parameter".to_string(),
                    })?;
                let result = geocode(query)?;
                Ok(serde_json::to_value(result).unwrap())
            }
            // The agent-facing surface: the weather where the user is, in one line and a small
            // state object, without opening the app.
            "app.describe" => Ok(describe_json(
                "weather",
                &self.describe_view(),
                &weather_actions(),
            )),
            "app.act" => self.act(&params),
            _ => Err(ServiceError {
                code: -1,
                message: format!("Unknown method: {method}"),
            }),
        }
    }
}

impl WeatherHandler {
    /// The current weather for the last place asked about, or an honest "nowhere yet".
    fn describe_view(&self) -> View {
        let asked = self.last_place.lock().ok().and_then(|g| g.clone());
        // Nobody has asked about anywhere yet: report on where the machine is, which the desktop
        // already knows. See `machine_place` for the afternoon a mind asked a person where they
        // were while the answer sat in a file beside it.
        let from_machine = asked.is_none();
        let place = asked.or_else(|| {
            machine_place::read().map(|m| LastPlace { location: m.location, fahrenheit: m.fahrenheit })
        });
        let Some(place) = place else {
            // Never queried this session. Say so, and say how to fix it, rather than invent a
            // city — a made-up location is worse than no answer.
            return View::new(
                "Weather — no place set yet, and the desktop has not recorded where this machine is; act set_location with a place name, or ask weather.current with lat/lon",
            )
            .with("has_location", false);
        };

        let unit = if place.fahrenheit { "°F" } else { "°C" };
        match fetch_current(&place.location, place.fahrenheit) {
            Ok(w) => {
                let summary = format!(
                    "Weather — {}°{} {} in {}, feels {}°, humidity {}%, wind {} {}",
                    w.temperature.round() as i64,
                    if place.fahrenheit { "F" } else { "C" },
                    w.condition,
                    place.location.name,
                    w.feels_like.round() as i64,
                    w.humidity,
                    w.wind_speed.round() as i64,
                    w.wind_direction,
                );
                View::new(summary)
                    .with("has_location", true)
                    // Which place this is, so that a reader can tell "the weather where this
                    // machine is" from "the weather somewhere somebody looked up".
                    .with(
                        "location_source",
                        if from_machine { "where this machine is (desktop settings)" } else { "the last place asked about" },
                    )
                    .with("location", place.location.name.clone())
                    .with("lat", place.location.lat)
                    .with("lon", place.location.lon)
                    .with("unit", unit)
                    .with("temperature", (w.temperature * 10.0).round() / 10.0)
                    .with("feels_like", (w.feels_like * 10.0).round() / 10.0)
                    .with("condition", w.condition)
                    .with("humidity", w.humidity as i64)
                    .with("wind_speed", (w.wind_speed * 10.0).round() / 10.0)
                    .with("wind_direction", w.wind_direction)
                    .with("uv_index", w.uv_index)
                    .with("pressure_hpa", w.pressure_hpa)
                    .with("visibility_km", w.visibility_km)
            }
            Err(e) => {
                // The place is known but the fetch failed (offline, upstream down). Report the
                // place and the failure, not a stale number we do not have.
                View::new(format!(
                    "Weather — could not reach the forecast for {} ({})",
                    place.location.name, e.message
                ))
                .with("has_location", true)
                .with("location", place.location.name.clone())
                .with("error", e.message)
            }
        }
    }

    /// Dispatch `app.act`. The one action changes which place describe reports on.
    fn act(&self, params: &serde_json::Value) -> Result<serde_json::Value, ServiceError> {
        let action = params["action"].as_str().unwrap_or("").trim();
        let args = params.get("args").cloned().unwrap_or_else(|| serde_json::json!({}));
        match action {
            "set_location" => {
                // Either a place name to geocode, or an explicit lat/lon for somewhere without a
                // name. A name is what a person types, so it comes first.
                let fahrenheit = args["fahrenheit"].as_bool();
                let location = if let Some(query) = args["query"].as_str() {
                    if query.trim().is_empty() {
                        return Err(ServiceError {
                            code: -32602,
                            message: "`set_location` query is empty".to_string(),
                        });
                    }
                    geocode(query)?
                } else if let (Some(lat), Some(lon)) = (args["lat"].as_f64(), args["lon"].as_f64())
                {
                    let name = args["name"].as_str().unwrap_or("(pinned location)").to_string();
                    Location { name, lat, lon }
                } else {
                    return Err(ServiceError {
                        code: -32602,
                        message: "`set_location` needs `query`, or `lat` and `lon`".to_string(),
                    });
                };
                // Keep the unit the caller last used, unless they overrode it here.
                let unit = fahrenheit.unwrap_or_else(|| {
                    self.last_place
                        .lock()
                        .ok()
                        .and_then(|g| g.as_ref().map(|p| p.fahrenheit))
                        .unwrap_or(false)
                });
                self.remember(&location, unit);
                let view = self.describe_view();
                Ok(act_json(
                    "weather",
                    "weather#act",
                    true,
                    serde_json::json!({ "location": location.name, "lat": location.lat, "lon": location.lon }),
                    &view,
                ))
            }
            "" => Err(ServiceError {
                code: -32602,
                message: "act needs a non-empty `action`".to_string(),
            }),
            other => Err(ServiceError {
                code: -32601,
                message: format!("unknown action `{other}`; this service offers: set_location"),
            }),
        }
    }
}

/// What the weather service can be asked to do. Reading is free (`app.describe`); the one action
/// changes which place is reported, and geocoding a name touches the network, so it is graded
/// `standard`, the floor for anything that reaches outside the process.
fn weather_actions() -> Vec<Action> {
    vec![Action::new("set_location", "Choose the place `describe` reports on")
        .arg(Param::text("query").describe("A place name to look up, e.g. 'Dallas, TX'").optional())
        .arg(Param::number("lat").describe("Latitude, if giving coordinates instead of a name").optional())
        .arg(Param::number("lon").describe("Longitude, if giving coordinates instead of a name").optional())
        .arg(Param::flag("fahrenheit").describe("Report in °F instead of °C").optional())]
}

// ── Parameter parsing ────────────────────────────────────────────────

fn parse_location(params: &serde_json::Value) -> Result<Location, ServiceError> {
    let lat = params["lat"]
        .as_f64()
        .ok_or_else(|| ServiceError {
            code: -32602,
            message: "Missing 'lat' parameter".to_string(),
        })?;
    let lon = params["lon"]
        .as_f64()
        .ok_or_else(|| ServiceError {
            code: -32602,
            message: "Missing 'lon' parameter".to_string(),
        })?;
    let name = params["name"].as_str().unwrap_or("Unknown").to_string();
    Ok(Location { name, lat, lon })
}

// ── Open-Meteo API fetching ──────────────────────────────────────────

fn open_meteo_forecast(
    loc: &Location,
    fahrenheit: bool,
) -> Result<serde_json::Value, ServiceError> {
    let temp_unit = if fahrenheit { "&temperature_unit=fahrenheit" } else { "" };
    let wind_unit = if fahrenheit { "&wind_speed_unit=mph" } else { "" };

    let url = format!(
        "https://api.open-meteo.com/v1/forecast?\
         latitude={}&longitude={}\
         &current=temperature_2m,relative_humidity_2m,apparent_temperature,\
         weather_code,wind_speed_10m,wind_direction_10m,surface_pressure,\
         is_day,cloud_cover,dew_point_2m\
         &hourly=temperature_2m,weather_code,precipitation_probability\
         &daily=weather_code,temperature_2m_max,temperature_2m_min,\
         precipitation_sum,sunrise,sunset,uv_index_max\
         &timezone=auto&forecast_days=7{temp_unit}{wind_unit}",
        loc.lat, loc.lon
    );

    let resp = ureq::get(&url)
        .timeout(std::time::Duration::from_secs(15))
        .call()
        .map_err(|e| ServiceError {
            code: -32000,
            message: format!("Open-Meteo API error: {e}"),
        })?;

    resp.into_json::<serde_json::Value>().map_err(|e| ServiceError {
        code: -32000,
        message: format!("JSON parse error: {e}"),
    })
}

fn fetch_current(loc: &Location, fahrenheit: bool) -> Result<CurrentWeather, ServiceError> {
    let json = open_meteo_forecast(loc, fahrenheit)?;
    let c = &json["current"];
    let d = &json["daily"];

    let weather_code = c["weather_code"].as_i64().unwrap_or(0) as i32;
    let is_day = c["is_day"].as_i64().unwrap_or(1) == 1;
    let wind_degrees = c["wind_direction_10m"].as_f64().unwrap_or(0.0);

    let uv_index = d["uv_index_max"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    Ok(CurrentWeather {
        temperature: c["temperature_2m"].as_f64().unwrap_or(0.0),
        feels_like: c["apparent_temperature"].as_f64().unwrap_or(0.0),
        humidity: c["relative_humidity_2m"].as_i64().unwrap_or(0) as i32,
        wind_speed: c["wind_speed_10m"].as_f64().unwrap_or(0.0),
        wind_direction: wind_direction_str(wind_degrees).to_string(),
        wind_degrees,
        condition: wmo_description(weather_code).to_string(),
        icon: wmo_icon(weather_code, is_day).to_string(),
        uv_index,
        visibility_km: 10.0, // Open-Meteo free tier doesn't provide visibility
        pressure_hpa: c["surface_pressure"].as_f64().unwrap_or(0.0),
        dew_point: c["dew_point_2m"].as_f64().unwrap_or(0.0),
        cloud_cover: c["cloud_cover"].as_i64().unwrap_or(0) as i32,
        is_day,
    })
}

fn fetch_hourly(
    loc: &Location,
    hours: u32,
    fahrenheit: bool,
) -> Result<Vec<HourlyForecast>, ServiceError> {
    let json = open_meteo_forecast(loc, fahrenheit)?;
    let h = &json["hourly"];

    let (times, temps, codes) = match (
        h["time"].as_array(),
        h["temperature_2m"].as_array(),
        h["weather_code"].as_array(),
    ) {
        (Some(t), Some(te), Some(c)) => (t, te, c),
        _ => return Ok(Vec::new()),
    };

    let precip_probs = h["precipitation_probability"].as_array();

    let current_hour = json["current"]["time"]
        .as_str()
        .map(extract_hour)
        .unwrap_or(0);

    let mut result = Vec::new();
    for i in current_hour..times.len().min(current_hour + hours as usize) {
        let time_str = times[i].as_str().unwrap_or("");
        let hour = extract_hour(time_str);
        let code = codes[i].as_i64().unwrap_or(0) as i32;
        let is_day_hour = (6..20).contains(&hour);
        let precip = precip_probs
            .and_then(|p| p.get(i))
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as i32;

        result.push(HourlyForecast {
            time: if i == current_hour {
                "Now".to_string()
            } else {
                format!("{:02}:00", hour)
            },
            temperature: temps[i].as_f64().unwrap_or(0.0),
            condition: wmo_description(code).to_string(),
            icon: wmo_icon(code, is_day_hour).to_string(),
            precipitation_chance: precip,
        });
    }

    Ok(result)
}

fn fetch_daily(
    loc: &Location,
    days: u32,
    fahrenheit: bool,
) -> Result<Vec<DailyForecast>, ServiceError> {
    let json = open_meteo_forecast(loc, fahrenheit)?;
    let d = &json["daily"];

    let (dates, codes, maxes, mins, precips, sunrises, sunsets) = match (
        d["time"].as_array(),
        d["weather_code"].as_array(),
        d["temperature_2m_max"].as_array(),
        d["temperature_2m_min"].as_array(),
        d["precipitation_sum"].as_array(),
        d["sunrise"].as_array(),
        d["sunset"].as_array(),
    ) {
        (Some(a), Some(b), Some(c), Some(d), Some(e), Some(f), Some(g)) => (a, b, c, d, e, f, g),
        _ => return Ok(Vec::new()),
    };

    let mut result = Vec::new();
    for i in 0..dates.len().min(days as usize) {
        let code = codes[i].as_i64().unwrap_or(0) as i32;
        let precip = precips[i].as_f64().unwrap_or(0.0);

        result.push(DailyForecast {
            date: dates[i].as_str().unwrap_or("").to_string(),
            temp_high: maxes[i].as_f64().unwrap_or(0.0),
            temp_low: mins[i].as_f64().unwrap_or(0.0),
            condition: wmo_description(code).to_string(),
            icon: wmo_icon(code, true).to_string(),
            precipitation_chance: if precip > 0.0 { (precip * 10.0) as i32 } else { 0 },
            sunrise: sunrises[i]
                .as_str()
                .map(extract_time)
                .unwrap_or_else(|| "--".to_string()),
            sunset: sunsets[i]
                .as_str()
                .map(extract_time)
                .unwrap_or_else(|| "--".to_string()),
        });
    }

    Ok(result)
}

fn fetch_alerts(loc: &Location, fahrenheit: bool) -> Result<Vec<WeatherAlert>, ServiceError> {
    let json = open_meteo_forecast(loc, fahrenheit)?;
    let c = &json["current"];
    let d = &json["daily"];

    let weather_code = c["weather_code"].as_i64().unwrap_or(0) as i32;
    let wind_speed = c["wind_speed_10m"].as_f64().unwrap_or(0.0);
    let uv_index = d["uv_index_max"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    Ok(generate_alerts(weather_code, wind_speed, uv_index, fahrenheit))
}

fn fetch_air_quality(loc: &Location) -> Result<AirQuality, ServiceError> {
    let url = format!(
        "https://air-quality-api.open-meteo.com/v1/air-quality?\
         latitude={}&longitude={}&current=european_aqi",
        loc.lat, loc.lon
    );

    let resp = ureq::get(&url)
        .timeout(std::time::Duration::from_secs(10))
        .call()
        .map_err(|e| ServiceError {
            code: -32000,
            message: format!("AQI API error: {e}"),
        })?;

    let json: serde_json::Value = resp.into_json().map_err(|e| ServiceError {
        code: -32000,
        message: format!("AQI JSON error: {e}"),
    })?;

    let aqi = json["current"]["european_aqi"]
        .as_f64()
        .unwrap_or(-1.0) as i32;

    if aqi < 0 {
        return Ok(AirQuality {
            value: 0.0,
            label: "N/A".to_string(),
            level: 0,
        });
    }

    let (label, level) = match aqi {
        0..=20 => ("Good", 0),
        21..=40 => ("Fair", 0),
        41..=60 => ("Moderate", 1),
        61..=80 => ("Poor", 1),
        81..=100 => ("Very Poor", 2),
        _ => ("Hazardous", 2),
    };

    Ok(AirQuality {
        value: aqi as f64,
        label: label.to_string(),
        level,
    })
}

/// Top `count` geocoding matches for a city query.
fn geocode_search(query: &str, count: u32) -> Result<Vec<LocationSuggestion>, ServiceError> {
    let encoded = url_encode(query);
    let url = format!(
        "https://geocoding-api.open-meteo.com/v1/search?name={encoded}&count={count}&language=en&format=json"
    );

    let resp = ureq::get(&url)
        .timeout(std::time::Duration::from_secs(10))
        .call()
        .map_err(|e| ServiceError {
            code: -32000,
            message: format!("Geocoding error: {e}"),
        })?;

    let json: serde_json::Value = resp.into_json().map_err(|e| ServiceError {
        code: -32000,
        message: format!("Geocoding JSON error: {e}"),
    })?;

    let results = json["results"].as_array().cloned().unwrap_or_default();
    Ok(results
        .into_iter()
        .map(|r| LocationSuggestion {
            name: r["name"].as_str().unwrap_or("").to_string(),
            lat: r["latitude"].as_f64().unwrap_or(0.0),
            lon: r["longitude"].as_f64().unwrap_or(0.0),
        })
        .collect())
}

/// Suggestion rows for the location search box — the query is echoed so a client can
/// confirm what was matched.
fn suggest(query: &str, count: u32) -> Result<Vec<LocationSuggestion>, ServiceError> {
    geocode_search(query, count)
}

fn geocode(query: &str) -> Result<Location, ServiceError> {
    let first = geocode_search(query, 1)?
        .into_iter()
        .next()
        .ok_or_else(|| ServiceError {
            code: -32000,
            message: format!("Location not found: {query}"),
        })?;

    Ok(Location {
        name: first.name,
        lat: first.lat,
        lon: first.lon,
    })
}

/// Percent-encode the parts of a query that must not appear raw in a URL.
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

// ── Alert generation ─────────────────────────────────────────────────

fn generate_alerts(
    weather_code: i32,
    wind_speed: f64,
    uv_index: f64,
    fahrenheit: bool,
) -> Vec<WeatherAlert> {
    let mut alerts = Vec::new();

    match weather_code {
        95 => alerts.push(WeatherAlert {
            severity: "warning".to_string(),
            title: "Thunderstorm Warning".to_string(),
            description: "Thunderstorm activity in the area. Seek shelter indoors.".to_string(),
            expires: String::new(),
        }),
        96 | 99 => alerts.push(WeatherAlert {
            severity: "emergency".to_string(),
            title: "Severe Thunderstorm".to_string(),
            description: "Thunderstorm with hail expected. Stay indoors.".to_string(),
            expires: String::new(),
        }),
        _ => {}
    }

    match weather_code {
        65 | 82 => alerts.push(WeatherAlert {
            severity: "watch".to_string(),
            title: "Heavy Rain Alert".to_string(),
            description: "Heavy rainfall expected. Potential for localized flooding.".to_string(),
            expires: String::new(),
        }),
        75 | 77 => alerts.push(WeatherAlert {
            severity: "warning".to_string(),
            title: "Heavy Snow Warning".to_string(),
            description: "Heavy snowfall expected. Travel may be hazardous.".to_string(),
            expires: String::new(),
        }),
        56 | 57 | 66 | 67 => alerts.push(WeatherAlert {
            severity: "warning".to_string(),
            title: "Freezing Precipitation".to_string(),
            description: "Freezing rain/drizzle. Icy conditions on roads.".to_string(),
            expires: String::new(),
        }),
        _ => {}
    }

    let wind_threshold = if fahrenheit { 40.0 } else { 60.0 };
    if wind_speed > wind_threshold {
        alerts.push(WeatherAlert {
            severity: "watch".to_string(),
            title: "High Wind Advisory".to_string(),
            description: format!(
                "Wind speeds of {:.0} {}. Secure loose objects.",
                wind_speed,
                if fahrenheit { "mph" } else { "km/h" }
            ),
            expires: String::new(),
        });
    }

    if uv_index >= 8.0 {
        alerts.push(WeatherAlert {
            severity: if uv_index >= 11.0 { "warning" } else { "watch" }.to_string(),
            title: "Very High UV Index".to_string(),
            description: format!("UV index of {uv_index:.0}. Limit sun exposure and wear sunscreen."),
            expires: String::new(),
        });
    }

    if weather_code == 45 || weather_code == 48 {
        alerts.push(WeatherAlert {
            severity: "watch".to_string(),
            title: "Fog Advisory".to_string(),
            description: "Reduced visibility due to fog. Drive with caution.".to_string(),
            expires: String::new(),
        });
    }

    alerts
}

// ── WMO code helpers (moved from wire/weather.rs) ────────────────────

/// Vector icon kind the UI draws for a WMO code. The dashboard maps these to
/// stroke icons (sun / moon / partly / cloud / rain / snow / fog / storm), so
/// rendering is identical on every platform — no emoji-font dependence.
fn wmo_icon(code: i32, is_day: bool) -> &'static str {
    match code {
        0 => if is_day { "sun" } else { "moon" },
        1 | 2 => if is_day { "partly" } else { "moon" },
        3 => "cloud",
        45 | 48 => "fog",
        51 | 53 | 55 => "rain",
        56 | 57 | 66 | 67 => "rain",
        61 | 63 | 65 | 80 | 81 | 82 => "rain",
        71 | 73 | 75 | 77 | 85 | 86 => "snow",
        95 | 96 | 99 => "storm",
        _ => "cloud",
    }
}

fn wmo_description(code: i32) -> &'static str {
    match code {
        0 => "Clear sky",
        1 => "Mainly clear",
        2 => "Partly cloudy",
        3 => "Overcast",
        45 => "Fog",
        48 => "Depositing rime fog",
        51 => "Light drizzle",
        53 => "Moderate drizzle",
        55 => "Dense drizzle",
        56 | 57 => "Freezing drizzle",
        61 => "Slight rain",
        63 => "Moderate rain",
        65 => "Heavy rain",
        66 | 67 => "Freezing rain",
        71 => "Slight snow",
        73 => "Moderate snow",
        75 => "Heavy snow",
        77 => "Snow grains",
        80 => "Slight rain showers",
        81 => "Moderate rain showers",
        82 => "Violent rain showers",
        85 => "Slight snow showers",
        86 => "Heavy snow showers",
        95 => "Thunderstorm",
        96 | 99 => "Thunderstorm with hail",
        _ => "Unknown",
    }
}

fn wind_direction_str(degrees: f64) -> &'static str {
    let dirs = [
        "N", "NNE", "NE", "ENE", "E", "ESE", "SE", "SSE",
        "S", "SSW", "SW", "WSW", "W", "WNW", "NW", "NNW",
    ];
    let idx = ((degrees + 11.25) / 22.5) as usize % 16;
    dirs[idx]
}

fn extract_time(s: &str) -> String {
    if let Some(pos) = s.find('T') {
        s[pos + 1..].to_string()
    } else {
        s.to_string()
    }
}

fn extract_hour(s: &str) -> usize {
    if let Some(pos) = s.find('T') {
        let time_part = &s[pos + 1..];
        time_part
            .split(':')
            .next()
            .and_then(|h| h.parse::<usize>().ok())
            .unwrap_or(0)
    } else {
        0
    }
}
