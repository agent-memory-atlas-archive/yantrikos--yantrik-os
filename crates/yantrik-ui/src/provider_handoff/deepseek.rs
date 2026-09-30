//! The DeepSeek harness (harnesses/deepseek): any OpenAI-compatible endpoint, read from
//! ~/.config/yantrik/deepseek.json at mode 600. It takes `api_key` in that file directly, so
//! giving it a provider is one file: `base_url`, `model` and `api_key` set, `api_key_env` dropped
//! (the key is in the file now), and everything else the person put there — `decider`,
//! `max_steps`, `temperature` — left as it was.

use std::path::Path;

use serde_json::{Map, Value};

use super::{Handoff, Plan, Write};
use crate::wire::settings::ProviderStoreEntry;

pub(crate) struct DeepSeek;

pub(crate) const UNIT: &str = "yantrik-deepseek.service";

impl Handoff for DeepSeek {
    fn harness(&self) -> &'static str {
        "deepseek"
    }

    fn name(&self) -> &'static str {
        "DeepSeek"
    }

    fn plan(&self, home: &Path, provider: &ProviderStoreEntry) -> Result<Plan, String> {
        if provider.model.trim().is_empty() {
            return Err(format!(
                "{} has no model chosen yet. Open it under Providers, press Models, and pick one.",
                provider.name
            ));
        }
        let path = home.join(".config/yantrik/deepseek.json");
        let mut config: Map<String, Value> = match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<Value>(&text) {
                Ok(Value::Object(m)) => m,
                _ => return Err(format!("{} is not a JSON object, so it is left alone", path.display())),
            },
            Err(_) => Map::new(),
        };
        // The harness speaks OpenAI chat completions, so a provider saved at a native address
        // (Anthropic's, Gemini's) is given its OpenAI-compatible one.
        config.insert("base_url".into(), Value::String(crate::wire::provider_models::openai_base(&provider.base_url)));
        config.insert("model".into(), Value::String(provider.model.clone()));
        config.remove("api_key_env");
        match provider.api_key.as_deref().filter(|k| !k.is_empty()) {
            Some(key) => {
                config.insert("api_key".into(), Value::String(key.to_string()));
            }
            None => {
                config.remove("api_key");
            }
        }
        let content = serde_json::to_string_pretty(&Value::Object(config)).map_err(|e| e.to_string())? + "\n";
        Ok(Plan {
            harness: self.harness().into(),
            harness_name: self.name().into(),
            provider_id: provider.id.clone(),
            provider_name: provider.name.clone(),
            model: provider.model.clone(),
            writes: vec![Write {
                path,
                content,
                what: if provider.api_key.as_deref().is_some_and(|k| !k.is_empty()) {
                    "its address, model and key (the file only you can read)".into()
                } else {
                    "its address and model".into()
                },
            }],
            restart: Some(UNIT.into()),
        })
    }
}
