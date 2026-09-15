//! A subprocess that speaks line-delimited JSON.
//!
//! The other shape agent harnesses come in. A CLI is handed the turn as one JSON object on stdin
//! and answers with one object per line on stdout:
//!
//! ```text
//! →  {"text":"what is on my calendar?","context":null}
//! ←  {"delta":"You have "}
//! ←  {"delta":"one meeting."}
//! ←  {"done":true}
//! ```
//!
//! An `{"error":"..."}` line ends the turn as a failure. A process that writes plain text rather
//! than JSON is read as text, because a wrapper script that just echoes its model's output is the
//! first thing anyone will try and it should work.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio as ProcessStdio};
use std::sync::mpsc;

use crate::spec::Spec;
use crate::{Answer, Capabilities, Chunk, Harness, Health, Turn};

pub struct Stdio {
    spec: Spec,
}

impl Stdio {
    pub fn new(spec: Spec) -> Self {
        Self { spec }
    }
}

impl Harness for Stdio {
    fn id(&self) -> &str {
        &self.spec.id
    }

    fn name(&self) -> &str {
        &self.spec.name
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { streaming: true, tools: false, memory: false }
    }

    fn health(&self) -> Health {
        let Some(program) = self.spec.command.first() else {
            return Health::NotConfigured("no command".into());
        };
        // Whether the binary is there, not whether it works — running an agent to find out if it
        // can be run is not a health check, it is a turn.
        if program.contains('/') {
            if std::path::Path::new(program).is_file() {
                Health::Ready
            } else {
                Health::NotConfigured(format!("{program} is not there"))
            }
        } else if which(program).is_some() {
            Health::Ready
        } else {
            Health::NotConfigured(format!("{program} is not on PATH"))
        }
    }

    fn send(&self, turn: Turn) -> Answer {
        let (tx, rx) = mpsc::channel();
        let command = self.spec.command.clone();
        let id = self.spec.id.clone();

        std::thread::Builder::new()
            .name(format!("harness-{id}"))
            .spawn(move || {
                let Some((program, args)) = command.split_first() else {
                    let _ = tx.send(Chunk::Failed("no command configured".into()));
                    return;
                };
                let child = Command::new(program)
                    .args(args)
                    .stdin(ProcessStdio::piped())
                    .stdout(ProcessStdio::piped())
                    .stderr(ProcessStdio::piped())
                    .spawn();
                let mut child = match child {
                    Ok(child) => child,
                    Err(e) => {
                        let _ = tx.send(Chunk::Failed(format!("cannot start {program}: {e}")));
                        return;
                    }
                };

                if let Some(mut stdin) = child.stdin.take() {
                    let payload = serde_json::json!({
                        "text": turn.text,
                        "context": turn.context,
                    });
                    let _ = writeln!(stdin, "{payload}");
                    // Closing stdin is what tells a well-behaved CLI the turn is complete;
                    // without it, one that reads to EOF waits forever and so do we.
                    drop(stdin);
                }

                let Some(stdout) = child.stdout.take() else {
                    let _ = tx.send(Chunk::Failed("the process produced no output".into()));
                    return;
                };

                let mut said_anything = false;
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    match parse_line(&line) {
                        Some(Chunk::Text(text)) => {
                            said_anything = true;
                            if tx.send(Chunk::Text(text)).is_err() {
                                let _ = child.kill();
                                return;
                            }
                        }
                        Some(Chunk::Failed(why)) => {
                            let _ = tx.send(Chunk::Failed(why));
                            let _ = child.wait();
                            return;
                        }
                        None => break, // `done`
                    }
                }

                match child.wait() {
                    Ok(status) if status.success() => {
                        if !said_anything {
                            let _ = tx.send(Chunk::Failed("the harness said nothing".into()));
                        }
                    }
                    Ok(status) => {
                        // stderr is where a CLI explains itself, and dropping it would leave a
                        // person with an exit code and nothing else.
                        let mut detail = String::new();
                        if let Some(mut stderr) = child.stderr.take() {
                            use std::io::Read;
                            let _ = stderr.read_to_string(&mut detail);
                        }
                        let detail = detail.trim();
                        let code = status.code().unwrap_or(-1);
                        let _ = tx.send(Chunk::Failed(if detail.is_empty() {
                            format!("{program} exited {code}")
                        } else {
                            format!("{program} exited {code}: {detail}")
                        }));
                    }
                    Err(e) => {
                        let _ = tx.send(Chunk::Failed(format!("{program}: {e}")));
                    }
                }
            })
            .ok();

