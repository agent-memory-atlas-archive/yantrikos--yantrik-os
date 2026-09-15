//! What a harness looks like on disk.
//!
//! One YAML file per harness, in `/etc/yantrik/harnesses/` for the ones an image ships and
//! `~/.config/yantrik/harnesses/` for the ones a person adds. YAML rather than TOML because the
//! rest of this OS is already YAML — `config/yantrik-os.yaml`, `~/.config/yantrik/settings.yaml`
//! — and a second config language would be a thing to explain forever.
//!
//! ```yaml
//! id: mind
//! name: Yantrik Mind
//! kind: openai-http
//! endpoint: http://192.168.4.66:8080/v1
//! model: qwen2.5
//! # Never the key itself: the name of the variable holding it.
//! api_key_env: YANTRIK_MIND_KEY
//! ```
//!
//! # Why a key is a variable name
//!
//! `api_key_env` names an environment variable; there is no field that takes a secret directly.
//! A config file gets copied into a bug report, read over a shoulder and committed to a repo, and
//! a field that accepts a key is a field that will eventually hold one. The indirection costs a
//! line and removes the whole class.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Which adapter drives this harness.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// Compiled into the shell — the companion. Cannot be declared by a config file; the file
    /// would have nothing to point at.
    Builtin,
    /// Anything serving `POST /v1/chat/completions`: yantrik-mind, hermes-agent, Ollama, vLLM,
    /// llama.cpp. This is the one most new harnesses want.
    OpenaiHttp,
    /// A subprocess speaking line-delimited JSON on stdin/stdout.
    Stdio,
}

impl Kind {
    /// The spellings a config file may use, for the error when it uses another.
    pub const NAMES: &'static [&'static str] = &["builtin", "openai-http", "stdio"];

    /// The name as it is written in a file.
    ///
    /// Not `{:?}`, which renders `OpenaiHttp` and lowercases to `openaihttp` — a spelling no
    /// config file would be accepted with, shown back to the person who has to write one.
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Builtin => "builtin",
            Kind::OpenaiHttp => "openai-http",
            Kind::Stdio => "stdio",
        }
    }
}

/// One harness, as declared.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Spec {
    /// Stable id. Also the name used with `set_active`, so it is what a person types.
    pub id: String,
    /// Shown in the picker.
    pub name: String,
    pub kind: Kind,
    /// Base URL for `openai-http`, e.g. `http://host:8080/v1`.
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// The NAME of an environment variable holding the key. Never the key.
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// Argv for `stdio`.
    #[serde(default)]
    pub command: Vec<String>,
    /// Off without being deleted — the way to park a harness you are not using.
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

/// Everything wrong with a spec, said in terms of the file a person has to edit.
#[derive(Debug, PartialEq, Eq)]
pub enum SpecError {
    Unreadable(String),
    Malformed(String),
    Incomplete(String),
}

impl std::fmt::Display for SpecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpecError::Unreadable(why) | SpecError::Malformed(why) | SpecError::Incomplete(why) => {
                write!(f, "{why}")
            }
        }
    }
}

impl Spec {
    /// Parse one file's contents.
    pub fn parse(yaml: &str, source: &str) -> Result<Spec, SpecError> {
        let spec: Spec = serde_yaml::from_str(yaml).map_err(|e| {
            // serde's "unknown variant" for `kind` is the error a person is most likely to hit,
            // and on its own it does not say what the valid spellings are.
            let detail = e.to_string();
            if detail.contains("unknown variant") {
                SpecError::Malformed(format!(
                    "{source}: `kind` must be one of {}, not what is there ({detail})",
                    Kind::NAMES.join(", ")
                ))
            } else {
                SpecError::Malformed(format!("{source}: {detail}"))
            }
        })?;
        spec.validate(source)?;
        Ok(spec)
    }

    /// Whether this spec has what its kind actually needs.
    ///
    /// Checked when it is read rather than when it is first used, so a typo surfaces in the
    /// harness list with a reason attached instead of as silence the first time someone asks it a
    /// question.
    pub fn validate(&self, source: &str) -> Result<(), SpecError> {
        if self.id.trim().is_empty() {
            return Err(SpecError::Incomplete(format!("{source}: `id` is empty")));
        }
        if self.id.contains(char::is_whitespace) {
            return Err(SpecError::Incomplete(format!(
                "{source}: `id` is what you type to select this harness, so it cannot contain spaces (`{}`)",
                self.id
            )));
        }
        match self.kind {
            Kind::OpenaiHttp => {
                if self.endpoint.as_deref().unwrap_or("").trim().is_empty() {
                    return Err(SpecError::Incomplete(format!(
                        "{source}: `openai-http` needs an `endpoint`, e.g. http://host:8080/v1"
                    )));
                }
            }
            Kind::Stdio => {
                if self.command.is_empty() {
                    return Err(SpecError::Incomplete(format!(
                        "{source}: `stdio` needs a `command` to run"
                    )));
                }
            }
            Kind::Builtin => {
                return Err(SpecError::Incomplete(format!(
                    "{source}: `builtin` harnesses are compiled in and cannot be declared in a file — \
                     there would be nothing for this to point at"
                )));
            }
        }
        Ok(())
    }

