//! Judges: small models that answer a typed question about some state with probabilities,
//! not text.
//!
//! A System One model (TypeSafe's Jev in the cloud, Kev run locally, and whatever speaks the
//! same `/v1/systemone` protocol next) reads the state once and returns, for each question, a
//! distribution over its options. That is enough for the decisions a chat model is otherwise
//! asked to write out at length (which tool fits this request, whether a condition holds), at a
//! fraction of the tokens and in a fraction of the time. What to do with an answer, and how sure
//! it must be, stays with the caller.

#[cfg(feature = "api-llm")]
mod systemone;

#[cfg(feature = "api-llm")]
pub use systemone::SystemOneJudge;

use std::collections::HashMap;

use anyhow::Result;
use serde_json::Value;

/// One question put to a judge.
#[derive(Debug, Clone)]
pub enum Question {
    /// Pick one of `options` (name, description), in the order given.
    Choice { instructions: String, options: Vec<(String, String)> },
    /// Does the condition in `instructions` hold?
    Noul { instructions: String },
}

/// A judge's answer to one question.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    /// The most likely option, the probability of each option, and how concentrated the
    /// distribution is (1.0 = all on one option).
    Choice { choice: String, probabilities: HashMap<String, f64>, confidence: f64 },
    /// The probability that the condition holds.
    Noul(f64),
}

impl Answer {
    /// The probability the judge gave the option it picked, for a choice answer.
    pub fn picked_probability(&self) -> Option<f64> {
        match self {
            Answer::Choice { choice, probabilities, .. } => probabilities.get(choice).copied(),
            Answer::Noul(_) => None,
        }
    }
}

/// Something that answers typed questions about a state. Questions asked together see the same
/// state and are answered independently.
pub trait Judge: Send + Sync {
    /// Model name, for logs.
    fn name(&self) -> &str;

    /// Ask `questions` (id, question) about `state`; answers are keyed by the same ids.
    fn ask(&self, state: &Value, questions: &[(&str, Question)]) -> Result<HashMap<String, Answer>>;
}
