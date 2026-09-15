//! Anything serving `POST /v1/chat/completions`.
//!
//! This is the adapter that makes a new harness a config file. `yantrik-mind` documents
//! `POST /v1/chat/completions` in its own README; hermes-agent was deployed against an
//! OpenAI-compatible endpoint; Ollama, vLLM and llama.cpp all speak it. One implementation, and
//! every one of them is a five-line YAML file away from being selectable in the picker.
//!
//! # Streaming
//!
//! Asks for `stream: true` and reads Server-Sent Events. A server that ignores the flag and
//! returns one JSON object still works — [`parse_body`] handles both — because "it streamed
//! nothing and then everything" is a performance property, not a correctness one, and a harness
//! should not be unusable for it.

use std::io::{BufRead, BufReader};
use std::sync::mpsc;
use std::time::Duration;

use crate::spec::Spec;
use crate::{Answer, Capabilities, Chunk, Harness, Health, Turn};

/// Long, because a local model on a cold cache is slow and a caller that gives up early leaves
/// the person with nothing. The health check uses its own much shorter budget.
const REPLY_TIMEOUT: Duration = Duration::from_secs(180);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(4);

pub struct OpenAiHttp {
    spec: Spec,
}

impl OpenAiHttp {
    pub fn new(spec: Spec) -> Self {
        Self { spec }
    }

    fn endpoint(&self) -> String {
        let base = self.spec.endpoint.clone().unwrap_or_default();
        format!("{}/chat/completions", base.trim_end_matches('/'))
    }
}

impl Harness for OpenAiHttp {
    fn id(&self) -> &str {
        &self.spec.id
    }

    fn name(&self) -> &str {
        &self.spec.name
    }

    fn capabilities(&self) -> Capabilities {
        // Tools are not claimed. The OS's tools run through the companion's registry with its
        // permission ceiling; an arbitrary endpoint has no access to them, and saying otherwise
        // would put a control an agent cannot use in front of a person.
        Capabilities { streaming: true, tools: false, memory: false }
    }

    fn health(&self) -> Health {
        let Some(base) = self.spec.endpoint.as_deref().filter(|e| !e.trim().is_empty()) else {
            return Health::NotConfigured("no endpoint".into());
        };
        if self.spec.api_key_env.is_some() && self.spec.api_key().is_none() {
            return Health::NotConfigured(format!(
                "{} is not set in the environment",
                self.spec.api_key_env.clone().unwrap_or_default()
            ));
        }
        // `/models` is the cheapest thing an OpenAI-compatible server will answer, and a 404 from
        // it still proves something is listening and speaking HTTP.
        let url = format!("{}/models", base.trim_end_matches('/'));
        match ureq::get(&url).timeout(HEALTH_TIMEOUT).call() {
            Ok(_) => Health::Ready,
            Err(ureq::Error::Status(_, _)) => Health::Ready,
            Err(e) => Health::Unreachable(format!("{e}")),
        }
    }

    fn send(&self, turn: Turn) -> Answer {
        let (tx, rx) = mpsc::channel();
        let url = self.endpoint();
        let body = request_body(&self.spec, &turn);
        let key = self.spec.api_key();

        std::thread::Builder::new()
            .name(format!("harness-{}", self.spec.id))
            .spawn(move || {
                let mut request = ureq::post(&url)
                    .timeout(REPLY_TIMEOUT)
                    .set("Content-Type", "application/json");
                if let Some(key) = key {
                    request = request.set("Authorization", &format!("Bearer {key}"));
                }
                match request.send_json(body) {
                    Ok(response) => stream_reply(response, &tx),
                    Err(ureq::Error::Status(code, response)) => {
                        // The body of an error usually says what is actually wrong — a bad model
                        // name, a missing key — and is far more use than the number.
                        let detail = response.into_string().unwrap_or_default();
                        let detail = detail.trim();
                        let message = if detail.is_empty() {
                            format!("HTTP {code}")
                        } else {
                            format!("HTTP {code}: {}", truncate(detail, 300))
                        };
                        let _ = tx.send(Chunk::Failed(message));
                    }
                    Err(e) => {
                        let _ = tx.send(Chunk::Failed(format!("{e}")));
                    }
                }
            })
            .ok();

        rx
    }
}

