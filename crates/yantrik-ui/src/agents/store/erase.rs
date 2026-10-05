//! Erasing words from one agent's session at the person's request (the harness `redact` event).
//!
//! The host decides whether an erasure may happen at all (yantrik-harness, `host::erase` and
//! `run_store::erase`) and erases the run store; this is the shell's own copy, the session the
//! Agents pane draws and `~/.local/share/yantrik/agents/<agent>.jsonl` keeps.
//!
//! # What is erased, and what is only shown erased
//!
//! The words the person and the agent said are replaced with the marker: each prompt, the
//! agent's text and its thinking (each joined across the turn's blocks before matching, so words
//! split across chunks or around a card are still found), the questions it asked, the shell's
//! notes, the title and the status line.
//!
//! The record of what happened is not touched: a tool call (its arguments, output and summary),
//! an approval (what was asked and how it came out), the refusal lines and every count and time
//! stay as they were, because an erasure must not be a way to make an action disappear. Where one
//! of those holds the words, it gets a mask — the same fields with the marker — and every view
//! draws the mask instead (`Card::shown`, `Approval::shown_what`, `Agent::shown_refusals`). The
//! mask keeps no words and no digest; it is keyed by the request the person answered.

use yantrik_harness::host::{ShellErased, ShellErasure};
use yantrik_harness::redact::{redact, redact_json, redact_pieces, Needle};

use super::*;

impl Store {
    /// Erase `e.needles` from agent `id`'s session in memory, mask the records that keep them,
    /// and record the erasure. Writing it to disk is the caller's (`Agents::redact`).
    pub fn redact(&mut self, id: &AgentId, e: &ShellErasure<'_>) -> ShellErased {
        let Some(i) = self.index(id) else { return ShellErased::default() };
        let now = self.now();
        let agent = &mut self.agents[i];
        let needles = e.needles;
        let mut done = ShellErased::default();

        done.places += in_place(&mut agent.meta.title, needles);
        done.places += in_place(&mut agent.status, needles);
        for turn in &mut agent.turns {
            done.places += in_place(&mut turn.prompt, needles);
            done.places += blocks(&mut turn.items, needles, false);
            done.places += blocks(&mut turn.items, needles, true);
            for item in &mut turn.items {
                match item {
                    Item::Question(q) => done.places += in_place(&mut q.prompt, needles),
                    Item::Note(note) => done.places += in_place(note, needles),
                    Item::Card(card) => done.masked += mask_card(card, e.request_id, needles),
                    Item::Approval(a) => done.masked += mask_approval(a, e.request_id, needles),
                    Item::Text(_) | Item::Thinking(_) => {}
                }
            }
        }
        let mut shown = agent.refusals_shown.clone().unwrap_or_else(|| agent.refusals.clone());
        let masked_lines: usize = shown.iter_mut().map(|line| in_place(line, needles)).sum();
        if masked_lines > 0 {
            agent.refusals_shown = Some(shown);
            done.masked += masked_lines;
        }

        let erasure = Erasure { request: e.request_id.to_string(), places: e.places_in_runs + done.places, at: now };
        if let Some(turn) = agent.turns.last_mut() {
            turn.items.push(Item::Note(erasure.line()));
        }
        agent.erasures.push(erasure);
        self.mark(i);
        done
    }
}

/// Erase the needles from one string in place. How many places.
fn in_place(text: &mut String, needles: &[Needle]) -> usize {
    match redact(text, needles) {
        Some((erased, n)) => {
            *text = erased;
            n
        }
        None => 0,
    }
}

/// Erase the needles from a turn's text blocks (or its thinking), joined as one text. How many
/// places.
fn blocks(items: &mut [Item], needles: &[Needle], thinking: bool) -> usize {
    let mut kept: Vec<(usize, String)> = Vec::new();
    for (at, item) in items.iter().enumerate() {
        match item {
            Item::Text(buffer) if !thinking => kept.push((at, buffer.text())),
            Item::Thinking(buffer) if thinking => kept.push((at, buffer.text())),
            _ => {}
        }
    }
    let pieces: Vec<&str> = kept.iter().map(|(_, t)| t.as_str()).collect();
    let Some((erased, n)) = redact_pieces(&pieces, needles) else { return 0 };
    for ((at, before), after) in kept.iter().zip(erased) {
        if *before == after {
            continue;
        }
        let mut buffer = Capped::new(TEXT_HEAD, TEXT_CAP);
        buffer.push(after.as_bytes());
        items[*at] = if thinking { Item::Thinking(buffer) } else { Item::Text(buffer) };
    }
    n
}

