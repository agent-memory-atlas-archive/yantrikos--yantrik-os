//! A provider that answered and refused the request: what it said, and the parameters it will not
//! take.
//!
//! VM 542, 16 Oct 2026: the person chose Claude in the first run, the key test passed, and every
//! companion turn then came back `400 {"error":{"message":"`temperature` is deprecated for this
//! model."}}`. Current Claude models refuse sampling parameters outright, and the OpenAI-compatible
//! clients sent `temperature` on every request. The turn was served by the offline responder, and
//! the first run said "it is not set up to think yet", which hid the one sentence that said what
//! was wrong.
//!
//! So, for every OpenAI-compatible client in this crate (one module, never copies):
//! - a 400 that names a sampling parameter as one the model will not take is answered by sending
//!   the same request once more without it, and the refusal is remembered per (address, model)
//!   for the life of the process, so later calls (and the key test, which shares the process)
//!   leave it out from the start ([`send_dropping_refused`]);
//! - on Anthropic's own endpoint a sampling value nobody chose (the [`GenerationConfig`] default)
//!   is not sent at all ([`prepare`]): Anthropic then uses the model's own default, which is what
//!   every current Claude model wants, and older ones lose nothing they were asked for;
//! - any other refusal is [`ProviderRefused`], carrying the provider's own one-line reason, so a
//!   caller can tell the person what the provider said instead of falling back to canned words.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::types::GenerationConfig;

/// The request fields a model may refuse to be sent, and that a request can go without: each
/// only tunes sampling, so leaving it out means the provider's default.
pub const SAMPLING_PARAMETERS: [&str; 5] = ["temperature", "top_p", "top_k", "frequency_penalty", "presence_penalty"];

/// Words a provider uses when it will not take a parameter at all (as opposed to a value out of
/// range, which is the caller's to fix and is not retried).
const WILL_NOT_TAKE: [&str; 8] = [
    "deprecated",
    "not supported",
    "unsupported",
    "not allowed",
    "not permitted",
    "no longer supported",
    "cannot both be specified",
    "cannot be specified",
];

/// Anthropic's API host: its OpenAI-compatible endpoint is `https://api.anthropic.com/v1`.
const ANTHROPIC_HOST: &str = "api.anthropic.com";

/// The host (and port) of a URL, never its path, query or `user:pass@`: a path can carry an
/// account id (Cloudflare), a query a key.
pub fn host_of(url: &str) -> String {
    let authority = url.split("://").nth(1).and_then(|r| r.split(['/', '?', '#']).next()).unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or("");
    if host.is_empty() { "the provider".to_string() } else { host.to_string() }
}

/// Whether `base_url` is Anthropic's own API.
pub fn is_anthropic(base_url: &str) -> bool {
    host_of(base_url).eq_ignore_ascii_case(ANTHROPIC_HOST)
}

/// Text from a provider, made safe to show and to log: only characters that print. Control
/// characters (C0 and C1, so ESC, NUL and BEL), bidirectional overrides and isolates, zero-width
/// characters and the byte-order mark are dropped, and every run of whitespace (newlines too)
/// becomes one space. An endpoint the person configured writes this text; without this a bidi
/// override could make "X refused the request: …" read as something else in the UI, and an
/// escape sequence would reach any terminal or log viewer that shows it raw.
pub fn sanitize(text: &str) -> String {
    let invisible = |c: char| {
        matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}')
    };
    let printable: String = text
        .chars()
        .filter_map(|c| if c.is_whitespace() { Some(' ') } else if c.is_control() || invisible(c) { None } else { Some(c) })
        .collect();
    printable.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A provider's error body, short enough to say what went wrong and safe to show: cleaned
/// ([`sanitize`]), the key masked, then cut, so a key echoed across the cut cannot leave its head
/// behind.
pub fn excerpt(body: &str, key: Option<&str>) -> String {
    let clean = sanitize(body);
    let masked = match key.filter(|k| k.len() >= 8) {
        Some(k) => clean.replace(k, "<key>"),
        None => clean,
    };
    masked.chars().take(200).collect()
}

/// The provider's own words in an error body, as it wrote them: `error.message` (OpenAI's shape,
/// Anthropic's in both its APIs, llama.cpp's), a bare `error` string, or a top-level `message`.
fn raw_message(body: &[u8]) -> Option<String> {
    let json: serde_json::Value = serde_json::from_slice(body).ok()?;
    let message = json["error"]["message"]
        .as_str()
        .or_else(|| json["error"].as_str())
        .or_else(|| json["message"].as_str())?;
    Some(message.to_string())
}

/// The provider's own words from an error body ([`raw_message`]), cleaned ([`sanitize`]): one line.
pub fn provider_message(body: &str) -> Option<String> {
    let line = sanitize(&raw_message(body.as_bytes())?);
    (!line.is_empty()).then_some(line)
}