/// Read the reply, whether it arrives as SSE or as one object.
fn stream_reply(response: ureq::Response, tx: &mpsc::Sender<Chunk>) {
    let streaming = response
        .header("content-type")
        .map(|t| t.contains("event-stream"))
        .unwrap_or(false);

    if !streaming {
        let body = response.into_string().unwrap_or_default();
        match parse_body(&body) {
            Some(text) if !text.is_empty() => {
                let _ = tx.send(Chunk::Text(text));
            }
            _ => {
                let _ = tx.send(Chunk::Failed(format!(
                    "the reply had no message content: {}",
                    truncate(body.trim(), 200)
                )));
            }
        }
        return;
    }

    let mut reader = BufReader::new(response.into_reader());
    let mut line = String::new();
    let mut said_anything = false;
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => match parse_sse_line(&line) {
                SseLine::Delta(text) => {
                    said_anything = true;
                    if tx.send(Chunk::Text(text)).is_err() {
                        return; // nobody is listening any more
                    }
                }
                SseLine::Done => break,
                SseLine::Ignore => {}
            },
            Err(e) => {
                let _ = tx.send(Chunk::Failed(format!("the stream broke: {e}")));
                return;
            }
        }
    }
    if !said_anything {
        let _ = tx.send(Chunk::Failed("the harness streamed an empty reply".into()));
    }
}

/// What one line of an SSE stream means.
#[derive(Debug, PartialEq, Eq)]
pub enum SseLine {
    Delta(String),
    Done,
    Ignore,
}

/// Parse one `data:` line.
///
/// Worth having as a function of a string: SSE framing is where this kind of adapter usually goes
/// wrong, and none of the ways it goes wrong need a server to reproduce.
pub fn parse_sse_line(line: &str) -> SseLine {
    let line = line.trim();
    if line.is_empty() || !line.starts_with("data:") {
        // Comments (`: keep-alive`), blank separators, and `event:` lines.
        return SseLine::Ignore;
    }
    let payload = line["data:".len()..].trim();
    if payload == "[DONE]" {
        return SseLine::Done;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return SseLine::Ignore;
    };
    let delta = value["choices"][0]["delta"]["content"].as_str().unwrap_or("");
    if delta.is_empty() {
        // The first frame of a stream carries a role and no content; so does the last.
        SseLine::Ignore
    } else {
        SseLine::Delta(delta.to_string())
    }
}

/// Pull the message out of a non-streamed reply.
///
/// Handles the `message.content` shape and the older `text` one, because "OpenAI-compatible"
/// covers servers that are compatible to different depths.
pub fn parse_body(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let choice = &value["choices"][0];
    choice["message"]["content"]
        .as_str()
        .or_else(|| choice["text"].as_str())
        .map(|s| s.to_string())
}

