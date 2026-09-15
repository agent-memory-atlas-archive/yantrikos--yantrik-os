//! Which mind is answering.
//!
//! The shell used to have exactly one: `yantrik-companion`, compiled in, reached through
//! `CompanionCommand::SendMessage` on a channel. That is a fine companion and a poor assumption —
//! `yantrik-mind`, OpenClaw and hermes-agent all exist, all want to drive this desktop, and none
//! of them is the companion. This crate is the seam between the shell and whichever one is
//! answering.
//!
//! # The thing this is designed for
//!
//! Adding the *next* harness, not the three we know about. Two decisions follow from that.
//!
//! **Most harnesses need no Rust.** `yantrik-mind` serves `POST /v1/chat/completions`; so does
//! hermes-agent, Ollama, vLLM, llama.cpp and almost everything else that will show up. One
//! [`adapters::OpenAiHttp`] covers all of them, so adding one is a YAML file in
//! `/etc/yantrik/harnesses/` or `~/.config/yantrik/harnesses/` — no build, no restart of anything
//! but the shell. A CLI agent that speaks line-delimited JSON on stdio is the other common shape
//! and gets [`adapters::Stdio`] for the same reason.
//!
//! **A harness that needs Rust implements one trait.** [`Harness`] is deliberately small: say who
//! you are, say what you can do, say whether you are reachable, and answer a turn as a stream of
//! chunks. Everything else — retries, history, the panel, the picker — is the shell's business,
//! not the adapter's.
//!
//! # Streaming is the contract
//!
//! [`Harness::send`] hands back a [`Receiver`] immediately and the chunks arrive as they are
//! produced. This is not a preference: the shell already renders a reply token by token from
//! `CompanionCommand::SendMessage`'s channel, and a trait that returned a finished `String` would
//! make every harness feel slower than the one it replaced. A backend with no streaming endpoint
//! sends one [`Chunk::Text`] and closes, which is honest and still works.
//!
//! # Health is asked, never assumed
//!
//! A picker that lists a harness the machine cannot reach is a picker that produces silence when
//! you choose it. [`Harness::health`] exists so the UI can say *why* before a person commits a
//! question to it — unreachable, or reachable but not configured, with the reason attached.

pub mod adapters;
pub mod registry;
pub mod spec;

pub use registry::Registry;
pub use spec::{Kind, Spec};

use std::sync::mpsc::Receiver;

/// One thing said to a harness.
///
/// Deliberately not a transcript. Conversation history lives in the shell, which owns the panel
/// and the memory; a harness that keeps its own history (the companion does) uses the text and
/// ignores the rest, and a stateless HTTP endpoint gets `context` to prepend. Putting the whole
/// history in this struct would mean every adapter had to agree on how to serialise a
/// conversation, which is exactly the coupling this crate exists to remove.
#[derive(Clone, Debug, Default)]
pub struct Turn {
    /// What the person typed.
    pub text: String,
    /// Optional system framing — who the harness is, what it is looking at.
    pub context: Option<String>,
}

impl Turn {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into(), context: None }
    }

    pub fn with_context(mut self, context: impl Into<String>) -> Self {
        self.context = Some(context.into());
        self
    }
}

/// One piece of an answer, as it arrives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Chunk {
    /// Text to append to the reply.
    Text(String),
    /// The turn failed. Carries what to show a person — a stream that simply stopped would be
    /// indistinguishable from a harness that had nothing more to say.
    Failed(String),
}

/// A stream of answer chunks. It ends when the channel closes.
pub type Answer = Receiver<Chunk>;

/// Whether a harness can be used right now, and if not, why not.
///
/// The distinction matters to the person choosing: `Unreachable` is usually something to fix on
/// the network or the other machine, `NotConfigured` is something to fix here, in Settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Health {
    Ready,
    /// Configured, but nothing answered.
    Unreachable(String),
    /// Missing something it needs before it could be reached at all — an endpoint, a key.
    NotConfigured(String),
}

impl Health {
    pub fn is_ready(&self) -> bool {
        matches!(self, Health::Ready)
    }

    /// One line for the picker, beside the name.
    pub fn summary(&self) -> String {
        match self {
            Health::Ready => "ready".to_string(),
            Health::Unreachable(why) => format!("unreachable — {why}"),
            Health::NotConfigured(why) => format!("not configured — {why}"),
        }
    }
}

/// What a harness can do, so the UI offers only what is actually there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// Chunks arrive as they are produced rather than all at once at the end.
    pub streaming: bool,
    /// Can run the OS's tools.
    pub tools: bool,
    /// Has memory that outlives the turn.
    pub memory: bool,
}

/// A mind the shell can put a question to.
///
/// Implement this only for a harness that cannot be described by an existing adapter. Before
/// writing one, check whether the thing speaks `/v1/chat/completions` or line-delimited JSON on
/// stdio — those already have adapters and need a config file instead.
pub trait Harness: Send + Sync {
    /// Stable id, as used in config and in `set_active`.
    fn id(&self) -> &str;

    /// What a person sees in the picker.
    fn name(&self) -> &str;

    fn capabilities(&self) -> Capabilities;

    /// Whether this could answer right now. May do IO; the UI calls it off the UI thread.
    fn health(&self) -> Health;

    /// Put one turn to this harness. Returns immediately; chunks arrive on the channel.
    fn send(&self, turn: Turn) -> Answer;
}

/// Collect a whole answer, for callers that cannot stream.
///
/// Returns `Err` on the first [`Chunk::Failed`], because half an answer followed by a failure is
/// not an answer, and a caller that concatenated both would show the error as if the harness had
/// said it.
pub fn collect(answer: Answer) -> Result<String, String> {
    let mut text = String::new();
    for chunk in answer {
        match chunk {
            Chunk::Text(part) => text.push_str(&part),
            Chunk::Failed(why) => return Err(why),
        }
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn answer_of(chunks: Vec<Chunk>) -> Answer {
        let (tx, rx) = mpsc::channel();
        for c in chunks {
            tx.send(c).unwrap();
        }
        rx
    }

    #[test]
    fn collecting_joins_the_chunks_in_order() {
        let answer = answer_of(vec![
            Chunk::Text("Hello".into()),
            Chunk::Text(", ".into()),
            Chunk::Text("world".into()),
        ]);
        assert_eq!(collect(answer).unwrap(), "Hello, world");
    }

    #[test]
    fn a_failure_partway_through_is_a_failure_not_a_partial_answer() {
        let answer = answer_of(vec![
            Chunk::Text("I was saying".into()),
            Chunk::Failed("the connection dropped".into()),
        ]);
        assert_eq!(collect(answer).unwrap_err(), "the connection dropped");
    }

    #[test]
    fn health_explains_itself_for_the_picker() {
        assert!(Health::Ready.is_ready());
        assert!(!Health::Unreachable("connection refused".into()).is_ready());
        assert_eq!(
            Health::NotConfigured("no endpoint".into()).summary(),
            "not configured — no endpoint"
        );
    }
}
