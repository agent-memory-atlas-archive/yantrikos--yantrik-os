//! Feeding the store from what a turn streams back.
//!
//! Every turn the shell sends an attached mind — from the Lens, from New agent, from an agent's
//! own prompt box — is read here as it streams, into that agent: its text, and its
//! `Chunk::Event`s, which go to the store as *reported*. A harness that writes only text still
//! gets cards: every `⚙️` trail line becomes a reported card, read by #125's one reader of the
//! trail (`crate::trail`), claiming no outcome because the trail does not say one. A harness that
//! sends both writes the line just before the event (docs/harness.md); the store lets the event
//! take the line's place and ignores the trail for the rest of that turn, so no call is shown
//! twice.

use std::sync::mpsc;

use yantrik_harness::{Answer, Chunk};

use super::model::{AgentId, AgentMeta, Provenance};
use crate::trail::{self, ToolCall};

/// One piece of an answer, sorted.
#[derive(Clone, Debug, PartialEq)]
pub enum Piece {
    Text(String),
    Call(ToolCall),
}

/// Splits streamed text into prose and trail calls, as it arrives.
///
/// A trail line has to be whole before it can be read, and prose should not wait for anything. So
/// a line is held only while it could still be a call — until its first visible character, and if
/// that is the gear, until its end. Hermes' verbose form puts a call's arguments on the line after
/// it, so a call written as `name([...])` waits one more line for them. A gear inside a code fence
/// is code.
#[derive(Debug, Default)]
pub struct TrailLines {
    pending: String,
    /// The current line is known to be prose and is passed through as it comes.
    prose: bool,
    /// A verbose call waiting for its arguments on the next line.
    held: Option<String>,
    fenced: bool,
}

impl TrailLines {
    pub fn push(&mut self, delta: &str) -> Vec<Piece> {
        let mut out = Vec::new();
        for segment in delta.split_inclusive('\n') {
            let whole = segment.ends_with('\n');
            if self.prose {
                out.push(Piece::Text(segment.to_string()));
                if whole {
                    self.prose = false;
                    self.note_fence(segment);
                }
                continue;
            }
            self.pending.push_str(segment);
            if whole {
                let line = std::mem::take(&mut self.pending);
                self.line(line, &mut out);
            } else if self.held.is_none() {
                let visible = self.pending.trim_start();
                if !visible.is_empty() && (self.fenced || !visible.starts_with('\u{2699}')) {
                    // Not a call: say it now and keep saying it until the line ends.
                    out.push(Piece::Text(std::mem::take(&mut self.pending)));
                    self.prose = true;
                }
            }
        }
        out
    }

    /// The answer ended: whatever was held is what it is.
    pub fn finish(&mut self) -> Vec<Piece> {
        let mut out = Vec::new();
        if let Some(held) = self.held.take() {
            if let Some((call, _)) = trail::parse(&held, None) {
                out.push(Piece::Call(call));
            }
        }
        let rest = std::mem::take(&mut self.pending);
        if !rest.is_empty() {
            self.classify(rest, &mut out);
        }
        self.prose = false;
        out
    }

    fn line(&mut self, line: String, out: &mut Vec<Piece>) {
        if let Some(held) = self.held.take() {
            let next = line.trim_end_matches(['\n', '\r']);
            if let Some((call, took)) = trail::parse(&held, Some(next)) {
                out.push(Piece::Call(call));
                if took {
                    return;
                }
            }
        }
        self.classify(line, out);
    }

    fn classify(&mut self, line: String, out: &mut Vec<Piece>) {
        if !self.fenced && trail::is_trail(&line) {
            let bare = line.trim_end_matches(['\n', '\r']);
            if verbose(bare) && line.ends_with('\n') {
                self.held = Some(bare.to_string());
                return;
            }
            if let Some((call, _)) = trail::parse(bare, None) {
                out.push(Piece::Call(call));
                return;
            }
        }
        self.note_fence(&line);
        out.push(Piece::Text(line));
    }

    fn note_fence(&mut self, line: &str) {
        if line.trim_start().starts_with("```") {
            self.fenced = !self.fenced;
        }
    }
}

