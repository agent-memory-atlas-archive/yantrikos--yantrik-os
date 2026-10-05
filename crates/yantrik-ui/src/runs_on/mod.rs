//! What runs on what: for every mind on this desktop, the provider and model it runs on, where
//! that is set, and how the desktop knows. One answer, used by every surface that says it.
//!
//! VM 520, the day this was written: the Minds panel and Accounts said Claude was ACTIVE while no
//! mind ran on it, Ollama Cloud ran five of the six minds and showed nowhere, and the AI page
//! showed an endpoint above "No AI providers configured". Each surface had worked it out on its
//! own. They all ask here now, and every claim names its source.
//!
//! What may be read, and what never is:
//! - the built-in companion: the address the AI page already resolves (`ai_status::target`), a
//!   saved provider or config.yaml;
//! - every other mind: only what it said about itself when it attached (`detail`). A mind's own
//!   settings are its own: the Mind's live under another account, and a harness's hold its key.
//!   So those rows say "its own settings" and "as it reported", never more than was said.
//! - a vendor sign-in: no mind here runs on one (no shipped harness takes a sign-in). The summary
//!   says so; when one does, the mind's row will name the account.

pub mod identity;

#[cfg(test)]
mod tests;

use identity::ProviderRef;

/// What the attached minds said about themselves (from the harness host's list).
#[derive(Clone, Debug, PartialEq)]
pub struct MindFact {
    pub id: String,
    pub name: String,
    /// The mind's own line, as it attached. Never parsed for anything but provider and model.
    pub detail: Option<String>,
    pub answering: bool,
    pub builtin: bool,
}

/// What the built-in companion is pointed at: the AI page's own answer.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct CompanionFact {
    pub base_url: String,
    pub model: String,
    /// Where that is set: "config.yaml" or "saved provider".
    pub source: String,
    /// The saved provider's name, when it is one.
    pub provider_name: String,
    /// How the companion reaches its model: config.yaml's `llm.backend`, or the API when a saved
    /// provider is primary (Settings applies one as an API backend).
    pub backend: Backend,
    /// Where it turns when that fails, already said ("Ollama at localhost:8341", "llama.cpp in
    /// this process"): config.yaml's `llm.fallback` or a saved fallback provider. `None` when
    /// there is no fallback.
    pub fallback: Option<String>,
}

/// How the built-in companion reaches its model, from config.yaml's `llm.backend` — the same
/// split `bridge` makes when it builds the backend (`LLMConfig::is_api_backend`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Backend {
    /// An HTTP API at `base_url`: Ollama, vLLM, a cloud provider, a proxy.
    #[default]
    Api,
    /// A model loaded into this process (candle, llama.cpp): nothing goes over the network.
    InProcess,
    /// The Claude Code CLI, which sends what it is given to Anthropic whatever the URL says.
    ClaudeCli,
}

impl Backend {
    pub fn from_config(backend: &str) -> Self {
        match backend.trim().to_ascii_lowercase().as_str() {
            "candle" | "llamacpp" => Backend::InProcess,
            "claude-cli" | "claude_cli" => Backend::ClaudeCli,
            _ => Backend::Api,
        }
    }
}

/// An address as it was seen: "Ollama at localhost:11434", "Ollama Cloud", "10.0.0.2:8000". The
/// words a destination or a fallback is named in when nothing more can be said for it.
pub fn seen_at(base_url: &str) -> String {
    let host = identity::host(base_url);
    match identity::from_url(base_url) {
        ProviderRef::Known(p) if p.kind == yantrik_ml::ProviderKind::Local => format!("{} at {host}", p.display_name),
        ProviderRef::Known(p) => p.display_name.to_string(),
        ProviderRef::Local(h) | ProviderRef::Custom(h) => h,
        ProviderRef::NotReported => "an address not known".to_string(),
    }
}

/// config.yaml's `llm.fallback`, as the destination line names it: "llama.cpp in this process", or
/// the address of the API it falls back to.
pub fn fallback_label(backend: &str, api_base_url: Option<&str>) -> String {
    if Backend::from_config(backend) == Backend::InProcess {
        return "llama.cpp in this process".to_string();
    }
    match api_base_url.map(str::trim).filter(|u| !u.is_empty()) {
        Some(url) => seen_at(url),
        None => "a fallback not known".to_string(),
    }
}