/// Mask a card whose fields hold the needles; the card keeps its own. How many places masked.
fn mask_card(card: &mut Card, request: &str, needles: &[Needle]) -> usize {
    let mut mask = match &card.mask {
        Some(mask) => (**mask).clone(),
        None => CardMask {
            request: String::new(),
            target: card.target.clone(),
            args: card.args.clone(),
            preview: card.preview.clone(),
            summary: card.summary.clone(),
            output: card.output.bytes.text(),
        },
    };
    let n = in_place(&mut mask.target, needles)
        + redact_json(&mut mask.args, needles)
        + in_place(&mut mask.preview, needles)
        + in_place(&mut mask.summary, needles)
        + in_place(&mut mask.output, needles);
    if n > 0 {
        mask.request = request.to_string();
        card.mask = Some(Box::new(mask));
    }
    n
}

/// Mask an approval whose words hold the needles; the approval keeps its own.
fn mask_approval(approval: &mut Approval, request: &str, needles: &[Needle]) -> usize {
    let mut mask = approval.mask.clone().unwrap_or_else(|| ApprovalMask {
        request: String::new(),
        what: approval.what.clone(),
        record: approval.record.clone(),
    });
    let n = in_place(&mut mask.what, needles) + in_place(&mut mask.record, needles);
    if n > 0 {
        mask.request = request.to_string();
        approval.mask = Some(mask);
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use yantrik_harness::redact::MARKER;

    const SECRET: &str = "Priya";

    fn erasure(needles: &[Needle]) -> ShellErasure<'_> {
        ShellErasure { request_id: "forget-1", needles, places_in_runs: 4 }
    }

    /// An agent whose session holds the secret everywhere it can: the prompt, the reply split
    /// across two chunks, its thinking, a tool call's arguments and output, an approval, and a
    /// question.
    fn session() -> (Store, AgentId) {
        let mut s = Store::with_clock(Box::new(|| 1_000));
        let pi = AgentId::new("pi", "c-1");
        s.open_turn(&pi, "my sister is Priya, remember that");
        s.text(&pi, "Got it: Pri");
        s.text(&pi, "ya is your sister.");
        s.event(&pi, &Event::Thinking { delta: "store Priya".into() }, Provenance::Reported);
        s.event(
            &pi,
            &Event::ToolStart { call: "t1".into(), name: "os_act".into(), target: "notes".into(), args: json!({"text": "sister: Priya"}) },
            Provenance::Reported,
        );
        s.event(&pi, &Event::ToolOutput { call: "t1".into(), stream: Stream::Stdout, delta: "saved Priya\n".into() }, Provenance::Reported);
        s.event(&pi, &Event::ToolEnd { call: "t1".into(), ok: true, summary: "noted Priya".into(), exit_code: None }, Provenance::Reported);
        s.approval_asked(&pi, "appr-1", "notes.write Priya");
        s.approval_answered(&pi, "appr-1", true);
        s.event(
            &pi,
            &Event::Request { request_id: "forget-1".into(), prompt: "Forget Priya?".into(), options: vec!["Keep".into(), "Erase".into()] },
            Provenance::Reported,
        );
        s.question_answered(&pi, "forget-1", "Erase");
        s.close_turn(&pi, true);
        (s, pi)
    }

    fn card(s: &Store, id: &AgentId) -> &Card {
        s.agent(id).unwrap().cards().next().unwrap()
    }

    fn approval(s: &Store, id: &AgentId) -> &Approval {
        s.agent(id).unwrap().turns[0].items.iter().find_map(|i| if let Item::Approval(a) = i { Some(a) } else { None }).unwrap()
    }

    #[test]
    fn the_conversation_is_erased_with_the_reply_joined_across_its_chunks() {
        let (mut s, pi) = session();
        let needles = [Needle::of(SECRET)];
        let done = s.redact(&pi, &erasure(&needles));
        let a = s.agent(&pi).unwrap();
        let turn = &a.turns[0];
        assert_eq!(turn.prompt, format!("my sister is {MARKER}, remember that"));
        assert_eq!(a.meta.title, format!("my sister is {MARKER}, remember that"));
        // "Pri" and "ya" came as two chunks; the pane holds them as one block, matched whole.
        let text: String = turn.items.iter().filter_map(|i| if let Item::Text(t) = i { Some(t.text()) } else { None }).collect();
        assert_eq!(text, format!("Got it: {MARKER} is your sister."));
        let thinking: String = turn.items.iter().filter_map(|i| if let Item::Thinking(t) = i { Some(t.text()) } else { None }).collect();
        assert_eq!(thinking, format!("store {MARKER}"));
        let question = turn.items.iter().find_map(|i| if let Item::Question(q) = i { Some(q) } else { None }).unwrap();
        assert_eq!((question.prompt.as_str(), question.answer.as_str()), (format!("Forget {MARKER}?").as_str(), "Erase"));
        // prompt, title, reply, thinking, question.
        assert_eq!(done.places, 5);
        assert_eq!(a.erasures, vec![Erasure { request: "forget-1".into(), places: 9, at: 1_000 }]);
        assert!(matches!(turn.items.last(), Some(Item::Note(n)) if n == "Erased 9 places at your request."));

        let transcript = s.transcript(&pi, 5).unwrap();
        assert!(!transcript.contains(SECRET), "nothing the pane or read_agent shows holds the words:\n{transcript}");
        assert!(transcript.contains("Erased 9 places at your request."));
    }

    #[test]
    fn a_tool_call_and_an_approval_keep_their_words_and_are_shown_masked() {
        let (mut s, pi) = session();
        let before_args = card(&s, &pi).args.clone();
        let before_output = card(&s, &pi).output.all();
        let before_what = approval(&s, &pi).what.clone();
        let needles = [Needle::of(SECRET)];
        let done = s.redact(&pi, &erasure(&needles));

        // The records of what happened are untouched.
        let c = card(&s, &pi);
        assert_eq!((c.args.clone(), c.output.all(), c.summary.as_str()), (before_args, before_output, "noted Priya"));
        let ap = approval(&s, &pi);
        assert_eq!((ap.what.as_str(), ap.outcome), (before_what.as_str(), ApprovalOutcome::Allowed));
        // What is drawn of them is masked, keyed by the erasure.
        let shown = c.shown();
        assert_eq!(shown.args, json!({"text": format!("sister: {MARKER}")}));
        assert_eq!(shown.output.all(), format!("saved {MARKER}\n"));
        assert_eq!(shown.summary, format!("noted {MARKER}"));
        assert_eq!((shown.state, shown.call.as_str()), (CallState::Ok, "t1"), "how it went is not touched");
        assert_eq!(c.mask.as_ref().unwrap().request, "forget-1");
        assert_eq!(ap.shown_what(), format!("notes.write {MARKER}"));
        // args, output, summary; the approval's words.
        assert_eq!(done.masked, 4);
    }

    #[test]
    fn the_saved_session_holds_the_words_only_inside_the_records_that_keep_them() {
        let (mut s, pi) = session();
        let needles = [Needle::of(SECRET)];
        s.redact(&pi, &erasure(&needles));
        let dir = std::env::temp_dir().join(format!("yantrik-erase-ui-{}-{}", std::process::id(), model::now()));
        let (path, contents) = s.file_of(&dir, &pi).unwrap();
        write_durably(&dir, &path, &contents).unwrap();
        assert!(!path.with_extension("jsonl.partial").exists());
        let saved = std::fs::read_to_string(&path).unwrap();
        for line in saved.lines() {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            if value["kind"] == "turn" {
                assert!(!value["prompt"].to_string().contains(SECRET));
                for item in value["items"].as_array().unwrap() {
                    if item.get("card").is_none() && item.get("approval").is_none() {
                        assert!(!item.to_string().contains(SECRET), "{item} holds the words");
                    }
                }
            }
        }
        // Read back, it is still shown masked.
        let back = Store::load(&dir, Box::new(|| 2_000));
        assert_eq!(card(&back, &pi).shown().args, json!({"text": format!("sister: {MARKER}")}));
        assert_eq!(approval(&back, &pi).shown_what(), format!("notes.write {MARKER}"));
        assert_eq!(back.agent(&pi).unwrap().erasures.len(), 1);
        assert!(!back.transcript(&pi, 5).unwrap().contains(SECRET));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn composed_and_decomposed_words_are_erased_alike() {
        let mut s = Store::with_clock(Box::new(|| 1));
        let pi = AgentId::new("pi", "c-2");
        s.open_turn(&pi, "meet me at the Cafe\u{301} Lune");
        s.text(&pi, "The caf\u{e9} it is.");
        let needles = [Needle::of("Caf\u{e9} Lune"), Needle::of("cafe\u{301}")];
        let done = s.redact(&pi, &erasure(&needles));
        assert_eq!(done.places, 3, "the prompt, the title and the reply");
        let a = s.agent(&pi).unwrap();
        assert_eq!(a.turns[0].prompt, format!("meet me at the {MARKER}"));
        assert!(matches!(&a.turns[0].items[0], Item::Text(t) if t.text() == format!("The {MARKER} it is.")));
    }

    #[test]
    fn an_agent_the_store_does_not_hold_is_nothing_to_erase() {
        let mut s = Store::with_clock(Box::new(|| 1));
        let needles = [Needle::of(SECRET)];
        assert_eq!(s.redact(&AgentId::new("pi", "c-9"), &erasure(&needles)), ShellErased::default());
    }

    #[test]
    fn a_redact_from_a_feeder_is_refused_and_counted() {
        let (mut s, pi) = session();
        s.open_turn(&pi, "again");
        s.event(&pi, &Event::Redact { request_id: "forget-1".into(), needles: vec![Needle::of(SECRET)] }, Provenance::Reported);
        let a = s.agent(&pi).unwrap();
        assert!(a.refusals.last().unwrap().contains("only the host applies one"));
        assert!(a.erasures.is_empty());
    }
}
