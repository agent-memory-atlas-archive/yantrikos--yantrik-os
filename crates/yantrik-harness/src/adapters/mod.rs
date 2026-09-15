//! The adapters a declared harness can be driven by.
//!
//! Two, deliberately, and they were chosen by looking at what the harnesses on this network
//! actually speak rather than at what could be supported. `yantrik-mind` serves
//! `POST /v1/chat/completions`; so does hermes-agent, and Ollama, and vLLM, and llama.cpp. That
//! one shape covers nearly everything that will turn up, which is why adding a harness is usually
//! a YAML file.
//!
//! A third would be justified by a harness that speaks neither — not by symmetry.

pub mod openai_http;
pub mod stdio;

use std::sync::Arc;

use crate::spec::{Kind, Spec};
use crate::Harness;

/// Build the harness a spec describes.
///
/// `None` for `builtin`, which by definition is not built from a file — the shell hands those in.
pub fn build(spec: &Spec) -> Option<Arc<dyn Harness>> {
    match spec.kind {
        Kind::OpenaiHttp => Some(Arc::new(openai_http::OpenAiHttp::new(spec.clone()))),
        Kind::Stdio => Some(Arc::new(stdio::Stdio::new(spec.clone()))),
        Kind::Builtin => None,
    }
}
