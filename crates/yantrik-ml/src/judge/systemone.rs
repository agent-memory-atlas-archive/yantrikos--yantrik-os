//! A judge reached over HTTP at a `/v1/systemone` endpoint: TypeSafe's cloud (Jev) or a local
//! server speaking the same protocol (Kev's `kev.serve`).

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Map, Value};

use super::{Answer, Judge, Question};

/// Where the judge is and how to reach it. The key is never held here as configuration: `key_env`
/// names the environment variable it is read from, when the endpoint needs one.
#[derive(Debug, Clone)]
pub struct SystemOneJudge {
    endpoint: String,
    model: String,
    key_env: Option<String>,
    timeout: Duration,
}

impl SystemOneJudge {
    /// `base_url` is the server (e.g. `https://api.typesafe.ai`, `http://127.0.0.1:8009`); the
    /// `/v1/systemone` path is added unless it is already there.
    pub fn new(base_url: &str, model: &str, key_env: Option<&str>, timeout: Duration) -> Self {
        let base = base_url.trim_end_matches('/');
        let endpoint = if base.ends_with("/v1/systemone") { base.to_string() } else { format!("{base}/v1/systemone") };
        Self {
            endpoint,
            model: model.to_string(),
            key_env: key_env.filter(|k| !k.is_empty()).map(str::to_string),
            timeout,
        }
    }

    fn body(&self, state: &Value, questions: &[(&str, Question)]) -> Value {
        let mut qs = Map::new();
        for (id, q) in questions {
            let q = match q {
                Question::Choice { instructions, options } => {
                    let criteria: Map<String, Value> =
                        options.iter().map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect();
                    json!({"type": "choice", "instructions": instructions, "criteria": criteria})
                }
                Question::Noul { instructions } => json!({"type": "noul", "instructions": instructions}),
            };
            qs.insert((*id).to_string(), q);
        }
        json!({"model": self.model, "state": state, "questions": qs})
    }
}

impl Judge for SystemOneJudge {
    fn name(&self) -> &str {
        &self.model
    }

    fn ask(&self, state: &Value, questions: &[(&str, Question)]) -> Result<HashMap<String, Answer>> {
        let agent = ureq::Agent::new_with_config(
            ureq::config::Config::builder().timeout_global(Some(self.timeout)).build(),
        );
        let mut request = agent.post(&self.endpoint).header("Content-Type", "application/json");
        if let Some(var) = &self.key_env {
            let key = std::env::var(var).map_err(|_| anyhow!("the judge's key variable {var} is not set"))?;
            request = request.header("Authorization", &format!("Bearer {key}"));
        }
        let reply: Value = request
            .send_json(self.body(state, questions))
            .with_context(|| format!("judge {} at {}", self.model, self.endpoint))?
            .body_mut()
            .read_json()
            .context("judge reply is not JSON")?;
        parse_answers(&reply, questions)
    }
}

/// Every question asked must come back answered, in the shape its type promises; a partial or
/// malformed reply is an error rather than a guess.
fn parse_answers(reply: &Value, questions: &[(&str, Question)]) -> Result<HashMap<String, Answer>> {
    let answers = reply.get("answers").and_then(Value::as_object).ok_or_else(|| anyhow!("judge reply has no answers"))?;
    let mut out = HashMap::new();
    for (id, q) in questions {
        let a = answers.get(*id).ok_or_else(|| anyhow!("judge did not answer {id}"))?;
        let answer = match q {
            Question::Choice { options, .. } => {
                let choice = a.get("choice").and_then(Value::as_str).ok_or_else(|| anyhow!("{id}: no choice"))?;
                if !options.iter().any(|(k, _)| k == choice) {
                    bail!("{id}: the judge chose {choice:?}, which was not offered");
                }
                let probabilities = a
                    .get("probabilities")
                    .and_then(Value::as_object)
                    .map(|m| m.iter().filter_map(|(k, v)| Some((k.clone(), v.as_f64()?))).collect())
                    .unwrap_or_default();
                let confidence = a.get("confidence").and_then(Value::as_f64).unwrap_or(0.0);
                Answer::Choice { choice: choice.to_string(), probabilities, confidence }
            }
            Question::Noul { .. } => Answer::Noul(a.get("noul").and_then(Value::as_f64).ok_or_else(|| anyhow!("{id}: no noul"))?),
        };
        out.insert((*id).to_string(), answer);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_question() -> Vec<(&'static str, Question)> {
        vec![
            ("tool", Question::Choice {
                instructions: "Which tool?".into(),
                options: vec![("get_weather".into(), "weather".into()), ("none".into(), "no tool".into())],
            }),
            ("urgent", Question::Noul { instructions: "Is it urgent?".into() }),
        ]
    }

    #[test]
    fn endpoint_path_is_added_once() {
        let d = Duration::from_secs(1);
        assert_eq!(SystemOneJudge::new("http://127.0.0.1:8009/", "kev-latest", None, d).endpoint, "http://127.0.0.1:8009/v1/systemone");
        assert_eq!(SystemOneJudge::new("https://api.typesafe.ai/v1/systemone", "jev-latest", None, d).endpoint, "https://api.typesafe.ai/v1/systemone");
    }

    #[test]
    fn request_carries_model_state_and_typed_questions() {
        let j = SystemOneJudge::new("http://x", "kev-latest", None, Duration::from_secs(1));
        let body = j.body(&json!({"request": "weather in Dallas"}), &tool_question());
        assert_eq!(body["model"], "kev-latest");
        assert_eq!(body["state"]["request"], "weather in Dallas");
        assert_eq!(body["questions"]["tool"]["type"], "choice");
        assert_eq!(body["questions"]["tool"]["criteria"]["get_weather"], "weather");
        assert_eq!(body["questions"]["urgent"]["type"], "noul");
    }

    #[test]
    fn a_whole_reply_parses() {
        let reply = json!({"answers": {
            "tool": {"type": "choice", "choice": "get_weather", "confidence": 0.9, "probabilities": {"get_weather": 0.95, "none": 0.05}},
            "urgent": {"type": "noul", "noul": 0.12}}});
        let a = parse_answers(&reply, &tool_question()).unwrap();
        assert_eq!(a["tool"].picked_probability(), Some(0.95));
        assert_eq!(a["urgent"], Answer::Noul(0.12));
    }

    #[test]
    fn a_missing_or_unoffered_answer_is_an_error_not_a_guess() {
        let missing = json!({"answers": {"tool": {"choice": "none", "probabilities": {}}}});
        assert!(parse_answers(&missing, &tool_question()).is_err());
        let unoffered = json!({"answers": {"tool": {"choice": "rm_rf"}, "urgent": {"noul": 0.1}}});
        assert!(parse_answers(&unoffered, &tool_question()).unwrap_err().to_string().contains("not offered"));
    }

    #[test]
    fn a_key_variable_that_is_not_set_fails_before_any_request() {
        let j = SystemOneJudge::new("http://127.0.0.1:9", "jev-latest", Some("YANTRIK_TEST_UNSET_JUDGE_KEY"), Duration::from_millis(50));
        let err = j.ask(&json!({}), &tool_question()).unwrap_err().to_string();
        assert!(err.contains("YANTRIK_TEST_UNSET_JUDGE_KEY"), "{err}");
    }
}