/// Whether `word` appears in `text` on its own: not inside a longer name (`top_p` is not in
/// `top_probability`).
fn names(text: &str, word: &str) -> bool {
    let is_name_char = |c: char| c.is_ascii_alphanumeric() || c == '_';
    text.match_indices(word).any(|(i, _)| {
        let before = text[..i].chars().next_back();
        let after = text[i + word.len()..].chars().next();
        !before.is_some_and(is_name_char) && !after.is_some_and(is_name_char)
    })
}

/// The sampling parameter a refusal says the model will not take, when the request carried it.
///
/// Conservative on purpose: only a 400, only a parameter the request really sent, and only when
/// the provider's message names it and says it is not taken at all. A 400 for anything else
/// (a bad message, a value out of range) is the caller's and is not retried.
pub fn refused_parameter(status: u16, body: &str, sent: &serde_json::Value) -> Option<&'static str> {
    if status != 400 {
        return None;
    }
    let message = provider_message(body)?.to_ascii_lowercase();
    if !WILL_NOT_TAKE.iter().any(|w| message.contains(w)) {
        return None;
    }
    SAMPLING_PARAMETERS.into_iter().find(|p| sent.get(*p).is_some() && names(&message, p))
}

type Refusals = HashMap<(String, String), Vec<&'static str>>;

fn refusals() -> &'static Mutex<Refusals> {
    static REFUSED: OnceLock<Mutex<Refusals>> = OnceLock::new();
    REFUSED.get_or_init(Default::default)
}

fn key(base_url: &str, model: &str) -> (String, String) {
    (base_url.trim().trim_end_matches('/').to_ascii_lowercase(), model.to_string())
}

/// Remember for this process that `model` at `base_url` will not take `parameter`.
pub fn remember(base_url: &str, model: &str, parameter: &'static str) {
    let mut all = refusals().lock().unwrap_or_else(|e| e.into_inner());
    let known = all.entry(key(base_url, model)).or_default();
    if !known.contains(&parameter) {
        known.push(parameter);
    }
}

/// What `model` at `base_url` is known to refuse.
pub fn remembered(base_url: &str, model: &str) -> Vec<&'static str> {
    refusals().lock().unwrap_or_else(|e| e.into_inner()).get(&key(base_url, model)).cloned().unwrap_or_default()
}

/// Make a built chat-completions body fit what this provider takes: leave out what the model is
/// known to refuse, and on Anthropic's endpoint the sampling values nobody chose. Every
/// OpenAI-compatible body builder ends with this, the key test included, so all send one shape.
pub fn prepare(base_url: &str, model: &str, config: &GenerationConfig, body: &mut serde_json::Value) {
    let Some(fields) = body.as_object_mut() else { return };
    for parameter in remembered(base_url, model) {
        fields.remove(parameter);
    }
    if is_anthropic(base_url) {
        let unchosen = GenerationConfig::default();
        if config.temperature == unchosen.temperature {
            fields.remove("temperature");
        }
        if config.top_p == unchosen.top_p {
            fields.remove("top_p");
        }
    }
}

/// Send `body` with `post`, and when the provider refuses a sampling parameter it names, send it
/// once more without that parameter, remembering the refusal for every later call.
///
/// `post` sends one request: `Ok(Ok(_))` is an answer, `Ok(Err((status, body)))` the provider's
/// refusal as it said it, `Err` a failure on the way. Each parameter is dropped at most once, so
/// this sends at most one request more than there are [`SAMPLING_PARAMETERS`].
pub fn send_dropping_refused<T>(
    base_url: &str,
    model: &str,
    body: &serde_json::Value,
    mut post: impl FnMut(&serde_json::Value) -> anyhow::Result<Result<T, (u16, String)>>,
) -> anyhow::Result<Result<T, (u16, String)>> {
    let mut body = body.clone();
    if let Some(fields) = body.as_object_mut() {
        for parameter in remembered(base_url, model) {
            fields.remove(parameter);
        }
    }
    loop {
        let (status, text) = match post(&body)? {
            Ok(answer) => return Ok(Ok(answer)),
            Err(refusal) => refusal,
        };
        let Some(parameter) = refused_parameter(status, &text, &body) else {
            return Ok(Err((status, text)));
        };
        tracing::warn!(
            host = %host_of(base_url),
            model,
            parameter,
            "the model refused a sampling parameter (400); asking once more without it, and not sending it again"
        );
        remember(base_url, model, parameter);
        if let Some(fields) = body.as_object_mut() {
            fields.remove(parameter);
        }
    }
}

