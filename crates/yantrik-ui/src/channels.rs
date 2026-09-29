//! What the desktop knows about each channel a person can reach it from, how a message from one
//! is answered, and how a card is answered from the phone (design/channels-2026-09-29.md).
//!
//! **Who answers a phone.** A message from the person's phone goes to the mind answering, held
//! so that a stolen phone is not the person at the keyboard. Only a mind that can be held
//! answers:
//!
//! - the built-in companion, which holds its own tools to `Safe` for the turn (`Turn::is_remote`);
//! - a mind running as its own account, whose every act carries its token (the mind door refuses
//!   one without), held through that token's reach on every door: reads run, anything up to
//!   `sensitive` asks the person, nothing above runs (`reaches::hold_remote`).
//!
//! A mind running as the person is not held by anything: it could drop its token, or use its own
//! shell. It does not answer a phone; the phone is told why.
//!
//! **Answered later.** The router asks one message at a time, so a turn from the phone is answered
//! on a thread of its own and its answer sent when it comes ([`Outbox`]): a mind waiting on the
//! person's Allow must not hold up the very message that carries it.
//!
//! **Cards on the phone.** A card raised by an agent answering a phone turn is sent to that phone
//! with a one-time code: the person replies `ALLOW 123456` or `DENY 123456`, from the same identity
//! on the same channel, before the card expires, once. Never for an act the app says cannot be
//! undone — that waits for the machine — and not on a channel whose operator can read it unless the
//! person turned approvals on for it (`chat.phone_approvals`). The desktop's card stays on the
//! screen as well; whichever answer comes first decides. This is the one place other than the
//! card's own buttons that answers a card, and it answers only for the person: the words come
//! from the person's identity on the channel, which no mind can write as.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use yantrik_chat::model::ConversationRef;
use yantrik_chat::router::{Asker, Outbox};
use yantrik_harness::protocol::Origin;
use yantrik_harness::{Chunk, Turn};

use crate::agents::model::AgentId;

/// Who besides the person can read a channel: `e2e` when it is end-to-end to this box, else
/// `provider-readable` — the operator can read it, as Telegram can a bot's chats. An unknown
/// channel is the latter: saying a channel is private when it is not is the mistake that matters.
pub fn trust_of(provider: &str) -> &'static str {
    match provider {
        "signal" | "native" => "e2e",
        _ => "provider-readable",
    }
}

/// How long a code on the phone answers its card: about the card's own life on the desktop.
const CODE_LIFE: Duration = Duration::from_secs(110);

static OUTBOX: OnceLock<Outbox> = OnceLock::new();
/// The providers whose operator can read them on which the person still turned approvals on.
static APPROVALS_ON: OnceLock<Vec<String>> = OnceLock::new();

/// Where a phone turn came from, while its agent answers it.
#[derive(Clone)]
struct PhoneTurn {
    agent: String,
    mind: String,
    provider: String,
    sender_id: String,
    conversation: ConversationRef,
}

/// A card sent to a phone: the code that answers it, and who may send it.
struct PhoneCard {
    code: String,
    card_id: String,
    provider: String,
    sender_id: String,
    until: Instant,
}

static PHONE_TURNS: Mutex<Vec<PhoneTurn>> = Mutex::new(Vec::new());
static PHONE_CARDS: Mutex<Vec<PhoneCard>> = Mutex::new(Vec::new());

/// What the shell sends to a channel unasked, and which channels the person trusts with an Allow.
/// Called once the channels have started.
pub fn configure(outbox: Outbox, approvals_on: Vec<String>) {
    let _ = OUTBOX.set(outbox);
    let _ = APPROVALS_ON.set(approvals_on.into_iter().map(|p| p.trim().to_ascii_lowercase()).collect());
}

fn approvals_on(provider: &str) -> bool {
    trust_of(provider) == "e2e" || APPROVALS_ON.get().is_some_and(|on| on.iter().any(|p| p == provider))
}