/// The JSON sent for one turn.
pub fn request_body(spec: &Spec, turn: &Turn) -> serde_json::Value {
    let mut messages = Vec::new();
    if let Some(context) = turn.context.as_ref().filter(|c| !c.trim().is_empty()) {
        messages.push(serde_json::json!({ "role": "system", "content": context }));
    }
    messages.push(serde_json::json!({ "role": "user", "content": turn.text }));

    serde_json::json!({
        // Some servers require the field even when they serve exactly one model.
        "model": spec.model.clone().unwrap_or_else(|| "default".to_string()),
        "messages": messages,
        "stream": true,
    })
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::Kind;

    fn spec() -> Spec {
        Spec {
            id: "mind".into(),
            name: "Mind".into(),
            kind: Kind::OpenaiHttp,
            endpoint: Some("http://host:8080/v1".into()),
            model: Some("qwen2.5".into()),
            api_key_env: None,
            command: vec![],
            enabled: true,
        }
    }

    #[test]
    fn the_endpoint_is_built_without_doubling_the_slash() {
        let mut s = spec();
        s.endpoint = Some("http://host:8080/v1/".into());
        assert_eq!(OpenAiHttp::new(s).endpoint(), "http://host:8080/v1/chat/completions");
        assert_eq!(OpenAiHttp::new(spec()).endpoint(), "http://host:8080/v1/chat/completions");
    }

    #[test]
    fn a_turn_becomes_the_request_a_server_expects() {
        let body = request_body(&spec(), &Turn::new("hello"));
        assert_eq!(body["model"], "qwen2.5");
        assert_eq!(body["stream"], true);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "hello");
    }

    #[test]
    fn context_is_sent_as_the_system_message() {
        let body = request_body(&spec(), &Turn::new("hi").with_context("You are Yantrik."));
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "You are Yantrik.");
        assert_eq!(body["messages"][1]["role"], "user");
    }

    #[test]
    fn a_server_with_one_model_still_gets_the_field_it_demands() {
        let mut s = spec();
        s.model = None;
        assert_eq!(request_body(&s, &Turn::new("x"))["model"], "default");
    }

    #[test]
    fn reads_the_deltas_out_of_a_stream() {
        assert_eq!(
            parse_sse_line(r#"data: {"choices":[{"delta":{"content":"Hel"}}]}"#),
            SseLine::Delta("Hel".into())
        );
        assert_eq!(parse_sse_line("data: [DONE]"), SseLine::Done);
    }

    #[test]
    fn ignores_the_frames_that_carry_no_text() {
        // The opening frame announces a role and no content; keep-alive comments and blank
        // separator lines are structural. Treating any of them as text puts empty strings, or
        // worse, into the reply.
        assert_eq!(parse_sse_line(r#"data: {"choices":[{"delta":{"role":"assistant"}}]}"#), SseLine::Ignore);
        assert_eq!(parse_sse_line(": keep-alive"), SseLine::Ignore);
        assert_eq!(parse_sse_line(""), SseLine::Ignore);
        assert_eq!(parse_sse_line("event: message"), SseLine::Ignore);
        // Malformed JSON in one frame must not take the whole reply down.
        assert_eq!(parse_sse_line("data: {not json"), SseLine::Ignore);
    }

    #[test]
    fn a_server_that_ignored_stream_true_still_works() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"the whole answer"}}]}"#;
        assert_eq!(parse_body(body).as_deref(), Some("the whole answer"));
    }

    #[test]
    fn understands_the_older_completion_shape_too() {
        assert_eq!(parse_body(r#"{"choices":[{"text":"older"}]}"#).as_deref(), Some("older"));
    }

    #[test]
    fn says_it_is_not_configured_before_trying_to_reach_nothing() {
        let mut s = spec();
        s.endpoint = None;
        assert_eq!(OpenAiHttp::new(s).health(), Health::NotConfigured("no endpoint".into()));
    }

    #[test]
    fn a_key_that_was_promised_and_is_missing_is_a_configuration_problem() {
        // Not "unreachable": the fix is here, in Settings, not on the other machine.
        let mut s = spec();
        s.api_key_env = Some("DEFINITELY_UNSET_YANTRIK_TEST_VAR".into());
        std::env::remove_var("DEFINITELY_UNSET_YANTRIK_TEST_VAR");
        match OpenAiHttp::new(s).health() {
            Health::NotConfigured(why) => assert!(why.contains("DEFINITELY_UNSET"), "{why}"),
            other => panic!("expected NotConfigured, got {other:?}"),
        }
    }

    #[test]
    fn does_not_claim_tools_it_cannot_run() {
        assert!(!OpenAiHttp::new(spec()).capabilities().tools);
        assert!(OpenAiHttp::new(spec()).capabilities().streaming);
    }
}