    /// Read one spec file.
    pub fn read(path: &Path) -> Result<Spec, SpecError> {
        let source = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let text = std::fs::read_to_string(path)
            .map_err(|e| SpecError::Unreadable(format!("{source}: {e}")))?;
        Spec::parse(&text, &source)
    }

    /// The key this spec points at, if the variable is set.
    pub fn api_key(&self) -> Option<String> {
        self.api_key_env.as_ref().and_then(|name| std::env::var(name).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_harness_a_person_would_write() {
        let spec = Spec::parse(
            "id: mind\nname: Yantrik Mind\nkind: openai-http\nendpoint: http://x:8080/v1\nmodel: qwen2.5\n",
            "mind.yaml",
        )
        .unwrap();
        assert_eq!(spec.id, "mind");
        assert_eq!(spec.kind, Kind::OpenaiHttp);
        assert_eq!(spec.model.as_deref(), Some("qwen2.5"));
        // Absent means on: a file that exists is a harness someone wanted.
        assert!(spec.enabled);
    }

    #[test]
    fn a_misspelled_kind_lists_the_real_ones() {
        let err = Spec::parse("id: x\nname: X\nkind: http\n", "x.yaml").unwrap_err();
        let message = err.to_string();
        for name in Kind::NAMES {
            assert!(message.contains(name), "{message}");
        }
    }

    #[test]
    fn an_http_harness_without_an_endpoint_says_so_when_it_is_read() {
        // Not when it is first asked a question, which would look like the harness ignoring you.
        let err = Spec::parse("id: x\nname: X\nkind: openai-http\n", "x.yaml").unwrap_err();
        assert!(err.to_string().contains("needs an `endpoint`"), "{err}");
    }

    #[test]
    fn a_stdio_harness_without_a_command_says_so() {
        let err = Spec::parse("id: x\nname: X\nkind: stdio\n", "x.yaml").unwrap_err();
        assert!(err.to_string().contains("needs a `command`"), "{err}");
    }

    #[test]
    fn a_file_cannot_declare_a_builtin() {
        let err = Spec::parse("id: companion\nname: Companion\nkind: builtin\n", "c.yaml")
            .unwrap_err();
        assert!(err.to_string().contains("compiled in"), "{err}");
    }

    #[test]
    fn an_id_with_a_space_is_refused_because_it_is_what_you_type() {
        let err = Spec::parse(
            "id: my mind\nname: X\nkind: openai-http\nendpoint: http://x/v1\n",
            "x.yaml",
        )
        .unwrap_err();
        assert!(err.to_string().contains("cannot contain spaces"), "{err}");
    }

    #[test]
    fn there_is_no_way_to_put_a_secret_in_the_file() {
        // The struct has no field for one. If this ever fails to compile because someone added
        // `api_key`, that is the point: the indirection is the feature.
        let spec = Spec::parse(
            "id: x\nname: X\nkind: openai-http\nendpoint: http://x/v1\napi_key_env: SOME_VAR\n",
            "x.yaml",
        )
        .unwrap();
        assert_eq!(spec.api_key_env.as_deref(), Some("SOME_VAR"));
        std::env::remove_var("SOME_VAR");
        assert_eq!(spec.api_key(), None, "an unset variable is not a key");
    }

    #[test]
    fn a_kind_is_shown_the_way_it_would_be_written() {
        // The UI shows this back to the person who has to type it into a file, so it has to be a
        // spelling a file would actually be accepted with.
        assert_eq!(Kind::OpenaiHttp.as_str(), "openai-http");
        assert_eq!(Kind::Stdio.as_str(), "stdio");
        for name in Kind::NAMES {
            assert!(
                [Kind::Builtin.as_str(), Kind::OpenaiHttp.as_str(), Kind::Stdio.as_str()]
                    .contains(name),
                "{name} is offered in errors but is not a spelling as_str produces"
            );
        }
    }

    #[test]
    fn a_parked_harness_stays_declared() {
        let spec = Spec::parse(
            "id: x\nname: X\nkind: stdio\ncommand: [/usr/bin/agent]\nenabled: false\n",
            "x.yaml",
        )
        .unwrap();
        assert!(!spec.enabled);
        assert_eq!(spec.command, vec!["/usr/bin/agent".to_string()]);
    }
}