/// The person wrote on a channel. An answer to a card is answered at once; anything else goes to
/// the mind answering, on a thread of its own, and its answer is sent when it comes.
pub fn from_phone(
    text: &str,
    context: &[String],
    max_reply: Option<usize>,
    asker: &Asker,
    outbox: &Outbox,
) -> Option<String> {
    if let Some(said) = card_answer(text, asker) {
        return Some(said);
    }
    let prompt = if context.is_empty() {
        text.to_string()
    } else {
        let history = context.iter().rev().take(6).rev().cloned().collect::<Vec<_>>().join("\n");
        format!("[Chat context]\n{history}\n\n[Latest message]\n{text}")
    };
    let asker = asker.clone();
    let outbox = outbox.clone();
    let spawned = std::thread::Builder::new().name("phone-turn".into()).spawn(move || {
        let mut said = ask_from_phone(prompt, &asker);
        if said.trim().is_empty() {
            return;
        }
        if let Some(max) = max_reply.filter(|m| said.len() > *m) {
            let at = said.floor_char_boundary(max.saturating_sub(3));
            said = format!("{}...", &said[..at]);
        }
        if let Err(why) = outbox.send(&asker.provider, &asker.conversation, &said) {
            tracing::warn!(provider = %asker.provider, reason = %why, "an answer to the phone could not be sent");
        }
    });
    if spawned.is_err() {
        return Some("The desktop could not take that right now; ask again in a moment.".to_string());
    }
    None
}

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
fn ask_from_phone(prompt: String, asker: &Asker) -> String {
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
    // Where a card this agent raises is to go while it answers.
    let _here = PhoneTurnGuard::enter(PhoneTurn {
        agent: agent.0.clone(),
        mind: name.clone(),
        provider: asker.provider.clone(),
        sender_id: asker.sender_id.clone(),
        conversation: asker.conversation.clone(),
    });
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

/// A phone turn's place in [`PHONE_TURNS`], taken out when the turn ends.
struct PhoneTurnGuard(String);

impl PhoneTurnGuard {
    fn enter(turn: PhoneTurn) -> PhoneTurnGuard {
        let agent = turn.agent.clone();
        PHONE_TURNS.lock().unwrap_or_else(|e| e.into_inner()).push(turn);
        PhoneTurnGuard(agent)
    }
}

impl Drop for PhoneTurnGuard {
    fn drop(&mut self) {
        let mut turns = PHONE_TURNS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(at) = turns.iter().position(|t| t.agent == self.0) {
            turns.remove(at);
        }
    }
}

/// A card was raised. When the agent it was raised for is answering a turn from the person's
/// phone, the phone is told — and, where the person may answer it there, given the code that
/// does. Sent on a thread of its own: this is called while a request is being answered.
pub fn card_raised(card_id: &str) {
    let Some(card) = crate::approvals::card(card_id) else { return };
    if card.verified.agent.is_empty() {
        return;
    }
    let Some(turn) = PHONE_TURNS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|t| t.agent == card.verified.agent)
        .cloned()
    else {
        return;
    };
    let Some(outbox) = OUTBOX.get().cloned() else { return };
    let what = crate::approvals::summary_of(&card.purpose);
    let said = if crate::approvals::unrecoverable(&card.purpose) {
        format!("{} asks to {}: {what}. That cannot be undone, so it waits for you at the machine.", turn.mind, card.action)
    } else if !approvals_on(&turn.provider) {
        format!(
            "{} asks to {}: {what}. It is waiting on the desktop's screen: approvals from {} are off, \
             since {} can read what is sent here.",
            turn.mind, card.action, turn.provider, turn.provider
        )
    } else {
        let Some(code) = fresh_code() else {
            tracing::error!("no randomness for a phone code; the card waits on the desktop");
            return;
        };
        PHONE_CARDS.lock().unwrap_or_else(|e| e.into_inner()).push(PhoneCard {
            code: code.clone(),
            card_id: card.id.clone(),
            provider: turn.provider.clone(),
            sender_id: turn.sender_id.clone(),
            until: Instant::now() + CODE_LIFE,
        });
        format!(
            "{} asks to {} on {}: {what}\nReply ALLOW {code} to let it, or DENY {code}. The code works once, \
             for about two minutes.",
            turn.mind, card.action, card.app
        )
    };
    let _ = std::thread::Builder::new().name("phone-card".into()).spawn(move || {
        if let Err(why) = outbox.send(&turn.provider, &turn.conversation, &said) {
            tracing::warn!(provider = %turn.provider, reason = %why, "a card could not be sent to the phone");
        }
    });
}

