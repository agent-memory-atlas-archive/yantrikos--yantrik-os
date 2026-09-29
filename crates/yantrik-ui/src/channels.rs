//! What the desktop knows about each channel a person can reach it from, and how a message from
//! one is answered (design/channels-2026-09-29.md).
//!
//! A message from the person's phone goes to the mind answering, held so that a stolen phone is
//! not the person at the keyboard: the turn reads and changes nothing (`REMOTE_CEILING`, `safe`)
//! until P2 asks the person on the phone for anything more. Only a mind that can be held answers:
//!
//! - the built-in companion, which holds its own tools for the turn (`Turn::is_remote`);
//! - a mind running as its own account, whose every act carries its token (the mind door
//!   refuses one without), held through that token's reach on every door.
//!
//! A mind running as the person is not held by anything: it could drop its token, or use its own
//! shell. It does not answer a phone; the phone is told why.

use yantrik_harness::protocol::Origin;
use yantrik_harness::{Chunk, Turn};

use crate::agents::model::AgentId;

/// What the phone is told when the mind answering runs as the person.
fn unheld(name: &str) -> String {
    format!(
        "{name} runs as you on this desktop, so nothing can hold it to what a phone may ask. \
         Talk to it at the machine, or make the built-in companion or the Yantrik Mind the answering mind."
    )
}

/// Ask the mind answering, from the phone `asker` wrote on, and gather its answer. What the phone
/// is told when nothing came is a sentence, never an internal error: the channel's operator can
/// read it.
pub fn ask_from_phone(prompt: String, asker: &yantrik_chat::router::Asker) -> String {
    let Some(host) = crate::wire::harness::host() else {
        return "The desktop is still starting; ask again in a moment.".to_string();
    };
    let origin = Origin {
        channel: asker.provider.clone(),
        remote: true,
        person: asker.sender_name.clone(),
        carries: asker.carries.clone(),
        trust: trust_of(&asker.provider).to_string(),
    };
    let turn = Turn::new(prompt).with_origin(origin);
    let active = host.active_id();
    if active == crate::wire::harness::BUILTIN_ID {
        // By id, not "whoever is active": a switch in between does not redirect it.
        return match host.send_builtin(&active, turn) {
            Some(answer) => gather(answer),
            None => "The desktop's companion is not available right now.".to_string(),
        };
    }
    let name = host.list().into_iter().find(|e| e.id == active).map(|e| e.name).unwrap_or_else(|| active.clone());
    let own_account = host.attached_uid(&active).is_some_and(yantrik_ipc_transport::mind_door::is_mind);
    if !own_account {
        return unheld(&name);
    }
    // The agent made before it is held, and held before its turn is queued; the turn goes only to
    // the agent holding the token that was held.
    let agent: AgentId = match host.ensure_main(&active) {
        Ok(agent) => agent,
        Err(why) => {
            tracing::warn!(mind = %active, reason = %why, "a turn from a phone found no mind to answer");
            return format!("{name} is not attached right now.");
        }
    };
    let hold = match crate::agents::reaches::hold_remote(host, &agent) {
        Ok(hold) => hold,
        Err(why) => {
            tracing::error!(agent = %agent, reason = %why, "a turn from a phone was not sent");
            return "Nothing was sent: this mind could not be held to what a phone may ask.".to_string();
        }
    };
    let answer = match host.send_to_holding(&agent, turn, |token| hold.holds(token)) {
        Ok(answer) => answer,
        Err(why) => {
            tracing::warn!(agent = %agent, reason = %why, "a turn from a phone was not sent");
            return if why == crate::private_mode::PAUSED { why } else { format!("{name} could not take it right now.") };
        }
    };
    let said = gather(answer);
    drop(hold);
    said
}

/// The answer as one message, read the way the Lens reads a stream: the built-in companion's
/// `__DONE__`, `__REPLACE__` and run marks are conventions, not text. A failure is said as a
/// sentence; its detail goes to the log, not to a channel someone else may read.
fn gather(answer: yantrik_harness::Answer) -> String {
    let mut said = String::new();
    let mut replacing = false;
    while let Ok(chunk) = answer.recv() {
        match chunk {
            Chunk::Text(token) => {
                if token == "__DONE__" {
                    break;
                }
                if token.starts_with(crate::streaming::RUN_MARK) {
                    continue;
                }
                if let Some(rest) = token.strip_prefix("__REPLACE__") {
                    said.clear();
                    if rest.is_empty() {
                        replacing = true;
                    } else {
                        said.push_str(rest);
                    }
                    continue;
                }
                if replacing {
                    said = token;
                    replacing = false;
                } else {
                    said.push_str(&token);
                }
            }
            Chunk::Failed(why) => {
                if why == crate::private_mode::PAUSED {
                    return why;
                }
                tracing::warn!(reason = %why, "a turn from a phone failed");
                return "The answer did not come; it is on the desktop's log.".to_string();
            }
            Chunk::Event(_) => {}
        }
    }
    said
}

/// Who besides the person can read a channel: `e2e` when it is end-to-end to this box, else
/// `provider-readable` — the operator can read it, as Telegram can a bot's chats. An unknown
/// channel is the latter: saying a channel is private when it is not is the mistake that matters.
pub fn trust_of(provider: &str) -> &'static str {
    match provider {
        "signal" | "native" => "e2e",
        _ => "provider-readable",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(tokens: &[&str]) -> yantrik_harness::Answer {
        let (tx, rx) = std::sync::mpsc::channel();
        for t in tokens {
            tx.send(Chunk::Text((*t).to_string())).unwrap();
        }
        rx
    }

    #[test]
    fn the_companions_stream_is_read_as_the_lens_reads_it() {
        assert_eq!(gather(stream(&["Hello", ", Pranab", "__DONE__", "ignored"])), "Hello, Pranab");
        assert_eq!(gather(stream(&["draft", "__REPLACE__", "final", "__DONE__"])), "final");
        assert_eq!(gather(stream(&["x", "__REPLACE__Private mode is on", "__DONE__"])), "Private mode is on");
        assert_eq!(gather(stream(&["done", "__RUN__:pi:main#3", "__DONE__"])), "done");
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Chunk::Failed("/home/pranab/.config/secret path failed".into())).unwrap();
        assert!(!gather(rx).contains("/home"), "no internal detail goes to a channel");
    }

    #[test]
    fn only_end_to_end_channels_say_so() {
        assert_eq!(trust_of("signal"), "e2e");
        assert_eq!(trust_of("telegram"), "provider-readable");
        assert_eq!(trust_of("whatsapp"), "provider-readable", "the Cloud API is Meta-readable");
        assert_eq!(trust_of("carrier-pigeon"), "provider-readable");
    }
}