        rx
    }
}

/// One line of output. `None` means the harness said it was done.
pub fn parse_line(line: &str) -> Option<Chunk> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Some(Chunk::Text(String::new()));
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        // Not JSON: a wrapper script echoing its model's output. Take it as text rather than
        // failing, because that script is the first thing anyone writes.
        return Some(Chunk::Text(format!("{line}\n")));
    };
    if value["done"].as_bool() == Some(true) {
        return None;
    }
    if let Some(error) = value["error"].as_str() {
        return Some(Chunk::Failed(error.to_string()));
    }
    let text = value["delta"]
        .as_str()
        .or_else(|| value["text"].as_str())
        .or_else(|| value["content"].as_str())
        .unwrap_or("");
    Some(Chunk::Text(text.to_string()))
}

/// Whether a bare program name is on PATH.
fn which(program: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(program)).find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::Kind;

    fn spec(command: Vec<&str>) -> Spec {
        Spec {
            id: "cli".into(),
            name: "CLI".into(),
            kind: Kind::Stdio,
            endpoint: None,
            model: None,
            api_key_env: None,
            command: command.into_iter().map(String::from).collect(),
            enabled: true,
        }
    }

    #[test]
    fn reads_the_deltas_a_cli_writes() {
        assert_eq!(
            parse_line(r#"{"delta":"Hello"}"#),
            Some(Chunk::Text("Hello".into()))
        );
        assert_eq!(parse_line(r#"{"text":"Hello"}"#), Some(Chunk::Text("Hello".into())));
        assert_eq!(parse_line(r#"{"done":true}"#), None);
    }

    #[test]
    fn an_error_line_ends_the_turn_as_a_failure() {
        assert_eq!(
            parse_line(r#"{"error":"no model loaded"}"#),
            Some(Chunk::Failed("no model loaded".into()))
        );
    }

    #[test]
    fn a_script_that_just_echoes_text_still_works() {
        // The first thing anyone writes is a shell wrapper that prints its model's output. It
        // should not have to learn a protocol to be usable.
        assert_eq!(
            parse_line("just some prose"),
            Some(Chunk::Text("just some prose\n".into()))
        );
    }

    #[test]
    fn a_missing_binary_is_a_configuration_problem_not_a_failed_turn() {
        let health = Stdio::new(spec(vec!["/definitely/not/here/agent"])).health();
        match health {
            Health::NotConfigured(why) => assert!(why.contains("is not there"), "{why}"),
            other => panic!("expected NotConfigured, got {other:?}"),
        }
    }

    #[test]
    fn no_command_at_all_says_so() {
        assert_eq!(Stdio::new(spec(vec![])).health(), Health::NotConfigured("no command".into()));
    }

    #[cfg(unix)]
    #[test]
    fn runs_a_real_process_and_streams_what_it_writes() {
        let harness = Stdio::new(spec(vec![
            "/bin/sh",
            "-c",
            r#"echo '{"delta":"Hello "}'; echo '{"delta":"world"}'; echo '{"done":true}'"#,
        ]));
        assert_eq!(crate::collect(harness.send(Turn::new("hi"))).unwrap(), "Hello world");
    }

    #[cfg(unix)]
    #[test]
    fn a_process_that_fails_explains_itself_with_its_stderr() {
        let harness = Stdio::new(spec(vec!["/bin/sh", "-c", "echo 'no model' >&2; exit 3"]));
        let err = crate::collect(harness.send(Turn::new("hi"))).unwrap_err();
        assert!(err.contains("exited 3"), "{err}");
        assert!(err.contains("no model"), "{err}");
    }
}