/// The provider answered and refused the request (a 4xx that is not a timeout or a rate limit):
/// the model was reached, so this is not "offline", and its reason is what the person should read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRefused {
    pub status: u16,
    /// Who refused, as the person knows them: "Claude" for Anthropic, else the host.
    pub provider: String,
    /// The provider's own one-line reason, or the status when it gave none.
    pub reason: String,
}

impl ProviderRefused {
    /// A refusal from `base_url`, if `status` is one: a 4xx other than 408 (timeout) and 429
    /// (rate limit), which say nothing about the request itself.
    pub fn of(status: u16, base_url: &str, body: &str, api_key: Option<&str>) -> Option<ProviderRefused> {
        if !(400..500).contains(&status) || status == 408 || status == 429 {
            return None;
        }
        let provider = if is_anthropic(base_url) { "Claude".to_string() } else { host_of(base_url) };
        let reason = provider_message(body)
            .map(|m| excerpt(&m, api_key).trim_end_matches('.').to_string())
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| format!("HTTP {status}"));
        Some(ProviderRefused { status, provider, reason })
    }

    /// The sentence the person reads in the chat and in the first run's check.
    pub fn said(&self) -> String {
        format!("{self}. Try again, or choose another model.")
    }
}

impl std::fmt::Display for ProviderRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} refused the request: {}", self.provider, self.reason)
    }
}

impl std::error::Error for ProviderRefused {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// api.anthropic.com/v1/chat/completions on VM 542, 16 Oct 2026, as the error log line showed it
    /// (`code` and `message` as logged; the log cut the rest).
    const TEMPERATURE_DEPRECATED: &str = include_str!("llm/testdata/anthropic_temperature_deprecated.json");

    fn body() -> serde_json::Value {
        serde_json::json!({"model": "claude-sonnet-5-5", "messages": [], "max_tokens": 8, "temperature": 0.1, "top_p": 0.9, "stream": true})
    }

    #[test]
    fn the_recorded_refusal_names_temperature() {
        assert_eq!(refused_parameter(400, TEMPERATURE_DEPRECATED, &body()), Some("temperature"));
        assert_eq!(provider_message(TEMPERATURE_DEPRECATED).as_deref(), Some("`temperature` is deprecated for this model."));
    }