/// Hermes' verbose form, `⚙️ name(['app', 'action'])`: its arguments are on the next line.
fn verbose(line: &str) -> bool {
    let rest = line.trim_start().trim_start_matches('\u{2699}').trim_start_matches('\u{fe0f}').trim();
    match rest.split_once('(') {
        Some((name, keys)) => !name.trim().is_empty() && !name.contains(' ') && keys.trim_end().ends_with(')'),
        None => false,
    }
}

/// Reads one answer into one agent.
struct Reader {
    agent: AgentId,
    lines: TrailLines,
    /// The built-in companion speaks the chat pump's token protocol: `__DONE__` and `__REPLACE__`
    /// are instructions, not text. From anything else they are text.
    builtin: bool,
    replace_next: bool,
    failed: bool,
}

impl Reader {
    fn new(agent: AgentId, builtin: bool) -> Self {
        Reader { agent, lines: TrailLines::default(), builtin, replace_next: false, failed: false }
    }

    fn chunk(&mut self, chunk: &Chunk) {
        let store = super::store();
        #[allow(unreachable_patterns)]
        match chunk {
            Chunk::Text(text) => {
                if self.builtin {
                    match text.as_str() {
                        "__DONE__" => return,
                        "__REPLACE__" => {
                            self.replace_next = true;
                            return;
                        }
                        _ if self.replace_next => {
                            self.replace_next = false;
                            self.lines = TrailLines::default();
                            store.replace_text(&self.agent, text);
                            return;
                        }
                        _ => {}
                    }
                }
                for piece in self.lines.push(text) {
                    self.apply(piece);
                }
            }
            Chunk::Failed(why) => {
                for piece in self.lines.finish() {
                    self.apply(piece);
                }
                store.note(&self.agent, &format!("The turn failed: {why}"));
                self.failed = true;
            }
            // What the agent is doing, beside the text: a card opening, its output, its end. The
            // harness's own account, so reported — and once a turn has them, the store stops
            // making cards out of the trail lines the harness still writes for the chat.
            Chunk::Event(event) => store.event(&self.agent, event, Provenance::Reported),
            _ => {}
        }
    }

    fn apply(&self, piece: Piece) {
        match piece {
            Piece::Text(text) => super::store().text(&self.agent, &text),
            Piece::Call(call) => super::store().trail_call(&self.agent, &call),
        }
    }

    fn finish(mut self, note: Option<&str>) {
        for piece in self.lines.finish() {
            self.apply(piece);
        }
        if let Some(note) = note {
            super::store().note(&self.agent, note);
        }
        super::store().close_turn(&self.agent, !self.failed && note.is_none());
    }
}

/// The agent a mind's one conversation is, today: `<harness>:main`.
pub fn main_agent(harness: &str) -> AgentId {
    AgentId::new(harness, AgentId::MAIN)
}

/// Who an agent is, from what the harness host knows of it: its mind's name, and whether that
/// mind holds more than one conversation.
pub fn meta_for(agent: &AgentId) -> AgentMeta {
    let host = crate::wire::harness::host();
    let live = host.and_then(|h| h.agents().into_iter().find(|a| &a.id == agent));
    let name = live
        .as_ref()
        .map(|a| a.harness_name.clone())
        .or_else(|| host.and_then(|h| h.list().into_iter().find(|e| e.id == agent.harness()).map(|e| e.name)))
        .unwrap_or_else(|| agent.harness().to_string());
    let mut meta = AgentMeta::new(agent.clone(), name);
    meta.conversations = live.is_some_and(|a| a.conversations);
    meta
}

/// Record a turn the Lens sent to an attached mind as that mind's `<harness>:main` agent, and
/// hand the answer on to the Lens unchanged.
///
/// The one hook in `wire/chat.rs`. The answer passes through a thread that copies each chunk into
/// the store before forwarding it, so the Lens sees exactly what it saw before. If the Lens stops
/// listening, this stops too and drops the harness's stream, which is what the harness saw before.
pub fn lens_turn(harness: &str, prompt: &str, answer: Answer) -> Answer {
    let agent = main_agent(harness);
    super::store().upsert_agent(meta_for(&agent));
    super::store().open_turn(&agent, prompt);
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new().name("agents-lens-turn".into()).spawn(move || {
        let mut reader = Reader::new(agent, false);
        while let Ok(chunk) = answer.recv() {
            reader.chunk(&chunk);
            if tx.send(chunk).is_err() {
                reader.finish(Some("The conversation panel stopped listening."));
                return;
            }
        }
        reader.finish(None);
    });
    if let Err(e) = spawned {
        // No thread, no copy: the Lens must still get its answer. It already has nothing to read
        // from, so say why instead.
        tracing::warn!(error = %e, "could not follow a Lens turn for the Agents screen");
    }
    rx
}