/// `ALLOW 123456` or `DENY 123456`, from the person the card was sent to, answers it once. `None`
/// for a message that is not such an answer, which goes to the mind as usual.
fn card_answer(text: &str, asker: &Asker) -> Option<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let [verb, code] = words.as_slice() else { return None };
    let allow = match verb.to_ascii_lowercase().as_str() {
        "allow" => true,
        "deny" => false,
        _ => return None,
    };
    if code.len() != 6 || !code.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let found = {
        let mut cards = PHONE_CARDS.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        cards.retain(|c| c.until > now);
        let at = cards.iter().position(|c| c.code == *code && c.provider == asker.provider && c.sender_id == asker.sender_id);
        at.map(|at| cards.remove(at))
    };
    let Some(card) = found else {
        return Some("No card is waiting for that code: it was answered, or it expired.".to_string());
    };
    let decided = if allow { crate::approvals::grant(&card.card_id) } else { crate::approvals::deny(&card.card_id) };
    Some(match decided {
        Ok(()) => {
            tracing::warn!(card = %card.card_id, provider = %card.provider, allow, "a card was answered from the phone");
            if allow { "Allowed.".to_string() } else { "Denied.".to_string() }
        }
        Err(why) => {
            tracing::info!(card = %card.card_id, reason = %why, "a phone's answer did not apply");
            "That card was already answered or has gone.".to_string()
        }
    })
}

/// Six digits from the kernel's randomness, or `None` without it.
fn fresh_code() -> Option<String> {
    use std::io::Read;
    let mut bytes = [0u8; 4];
    std::fs::File::open("/dev/urandom").ok()?.read_exact(&mut bytes).ok()?;
    Some(format!("{:06}", u32::from_le_bytes(bytes) % 1_000_000))
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

    fn asker(provider: &str, sender: &str) -> Asker {
        Asker {
            provider: provider.into(),
            sender_name: "Pranab".into(),
            sender_id: sender.into(),
            carries: vec!["text".into()],
            conversation: ConversationRef::direct(provider, sender),
        }
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
    fn a_code_answers_only_its_card_only_from_its_person_and_only_once() {
        PHONE_CARDS.lock().unwrap().push(PhoneCard {
            code: "314159".into(),
            card_id: "appr-not-in-the-store".into(),
            provider: "signal".into(),
            sender_id: "+15550001".into(),
            until: Instant::now() + CODE_LIFE,
        });
        assert!(card_answer("what is on my screen", &asker("signal", "+15550001")).is_none(), "not an answer: to the mind");
        assert!(card_answer("allow 31415", &asker("signal", "+15550001")).is_none(), "five digits is not a code");
        let stranger = card_answer("ALLOW 314159", &asker("signal", "+15559999")).unwrap();
        assert!(stranger.starts_with("No card is waiting"), "{stranger}");
        let other_channel = card_answer("allow 314159", &asker("telegram", "+15550001")).unwrap();
        assert!(other_channel.starts_with("No card is waiting"), "{other_channel}");
        let first = card_answer("allow 314159", &asker("signal", "+15550001")).unwrap();
        assert_eq!(first, "That card was already answered or has gone.", "the code was taken; the card itself is not in this test's store");
        let again = card_answer("allow 314159", &asker("signal", "+15550001")).unwrap();
        assert!(again.starts_with("No card is waiting"), "once: {again}");
    }

    #[test]
    fn an_expired_code_answers_nothing() {
        PHONE_CARDS.lock().unwrap().push(PhoneCard {
            code: "271828".into(),
            card_id: "appr-x".into(),
            provider: "signal".into(),
            sender_id: "+15550002".into(),
            until: Instant::now() - Duration::from_secs(1),
        });
        let said = card_answer("allow 271828", &asker("signal", "+15550002")).unwrap();
        assert!(said.starts_with("No card is waiting"), "{said}");
    }

    #[test]
    fn a_code_is_six_digits_from_the_kernel() {
        let code = fresh_code().expect("urandom");
        assert!(code.len() == 6 && code.chars().all(|c| c.is_ascii_digit()), "{code}");
    }

    #[test]
    fn only_end_to_end_channels_say_so() {
        assert_eq!(trust_of("signal"), "e2e");
        assert_eq!(trust_of("telegram"), "provider-readable");
        assert_eq!(trust_of("whatsapp"), "provider-readable", "the Cloud API is Meta-readable");
        assert_eq!(trust_of("carrier-pigeon"), "provider-readable");
    }
}