    #[test]
    fn other_refusals_are_not_taken_for_a_parameter() {
        let sent = body();
        // Not a 400; a 400 about something else; a value out of range; a parameter not sent.
        assert_eq!(refused_parameter(422, TEMPERATURE_DEPRECATED, &sent), None);
        assert_eq!(refused_parameter(400, r#"{"error":{"message":"messages: at least one message is required"}}"#, &sent), None);
        assert_eq!(refused_parameter(400, r#"{"error":{"message":"temperature: must be between 0 and 1"}}"#, &sent), None);
        assert_eq!(refused_parameter(400, r#"{"error":{"message":"`top_k` is not supported"}}"#, &sent), None);
        assert_eq!(refused_parameter(400, "not json at all: temperature is deprecated", &sent), None);
        assert_eq!(refused_parameter(400, r#"{"error":{"message":"top_probability is deprecated"}}"#, &sent), None);
        // The other one the same models refuse, once temperature is gone.
        assert_eq!(refused_parameter(400, r#"{"error":{"message":"`top_p` is not supported for this model."}}"#, &sent), Some("top_p"));
    }

    #[test]
    fn a_refused_parameter_is_dropped_retried_once_and_remembered() {
        let base = "https://refusal-test-1.example/v1";
        let sent = RefCell::new(Vec::new());
        let reply = send_dropping_refused(base, "m", &body(), |b| {
            sent.borrow_mut().push(b.clone());
            Ok(if b.get("temperature").is_some() { Err((400, TEMPERATURE_DEPRECATED.to_string())) } else { Ok("answer") })
        })
        .unwrap();
        assert_eq!(reply, Ok("answer"));
        let sent = sent.into_inner();
        assert_eq!(sent.len(), 2, "the refusal, then one retry");
        assert!(sent[0].get("temperature").is_some());
        assert!(sent[1].get("temperature").is_none(), "the retry goes without it");
        assert_eq!(sent[1]["top_p"], 0.9, "and keeps everything else");
        assert_eq!(remembered(base, "m"), vec!["temperature"]);
        assert!(remembered(base, "another-model").is_empty(), "per model");

        // The next call leaves it out from the start: one request, no refusal.
        let mut calls = 0;
        send_dropping_refused(base, "m", &body(), |b| {
            calls += 1;
            assert!(b.get("temperature").is_none());
            Ok(Ok::<_, (u16, String)>(()))
        })
        .unwrap()
        .unwrap();
        assert_eq!(calls, 1);
    }

    #[test]
    fn any_other_400_is_returned_as_it_came_and_not_retried() {
        let base = "https://refusal-test-2.example/v1";
        let mut calls = 0;
        let reply = send_dropping_refused(base, "m", &body(), |_| {
            calls += 1;
            Ok(Err::<(), _>((400, r#"{"error":{"message":"bad message"}}"#.to_string())))
        })
        .unwrap();
        assert_eq!(reply, Err((400, r#"{"error":{"message":"bad message"}}"#.to_string())));
        assert_eq!(calls, 1);
        assert!(remembered(base, "m").is_empty());
    }

    #[test]
    fn each_parameter_is_dropped_at_most_once() {
        let base = "https://refusal-test-3.example/v1";
        let mut calls = 0;
        let reply = send_dropping_refused(base, "m", &body(), |b| {
            calls += 1;
            let named = if b.get("temperature").is_some() { "temperature" } else { "top_p" };
            Ok(Err::<(), _>((400, format!(r#"{{"error":{{"message":"`{named}` is deprecated for this model."}}}}"#))))
        })
        .unwrap();
        assert!(reply.is_err(), "a request with nothing left to drop is the caller's refusal");
        assert_eq!(calls, 3, "with both, without temperature, without top_p");
    }

    #[test]
    fn on_anthropics_endpoint_sampling_nobody_chose_is_not_sent() {
        let unchosen = GenerationConfig::default();
        let mut sent = serde_json::json!({"temperature": unchosen.temperature, "top_p": unchosen.top_p});
        prepare("https://api.anthropic.com/v1", "claude-haiku-4-5", &unchosen, &mut sent);
        assert_eq!(sent, serde_json::json!({}));

        // A value someone chose is still sent (an older Claude takes it; a newer one is retried).
        let chosen = GenerationConfig { temperature: 0.1, ..Default::default() };
        let mut sent = serde_json::json!({"temperature": 0.1, "top_p": unchosen.top_p});
        prepare("https://api.anthropic.com/v1", "claude-haiku-4-5", &chosen, &mut sent);
        assert_eq!(sent, serde_json::json!({"temperature": 0.1}));

        // Anywhere else nothing changes.
        let mut sent = serde_json::json!({"temperature": unchosen.temperature});
        prepare("https://api.openai.com/v1", "gpt-4o", &unchosen, &mut sent);
        assert_eq!(sent["temperature"], unchosen.temperature);
    }

    #[test]
    fn provider_text_keeps_only_what_prints() {
        // A bidi override that would turn the sentence around, a clear-screen escape, a bell, a
        // NUL, zero-width characters, a BOM, an isolate and a C1 control, with newlines and tabs.
        let said = "\u{202e}txet\x1b[2J\x07 is\0 de\u{200b}pre\u{feff}cated\n\tfor\u{2066} this\u{9b} model\u{2069}.";
        assert_eq!(sanitize(said), "txet[2J is deprecated for this model.");
        assert_eq!(excerpt(said, None), "txet[2J is deprecated for this model.");

        let body = serde_json::json!({"error": {"message": said}}).to_string();
        assert_eq!(provider_message(&body).as_deref(), Some("txet[2J is deprecated for this model."));
        let r = ProviderRefused::of(400, "https://api.anthropic.com/v1", &body, None).unwrap();
        assert_eq!(r.to_string(), "Claude refused the request: txet[2J is deprecated for this model");
        assert!(!r.said().chars().any(|c| c.is_control() || ('\u{2000}'..='\u{20ff}').contains(&c)), "{:?}", r.said());

        // The key is masked after cleaning, so a key split by an invisible character is still masked.
        let key = "sk-0123456789abcdef";
        assert_eq!(excerpt("bad key sk-01234\u{200b}56789abcdef", Some(key)), "bad key <key>");
    }

    #[test]
    fn a_refusal_says_who_refused_and_their_reason_in_one_line() {
        let r = ProviderRefused::of(400, "https://api.anthropic.com/v1", TEMPERATURE_DEPRECATED, None).unwrap();
        assert_eq!(r.to_string(), "Claude refused the request: `temperature` is deprecated for this model");
        assert_eq!(r.said(), "Claude refused the request: `temperature` is deprecated for this model. Try again, or choose another model.");

        let key = "sk-0123456789abcdef";
        let r = ProviderRefused::of(401, "https://api.groq.com/openai/v1", &format!(r#"{{"error":{{"message":"Invalid API key {key}\nprovided"}}}}"#), Some(key)).unwrap();
        assert_eq!(r.to_string(), "api.groq.com refused the request: Invalid API key <key> provided");
        assert_eq!(ProviderRefused::of(404, "http://10.0.0.2:8080/v1", "", None).unwrap().reason, "HTTP 404");
        for not_a_refusal in [200, 408, 429, 500, 503] {
            assert!(ProviderRefused::of(not_a_refusal, "https://api.anthropic.com/v1", TEMPERATURE_DEPRECATED, None).is_none());
        }
    }
}