/// Record an answer that nothing else is reading — a turn from New agent or an agent's prompt box.
pub fn record(agent: AgentId, answer: Answer, builtin: bool) {
    let _ = std::thread::Builder::new().name("agents-turn".into()).spawn(move || {
        let mut reader = Reader::new(agent, builtin);
        while let Ok(chunk) = answer.recv() {
            let done = builtin && matches!(&chunk, Chunk::Text(t) if t == "__DONE__");
            reader.chunk(&chunk);
            if done {
                break;
            }
        }
        reader.finish(None);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(chunks: &[&str]) -> Vec<Piece> {
        let mut lines = TrailLines::default();
        let mut out: Vec<Piece> = chunks.iter().flat_map(|c| lines.push(c)).collect();
        out.extend(lines.finish());
        out
    }

    /// Prose as one string, calls by their one-line summary — the shape a reader cares about.
    fn shape(pieces: &[Piece]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for piece in pieces {
            match piece {
                Piece::Text(t) => match out.last_mut() {
                    Some(last) if !last.starts_with("CALL ") => last.push_str(t),
                    _ => out.push(t.clone()),
                },
                Piece::Call(c) => out.push(format!("CALL {}", c.summary())),
            }
        }
        out
    }

    #[test]
    fn a_trail_line_split_across_chunks_is_one_call_between_the_prose() {
        let pieces = feed(&[
            "Making it ",
            "now.\n⚙️ os_act studio.gen",
            "erate {\"args\":{\"prompt\":\"a red kite\"}}\n\nDone: one picture.",
        ]);
        assert_eq!(
            shape(&pieces),
            vec![
                "Making it now.\n".to_string(),
                "CALL os_act studio.generate prompt=\"a red kite\"".to_string(),
                "\nDone: one picture.".to_string(),
            ]
        );
    }

    #[test]
    fn prose_is_not_held_back_waiting_for_a_newline() {
        let mut lines = TrailLines::default();
        assert_eq!(lines.push("Hel"), vec![Piece::Text("Hel".into())], "said at once");
        assert_eq!(lines.push("lo"), vec![Piece::Text("lo".into())]);
        // A line that might be a call is held until it is whole.
        assert_eq!(lines.push("\n⚙️ os_apps"), vec![Piece::Text("\n".into())]);
        let rest = lines.finish();
        assert!(matches!(&rest[..], [Piece::Call(c)] if c.name == "os_apps"), "{rest:?}");
    }

    #[test]
    fn hermes_verbose_form_waits_one_line_for_its_arguments() {
        let pieces = feed(&[
            "⚙️ mcp_yantrik_os_os_act(['app', 'action', 'args'])\n",
            "{\"app\": \"studio\", \"action\": \"generate\", \"args\": {\"prompt\": \"a kite\"}}\n",
            "ok",
        ]);
        assert_eq!(
            shape(&pieces),
            vec!["CALL mcp_yantrik_os_os_act studio.generate prompt=\"a kite\"".to_string(), "ok".to_string()]
        );
    }

    #[test]
    fn a_gear_in_prose_or_in_a_code_fence_is_not_a_call() {
        let pieces = feed(&["The gear ⚙️ opens settings.\n```\n⚙️ os_apps\n```\n"]);
        assert!(pieces.iter().all(|p| matches!(p, Piece::Text(_))), "{pieces:?}");
    }

    #[test]
    fn hermes_default_line_is_a_name_and_nothing_more() {
        let pieces = feed(&["⚙️ mcp_yantrik_os_os_act...\n", "Done."]);
        assert_eq!(shape(&pieces), vec!["CALL mcp_yantrik_os_os_act".to_string(), "Done.".to_string()]);
    }
}
