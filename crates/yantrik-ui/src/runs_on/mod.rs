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
#[derive(Clone, Debug, PartialEq)]
pub struct CompanionFact {
    pub base_url: String,
    pub model: String,
    /// Where that is set: "config.yaml" or "saved provider".
    pub source: String,
    /// The saved provider's name, when it is one.
    pub provider_name: String,
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
}

impl RunsOn {
    /// "Ollama Cloud · deepseek-v4.1-flash"; "provider not reported · deepseek-v4.1-flash";
    /// "Nothing set up" when the companion has no address.
    pub fn runs_on(&self) -> String {
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

    /// Where words typed to this mind go, for the line under the chat composer:
    /// "Yantrik Mind · deepseek-v4.1-flash · online, via Ollama Cloud";
    /// "Yantrik Companion · qwen3.5:9b · on this machine". Empty when the provider is not known:
    /// the line is an observed fact or nothing, never a guess.
    pub fn destination(&self) -> String {
        let place = match &self.provider {
            _ if self.set_in == SetIn::Nowhere => return String::new(),
            ProviderRef::NotReported => return String::new(),
            // A local runtime named by the desktop's own address is this machine. Named only in a
            // mind's words ("ollama:qwen3.5:9b") it could be on any machine, so no place is said.
            ProviderRef::Known(p) if p.kind == yantrik_ml::ProviderKind::Local && self.reported => format!("via {}", p.display_name),
            ProviderRef::Known(p) if p.kind == yantrik_ml::ProviderKind::Local => "on this machine".to_string(),
            ProviderRef::Known(p) => format!("online, via {}", p.display_name),
            ProviderRef::Local(host) if identity::is_loopback(host) => "on this machine".to_string(),
            ProviderRef::Local(host) => format!("on this network, at {host}"),
            ProviderRef::Custom(host) => format!("online, via {host}"),
        };
        match self.model.as_deref().filter(|m| !m.is_empty()) {
            Some(m) => format!("{} \u{b7} {m} \u{b7} {place}", self.name),
            None => format!("{} \u{b7} {place}", self.name),
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
    };
    if m.builtin {
        return match companion {
            Some(c) if !c.base_url.trim().is_empty() => {
                let set_in = if c.source == "saved provider" {
                    SetIn::SavedProvider(c.provider_name.clone())
                } else {
                    SetIn::ConfigYaml
                };
                let model = Some(c.model.trim().to_string()).filter(|s| !s.is_empty());
                base(identity::from_url(&c.base_url), model, set_in, false)
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
    let silent = rows.iter().filter(|r| r.provider == ProviderRef::NotReported && r.set_in != SetIn::Nowhere).count();
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
