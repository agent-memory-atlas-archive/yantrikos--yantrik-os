//! `judge:` — a System One model that makes the companion's small decisions instead of the
//! chat model.
//!
//! A judge answers a typed question (which of these tools fits this request?) with a probability
//! for each option, in one pass and without writing text. Asking it which tool a request needs lets
//! the chat model see that one tool's schema instead of a shortlist of twenty, which saves the
//! tokens of every schema left out and stops the chat model picking the wrong one. Anything the
//! judge is unsure of, or any failure to reach it, falls back to the ordinary selection.
//!
//! The judge reads each request and the person's last few messages, never the assistant's
//! replies, with anything shaped like a credential redacted. A cloud judge therefore receives
//! that text; in incognito the companion asks no judge at all.
//!
//! Any server speaking `/v1/systemone` works:
//!
//! ```yaml
//! judge:                                   # TypeSafe's cloud
//!   endpoint: "https://api.typesafe.ai"
//!   model: "jev-latest"
//!   api_key_env: "JEV_API_KEY"
//!
//! judge:                                   # Kev on this machine (kev.serve)
//!   endpoint: "http://127.0.0.1:8009"
//!   model: "kev-latest"
//! ```

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JudgeConfig {
    /// Which decision model answers: `off`, `jev`, `kev`, `laya`, `jeff`, `systemone` (any other
    /// `/v1/systemone` server) or `chat_model` (the configured chat model answers the same typed
    /// questions). Empty is the older form of this section: a System One server when `endpoint`
    /// is set, otherwise off.
    #[serde(default)]
    pub provider: String,
    /// The server (its `/v1/systemone` path is added). Empty: no judge, the default.
    #[serde(default)]
    pub endpoint: String,
    /// The model the server should answer with.
    #[serde(default = "default_model")]
    pub model: String,
    /// The environment variable that holds the key, for servers that need one. The key itself
    /// never goes in this file.
    #[serde(default)]
    pub api_key_env: String,
    /// How long a decision may take before the companion goes on without it.
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    /// Let the judge choose the tool a request needs.
    #[serde(default = "default_true")]
    pub route_tools: bool,
    /// How sure the judge must be of its pick for the companion to follow it. At 0.7 Kev-4B was
    /// right 98% of the time on the 243-tool catalogue, and that sure for 69% of requests.
    #[serde(default = "default_route_at")]
    pub route_at: f64,
    /// How many tools, closest by meaning, the judge chooses among.
    #[serde(default = "default_shortlist")]
    pub shortlist: usize,
    /// Let the judge check whether a browser press reads as a commitment (in addition to the word
    /// list, never instead of it: a judge can only add a card).
    #[serde(default = "default_true")]
    pub browser_commitments: bool,
}

/// What kind of decision model a configuration names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JudgeKind {
    Off,
    /// A `/v1/systemone` server; the name is its dialect (`jev`, `kev`, `laya`, `jeff`, `systemone`).
    SystemOne(&'static str),
    ChatModel,
}

/// The providers Settings offers, and what each fills in. `label` is what the person reads.
pub const PRESETS: &[(&str, &str, &str, &str, &str)] = &[
    // (provider, label, endpoint, model, key variable)
    ("off", "Off: no decision model", "", "", ""),
    ("kev", "Kev (on this machine or the home GPU box)", "http://127.0.0.1:8009", "kev-latest", ""),
    ("laya", "Laya (small, runs on this machine's CPU)", "http://127.0.0.1:8000", "laya", ""),
    ("jeff", "Jeff (on this machine)", "http://127.0.0.1:8765", "jeff-latest", ""),
    ("jev", "Jev (TypeSafe cloud: what is judged leaves this machine)", "https://api.typesafe.ai", "jev-latest", "JEV_API_KEY"),
    ("systemone", "Another System One server", "", "", ""),
    ("chat_model", "The chat model (slower, uncalibrated)", "", "", ""),
];

fn default_model() -> String { "kev-latest".to_string() }
fn default_timeout_ms() -> u64 { 2000 }
fn default_true() -> bool { true }
fn default_route_at() -> f64 { 0.7 }
fn default_shortlist() -> usize { 20 }

impl Default for JudgeConfig {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            model: default_model(),
            api_key_env: String::new(),
            timeout_ms: default_timeout_ms(),
            route_tools: default_true(),
            route_at: default_route_at(),
            shortlist: default_shortlist(),
            provider: String::new(),
            browser_commitments: default_true(),
        }
    }
}

impl JudgeConfig {
    /// What kind of decision model this names. An unknown provider is off, never a guess.
    pub fn kind(&self) -> JudgeKind {
        let has_endpoint = !self.endpoint.trim().is_empty();
        match self.provider.trim().to_ascii_lowercase().as_str() {
            "" if has_endpoint => JudgeKind::SystemOne("systemone"),
            "" | "off" => JudgeKind::Off,
            "chat_model" => JudgeKind::ChatModel,
            p => match PRESETS.iter().find(|(name, ..)| *name == p) {
                Some((name, ..)) if has_endpoint && !matches!(*name, "off" | "chat_model") => JudgeKind::SystemOne(name),
                _ => JudgeKind::Off,
            },
        }
    }

    /// Whether a judge is configured at all.
    pub fn enabled(&self) -> bool {
        self.kind() != JudgeKind::Off
    }

    /// A preset's configuration, keeping this one's policy fields (thresholds, which uses are on).
    pub fn with_preset(&self, provider: &str) -> JudgeConfig {
        let mut out = self.clone();
        if let Some((name, _, endpoint, model, key)) = PRESETS.iter().find(|(name, ..)| *name == provider) {
            out.provider = name.to_string();
            out.endpoint = endpoint.to_string();
            out.model = model.to_string();
            out.api_key_env = key.to_string();
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_provider_names_the_kind_and_an_unknown_one_is_off() {
        let kev = JudgeConfig::default().with_preset("kev");
        assert_eq!(kev.kind(), JudgeKind::SystemOne("kev"));
        assert_eq!(kev.endpoint, "http://127.0.0.1:8009");
        assert_eq!(JudgeConfig::default().with_preset("chat_model").kind(), JudgeKind::ChatModel);
        assert_eq!(JudgeConfig::default().with_preset("off").kind(), JudgeKind::Off);
        let odd = JudgeConfig { provider: "gpt-judge-9000".into(), endpoint: "http://x".into(), ..JudgeConfig::default() };
        assert_eq!(odd.kind(), JudgeKind::Off);
        let no_endpoint = JudgeConfig { provider: "kev".into(), ..JudgeConfig::default() };
        assert_eq!(no_endpoint.kind(), JudgeKind::Off, "a server with no address is not a judge");
        let jev = JudgeConfig::default().with_preset("jev");
        assert_eq!(jev.api_key_env, "JEV_API_KEY");
    }

    #[test]
    fn a_preset_keeps_the_policy_it_was_chosen_under() {
        let mut c = JudgeConfig::default();
        c.route_at = 0.9;
        c.browser_commitments = false;
        let laya = c.with_preset("laya");
        assert_eq!((laya.route_at, laya.browser_commitments), (0.9, false));
    }

    #[test]
    fn no_judge_unless_an_endpoint_is_named() {
        assert!(!JudgeConfig::default().enabled());
        let c: JudgeConfig = serde_yaml::from_str("endpoint: \"http://127.0.0.1:8009\"").unwrap();
        assert!(c.enabled());
        assert_eq!(c.model, "kev-latest");
        assert!(c.route_tools);
        assert_eq!(c.route_at, 0.7);
    }
}