/// A provider that forwards what it is sent somewhere else, so an address on this machine is no
/// promise about where the words end up: a LiteLLM proxy, and an Ollama model that runs on
/// Ollama's cloud (`*-cloud` / `*:cloud`) through the local daemon.
fn forwards_elsewhere(provider: &ProviderRef, model: Option<&str>) -> Option<String> {
    let ProviderRef::Known(p) = provider else { return None };
    match p.id {
        "litellm" => Some("a proxy that forwards it on".to_string()),
        "ollama" if model.is_some_and(|m| {
            let m = m.trim().to_ascii_lowercase();
            m.ends_with("-cloud") || m.ends_with(":cloud")
        }) => Some("Ollama Cloud".to_string()),
        _ => None,
    }
}

/// Where a mind's provider is set.
#[derive(Clone, Debug, PartialEq)]
pub enum SetIn {
    /// /opt/yantrik/config.yaml, the machine's file.
    ConfigYaml,
    /// A provider saved in Settings → AI & Intelligence, by its name.
    SavedProvider(String),
    /// The mind's own settings, which the desktop does not read.
    OwnSettings,
    /// Nothing is set: the companion has no address.
    Nowhere,
}

/// One mind and what it runs on.
#[derive(Clone, Debug, PartialEq)]
pub struct RunsOn {
    pub id: String,
    pub name: String,
    pub answering: bool,
    pub builtin: bool,
    pub provider: ProviderRef,
    pub model: Option<String>,
    pub set_in: SetIn,
    /// The facts came from the mind's own words, not from a file the desktop read.
    pub reported: bool,
    /// How the companion reaches its model; `Api` for every attached mind.
    pub backend: Backend,
    /// The companion's fallback, said; `None` for every attached mind.
    pub fallback: Option<String>,
    /// The companion's address as the desktop read it; empty for every attached mind.
    pub base_url: String,
}

impl RunsOn {
    /// "Ollama Cloud · deepseek-v4.1-flash"; "provider not reported · deepseek-v4.1-flash";
    /// "Nothing set up" when the companion has no address.
    pub fn runs_on(&self) -> String {
        let model = self.model.as_deref().filter(|m| !m.is_empty());
        let with_model = |what: &str| model.map_or(what.to_string(), |m| format!("{what} \u{b7} {m}"));
        match self.backend {
            Backend::ClaudeCli => return with_model("Anthropic (Claude CLI)"),
            Backend::InProcess => return with_model("This machine, in process"),
            Backend::Api => {}
        }
        if self.set_in == SetIn::Nowhere {
            return "Nothing set up".to_string();
        }
        match &self.model {
            Some(m) if !m.is_empty() => format!("{} \u{b7} {m}", self.provider.label()),
            _ => self.provider.label(),
        }
    }

    /// Where it is set, and how the desktop knows: "its own settings · as it reported";
    /// "/opt/yantrik/config.yaml"; "your saved provider “AIG”".
    pub fn source(&self) -> String {
        let place = match &self.set_in {
            SetIn::ConfigYaml => "set in /opt/yantrik/config.yaml".to_string(),
            SetIn::SavedProvider(name) => format!("your saved provider \u{201c}{name}\u{201d}"),
            SetIn::OwnSettings => "its own settings".to_string(),
            SetIn::Nowhere => "add a provider under Providers".to_string(),
        };
        if self.reported {
            format!("{place} \u{b7} as it reported")
        } else {
            place
        }
    }

