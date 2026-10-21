//! The platform's services, as the runner hands them to an app (design §2.2 step 6, §5): one
//! typed client per domain, behind its trait, so the app's jobs call `boot.services.weather`
//! and a test's app calls a fake. Each domain joins here as the first app on the framework
//! needs it; the manifest's entitlements will configure them (§3.2: configuration, not a
//! security boundary).

use std::sync::Arc;

use yantrik_services::weather::{FakeWeather, Weather, WeatherApi};

/// What an app may call. Cloning shares the clients.
#[derive(Clone)]
pub struct Services {
    pub weather: Arc<dyn WeatherApi>,
}

impl Services {
    /// The live services, over the socket bus.
    pub fn live() -> Services {
        Services {
            weather: Arc::new(Weather),
        }
    }

    /// Fakes that know nothing and answer "unreachable", for a test that does not set its own.
    pub fn fakes() -> Services {
        Services {
            weather: Arc::new(FakeWeather::new()),
        }
    }

    /// These services, with this weather client.
    pub fn with_weather(mut self, weather: Arc<dyn WeatherApi>) -> Services {
        self.weather = weather;
        self
    }
}

impl std::fmt::Debug for Services {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Services").finish_non_exhaustive()
    }
}
