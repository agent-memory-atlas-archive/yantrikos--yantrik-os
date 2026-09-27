//! `judge:` — a System One model that makes the companion's small decisions instead of the
//! chat model.
//!
//! A judge answers a typed question (which of these tools fits this request?) with a probability
//! for each option, in one pass and without writing text. Asking it which tool a request needs lets
//! the chat model see that one tool's schema instead of a shortlist of twenty, which saves the
//! tokens of every schema left out and stops the chat model picking the wrong one. Anything the
//! judge is unsure of, or any failure to reach it, falls back to the ordinary selection.
//!
//! The judge reads each request and the last few messages before it. A cloud judge therefore
//! receives that text; in incognito the companion asks no judge at all.
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeConfig {
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
}

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
        }
    }
}

impl JudgeConfig {
    /// Whether a judge is configured at all.
    pub fn enabled(&self) -> bool {
        !self.endpoint.trim().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
