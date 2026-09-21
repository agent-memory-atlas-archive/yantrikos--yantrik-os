//! Where this machine is, as the desktop already knows it.
//!
//! Asked "what is the temperature outside?", a mind described this service, read "no place set
//! yet", and asked the person where they were. That was the honest thing to do with what it had
//! been shown — and the wrong outcome, because the machine knew. The shell keeps the machine's
//! place in `~/.config/yantrik/settings.yaml` (detected at first run or chosen in Settings: city,
//! coordinates, timezone), puts it in every turn's context, and shows it on the desktop. This
//! service, stateless by design, never looked, so the one question the weather exists to answer
//! came back as a question.
//!
//! The shell stays the owner of that setting. This reads it, and only when nothing has asked
//! about anywhere else this session: a place the person picked in the Weather app, or one a mind
//! set with `set_location`, always wins. Nothing is written here.

use serde::Deserialize;
use yantrik_ipc_contracts::weather::Location;

/// A place to report on when nobody has named one, and how to report it.
#[derive(Debug, Clone)]
pub struct MachinePlace {
    pub location: Location,
    pub fahrenheit: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Settings {
    place: Place,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Place {
    city: String,
    region: String,
    country: String,
    lat: f64,
    lon: f64,
}

/// The machine's place out of the text of `settings.yaml`, if it holds one.
///
/// A place with no coordinates is no place: `0,0` is a point in the Gulf of Guinea, and a
/// forecast for it would be an invented answer wearing a real number.
pub fn from_settings(yaml: &str) -> Option<MachinePlace> {
    let settings: Settings = serde_yaml::from_str(yaml).ok()?;
    let p = settings.place;
    if p.lat == 0.0 && p.lon == 0.0 {
        return None;
    }
    if !(-90.0..=90.0).contains(&p.lat) || !(-180.0..=180.0).contains(&p.lon) {
        return None;
    }
    let name = match (p.city.trim(), p.region.trim()) {
        ("", "") => format!("{:.2}, {:.2}", p.lat, p.lon),
        (city, "") => city.to_string(),
        ("", region) => region.to_string(),
        (city, region) => format!("{city}, {region}"),
    };
    Some(MachinePlace {
        location: Location { name, lat: p.lat, lon: p.lon },
        fahrenheit: uses_fahrenheit(&p.country),
    })
}

/// The handful of countries where a forecast in Celsius has to be converted in the reader's head.
fn uses_fahrenheit(country: &str) -> bool {
    matches!(
        country.trim().to_lowercase().as_str(),
        "united states" | "united states of america" | "usa" | "us" | "bahamas" | "belize"
            | "cayman islands" | "liberia" | "palau" | "marshall islands"
            | "federated states of micronesia"
    )
}

/// Read it from where the shell keeps it.
pub fn read() -> Option<MachinePlace> {
    let home = std::env::var_os("HOME")?;
    let path = std::path::Path::new(&home).join(".config/yantrik/settings.yaml");
    from_settings(&std::fs::read_to_string(path).ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const AS_THE_SHELL_WRITES_IT: &str = "theme: dark\nuser_name: Alex\nplace:\n  city: Bentonville\n  region: Arkansas\n  country: United States\n  lat: 36.3728\n  lon: -94.2088\n  timezone: America/Chicago\n  source: detected\nwallpaper: serenity\n";

    #[test]
    fn the_machines_place_is_read_from_the_shells_settings() {
        let place = from_settings(AS_THE_SHELL_WRITES_IT).expect("a place");
        assert_eq!(place.location.name, "Bentonville, Arkansas");
        assert!((place.location.lat - 36.3728).abs() < 1e-6);
        assert!((place.location.lon + 94.2088).abs() < 1e-6);
        assert!(place.fahrenheit, "a forecast for Arkansas is read in Fahrenheit");
    }

    #[test]
    fn a_place_elsewhere_is_reported_in_celsius() {
        let yaml = "place:\n  city: Pune\n  country: India\n  lat: 18.52\n  lon: 73.86\n";
        let place = from_settings(yaml).expect("a place");
        assert_eq!(place.location.name, "Pune");
        assert!(!place.fahrenheit);
    }

    #[test]
    fn settings_with_no_place_are_not_a_place_in_the_gulf_of_guinea() {
        assert!(from_settings("theme: dark\n").is_none());
        assert!(from_settings("place:\n  city: Nowhere\n").is_none());
        assert!(from_settings("place:\n  lat: 0\n  lon: 0\n").is_none());
    }

    #[test]
    fn coordinates_off_the_planet_and_files_that_are_not_yaml_are_no_place() {
        assert!(from_settings("place:\n  lat: 412.0\n  lon: 7.0\n").is_none());
        assert!(from_settings(":\n\t- not yaml {").is_none());
        assert!(from_settings("").is_none());
    }

    #[test]
    fn coordinates_without_a_name_are_named_by_their_coordinates() {
        let place = from_settings("place:\n  lat: 48.85\n  lon: 2.35\n").expect("a place");
        assert_eq!(place.location.name, "48.85, 2.35");
    }
}