    /// Where words typed to this mind go, for the row above the chat composer's Send:
    /// "Sends your message and conversation context to: Ollama Cloud · deepseek-v4.1-flash";
    /// "Stays on this machine · qwen3.5:9b". It says what leaves the machine (the message and the
    /// conversation sent with it; the composer carries no attachments) and to whom. Empty when the
    /// provider is not known: the line is an observed fact or nothing, never a guess (review of
    /// the UI overhaul by GPT-6 Astra, A).
    ///
    /// "Stays on this machine" is said only when it is certain (security review of #648, H1): a
    /// model in this process, or an API on this machine's loopback, read by the desktop itself
    /// and not a mind's own words, that is not a proxy forwarding it on, with no fallback that
    /// could take the words somewhere else. Anything short of that says what was seen.
    pub fn destination(&self) -> String {
        let to = |whom: String| format!("Sends your message and conversation context to: {whom}");
        let model = self.model.as_deref().filter(|m| !m.is_empty());
        let said = match (&self.backend, &self.provider) {
            // The CLI sends to Anthropic whatever address config.yaml also holds.
            (Backend::ClaudeCli, _) => to("Anthropic (Claude CLI)".to_string()),
            (Backend::InProcess, _) if self.fallback.is_none() => "Stays on this machine".to_string(),
            (Backend::InProcess, _) => "Runs in this process".to_string(),
            _ if self.set_in == SetIn::Nowhere => return String::new(),
            (_, ProviderRef::NotReported) => return String::new(),
            (_, provider) => match forwards_elsewhere(provider, model) {
                Some(on) if !self.base_url.is_empty() => to(format!("{on}, through {}", seen_at(&self.base_url))),
                Some(on) => to(on),
                None => match provider {
                    // A mind's own words ("ollama:qwen3.5:9b", "localhost:11434") could be about
                    // any machine, so no place is said for them.
                    _ if self.reported => to(match provider {
                        ProviderRef::Known(p) => p.display_name.to_string(),
                        ProviderRef::Local(h) | ProviderRef::Custom(h) => h.clone(),
                        ProviderRef::NotReported => return String::new(),
                    }),
                    ProviderRef::Known(p) if p.kind == yantrik_ml::ProviderKind::Local && self.fallback.is_none() => {
                        "Stays on this machine".to_string()
                    }
                    ProviderRef::Local(host) if identity::is_loopback(host) && self.fallback.is_none() => {
                        "Stays on this machine".to_string()
                    }
                    ProviderRef::Local(host) if !identity::is_loopback(host) => to(format!("{host}, on this network")),
                    _ if !self.base_url.is_empty() => to(seen_at(&self.base_url)),
                    ProviderRef::Known(p) => to(p.display_name.to_string()),
                    ProviderRef::Local(h) | ProviderRef::Custom(h) => to(h.clone()),
                    ProviderRef::NotReported => return String::new(),
                },
            },
        };
        let said = match model {
            Some(m) => format!("{said} \u{b7} {m}"),
            None => said,
        };
        match &self.fallback {
            Some(fb) => format!("{said} \u{b7} falls back to {fb}"),
            None => said,
        }
    }

    /// The word for its place in the chat: "Answering", "Built in", "Attached".
    pub fn state(&self) -> &'static str {
        if self.answering {
            "Answering"
        } else if self.builtin {
            "Built in"
        } else {
            "Attached"
        }
    }
}

/// Every mind and what it runs on: the answering one first, then the built-in companion, then the
/// rest by name. `minds` is the harness host's list; the built-in companion in it is described by
/// `companion` (its own `detail` is empty: its provider is the desktop's configuration).
pub fn resolve(minds: &[MindFact], companion: Option<&CompanionFact>) -> Vec<RunsOn> {
    let mut rows: Vec<RunsOn> = minds.iter().map(|m| one(m, companion)).collect();
    rows.sort_by(|a, b| {
        b.answering
            .cmp(&a.answering)
            .then(b.builtin.cmp(&a.builtin))
            .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    rows
}

fn one(m: &MindFact, companion: Option<&CompanionFact>) -> RunsOn {
    let base = |provider, model, set_in, reported| RunsOn {
        id: m.id.clone(),
        name: m.name.clone(),
        answering: m.answering,
        builtin: m.builtin,
        provider,
        model,
        set_in,
        reported,
        backend: Backend::Api,
        fallback: None,
        base_url: String::new(),
    };
    if m.builtin {
        return match companion {
            Some(c) if !c.base_url.trim().is_empty() || c.backend != Backend::Api => {
                let set_in = if c.source == "saved provider" {
                    SetIn::SavedProvider(c.provider_name.clone())
                } else {
                    SetIn::ConfigYaml
                };
                let model = Some(c.model.trim().to_string()).filter(|s| !s.is_empty());
                RunsOn {
                    backend: c.backend,
                    fallback: c.fallback.clone(),
                    base_url: c.base_url.trim().to_string(),
                    ..base(identity::from_url(&c.base_url), model, set_in, false)
                }
            }
            _ => base(ProviderRef::NotReported, None, SetIn::Nowhere, false),
        };
    }
    let (provider, model) = m.detail.as_deref().map(identity::from_reported).unwrap_or((ProviderRef::NotReported, None));
    base(provider, model, SetIn::OwnSettings, true)
}

/// One sentence under the map: what pays for the minds, and what does not.
/// "Ollama Cloud runs 4 minds. 1 mind does not say what it runs on. No mind here runs on a
/// signed-in account."
pub fn summary(rows: &[RunsOn]) -> String {
    let mut counts: Vec<(String, String, usize)> = Vec::new(); // key, label, minds
    for r in rows {
        if let Some(k) = r.provider.key() {
            match counts.iter_mut().find(|(key, _, _)| *key == k) {
                Some(c) => c.2 += 1,
                None => counts.push((k, r.provider.label(), 1)),
            }
        }
    }
    counts.sort_by(|a, b| b.2.cmp(&a.2).then(a.1.cmp(&b.1)));
    let mut parts: Vec<String> = counts
        .iter()
        .map(|(_, label, n)| format!("{label} runs {n} {}.", if *n == 1 { "mind" } else { "minds" }))
        .collect();
    let silent = rows
        .iter()
        .filter(|r| r.provider == ProviderRef::NotReported && r.set_in != SetIn::Nowhere && r.backend == Backend::Api)
        .count();
    if silent > 0 {
        parts.push(format!("{silent} {} not say what {} on.", if silent == 1 { "mind does" } else { "minds do" }, if silent == 1 { "it runs" } else { "they run" }));
    }
    parts.push("No mind here runs on a signed-in account.".to_string());
    parts.join(" ")
}

/// A provider the minds run on that is not saved in Settings, for the Providers list: so the list
/// never says "No AI providers configured" while minds plainly run on some.
#[derive(Clone, Debug, PartialEq)]
pub struct InUse {
    /// "Ollama Cloud", "Custom endpoint · aig.mycluster.cyou".
    pub label: String,
    /// The models the minds named, in order.
    pub models: Vec<String>,
    /// The minds that run on it.
    pub used_by: Vec<String>,
    /// Where it is set: "set in /opt/yantrik/config.yaml · used by the built-in companion", or
    /// "each mind keeps its own key in its own settings".
    pub where_set: String,
    /// What Add as provider fills in: the catalogue preset ("custom" for an address it does not
    /// know), the address when the desktop knows it, and the first model. Never a key.
    pub preset: String,
    pub base_url: String,
    pub model: String,
}

/// The providers in use that are not saved in Settings: everything `rows` names except what a
/// saved provider (by its address) already covers. The most-used first.
pub fn in_use_not_saved(rows: &[RunsOn], companion: Option<&CompanionFact>, saved_urls: &[String]) -> Vec<InUse> {
    let saved: Vec<String> = saved_urls.iter().filter_map(|u| identity::from_url(u).key()).collect();
    // key, entry, whether the companion's config.yaml address is one of its uses
    let mut out: Vec<(String, InUse, bool)> = Vec::new();
    for r in rows {
        let Some(key) = r.provider.key() else { continue };
        if saved.contains(&key) || matches!(r.set_in, SetIn::SavedProvider(_) | SetIn::Nowhere) {
            continue;
        }
        let i = match out.iter().position(|(k, _, _)| *k == key) {
            Some(i) => i,
            None => {
                let (preset, base_url) = match &r.provider {
                    ProviderRef::Known(p) => (p.id.to_string(), p.default_base_url.to_string()),
                    _ => ("custom".to_string(), String::new()),
                };
                let entry = InUse {
                    label: r.provider.label(),
                    models: Vec::new(),
                    used_by: Vec::new(),
                    where_set: String::new(),
                    preset,
                    base_url,
                    model: String::new(),
                };
                out.push((key, entry, false));
                out.len() - 1
            }
        };
        let (_, e, has_config) = &mut out[i];
        e.used_by.push(r.name.clone());
        if let Some(m) = r.model.as_ref().filter(|m| !m.is_empty()) {
            if !e.models.contains(m) {
                e.models.push(m.clone());
            }
        }
        if r.set_in == SetIn::ConfigYaml {
            *has_config = true;
            if let Some(c) = companion {
                e.base_url = c.base_url.clone();
            }
        }
    }
    let mut list: Vec<InUse> = out
        .into_iter()
        .map(|(_, mut e, has_config)| {
            e.model = e.models.first().cloned().unwrap_or_default();
            let others = e.used_by.len() - usize::from(has_config);
            e.where_set = match (has_config, others) {
                (true, 0) => "set in /opt/yantrik/config.yaml \u{b7} used by the built-in companion".to_string(),
                (true, _) => "set in /opt/yantrik/config.yaml for the built-in companion; the other minds keep their own key in their own settings".to_string(),
                (false, 1) => "kept in that mind's own settings, with its own key".to_string(),
                (false, _) => "each mind keeps its own key in its own settings".to_string(),
            };
            e
        })
        .collect();
    list.sort_by(|a, b| b.used_by.len().cmp(&a.used_by.len()).then(a.label.cmp(&b.label)));
    list
}
