//! Interjection Classifier — rule-based classification of user input
//! during active recipe execution.
//!
//! Pure pattern matching + keyword detection. No LLM call needed.
//! Must be fast enough to run on every incoming message when a recipe is active.
//!
//! The chat's own word to a recipe — "pause the digest", the answer to a recipe's question — is
//! [`from_chat`], read against the recipes as the desk shows them; the shell asks it for every
//! line typed to the desktop before any mind is asked, and acts on it through
//! [`crate::recipe_view::apply`], the Recipes screen's path.

use crate::recipe::{waited_on, RecipeStatus, RecipeStep, RecipeStore, Trail, WAIT_VAR};
use crate::recipe_view::{is_in_flight, RecipeOp, RecipeView};
use rusqlite::Connection;

/// Classification of a user message during active recipe execution.
#[derive(Debug, Clone, PartialEq)]
pub enum Interjection {
    /// User wants to stop the recipe entirely.
    /// Keywords: "cancel", "stop", "abort", "nevermind", "forget it"
    Cancel,
    /// User wants to pause the recipe and resume later.
    /// Keywords: "pause", "hold on", "wait", "not now", "later"
    Pause,
    /// User is answering an AskUser prompt from the recipe.
    /// Detected when a recipe is in Waiting state with an AskUser step.
    AnswerAskUser {
        recipe_id: String,
        answer: String,
    },
    /// User wants to change parameters and restart the recipe.
    /// Keywords: "change", "actually", "instead", "modify", "redo"
    ModifyAndRestart {
        modification: String,
    },
    /// User's message is unrelated to the active recipe.
    /// Handle normally without disrupting the recipe.
    OutOfBandChat,
}

/// Classify user input in the context of active recipes.
///
/// Returns `None` if no recipe is active (normal message flow).
/// Returns `Some(Interjection)` if a recipe is running/waiting.
pub fn classify(conn: &Connection, user_text: &str) -> Option<Interjection> {
    // Check if any recipe is active
    let active = RecipeStore::list(conn, Some("running"), 1);
    let waiting = RecipeStore::list(conn, Some("waiting"), 1);

    if active.is_empty() && waiting.is_empty() {
        return None; // No active recipe — normal message flow
    }

    let text_lower = user_text.trim().to_lowercase();

    // 1. Check for cancel intent (highest priority)
    if is_cancel(&text_lower) {
        return Some(Interjection::Cancel);
    }

    // 2. Check for pause intent
    if is_pause(&text_lower) {
        return Some(Interjection::Pause);
    }

    // 3. Check for AskUser answer (recipe is waiting for user input)
    if let Some(recipe) = waiting.first() {
        let asking = waited_on(recipe, &RecipeStore::get_steps(conn, &recipe.id), &RecipeStore::get_vars(conn, &recipe.id))
            .is_some_and(|w| w.store_as().is_some());
        // This looks like an answer to the AskUser prompt, unless it's clearly a modify command.
        if asking && !is_modify(&text_lower) {
            return Some(Interjection::AnswerAskUser { recipe_id: recipe.id.clone(), answer: user_text.to_string() });
        }
    }

    // 4. Check for modify/restart intent
    if is_modify(&text_lower) {
        return Some(Interjection::ModifyAndRestart {
            modification: user_text.to_string(),
        });
    }

    // 5. Default: out-of-band chat
    // The message doesn't seem related to recipe control — handle normally
    Some(Interjection::OutOfBandChat)
}

/// Handle the classified interjection. Returns an optional response message.
pub fn handle(conn: &Connection, interjection: &Interjection) -> Option<String> {
    match interjection {
        Interjection::Cancel => {
            // Cancel all running/waiting recipes
            let running = RecipeStore::list(conn, Some("running"), 10);
            let waiting = RecipeStore::list(conn, Some("waiting"), 10);
            let mut cancelled = 0;
            for recipe in running.iter().chain(waiting.iter()) {
                RecipeStore::set_error(conn, &recipe.id, crate::recipe::CANCELLED);
                cancelled += 1;
            }
            if cancelled > 0 {
                Some(format!("Cancelled {} active recipe(s).", cancelled))
            } else {
                Some("No active recipes to cancel.".to_string())
            }
        }
        Interjection::Pause => {
            let running = RecipeStore::list(conn, Some("running"), 10);
            let mut paused = 0;
            // Paused, not Waiting: a `waiting` recipe whose last step was not a WaitFor is
            // resumed by `get_expired_waiting` at the next message, so this pause never held.
            for recipe in &running {
                if RecipeStore::pause(conn, &recipe.id).is_ok() {
                    paused += 1;
                }
            }
            if paused > 0 {
                Some(format!(
                    "Paused {} recipe(s). Say 'resume' or 'continue' to restart.",
                    paused
                ))
            } else {
                Some("No running recipes to pause.".to_string())
            }
        }
        Interjection::AnswerAskUser {
            recipe_id,
            answer,
        } => {
            // Store the answer in recipe vars and resume. The question is where the executor's
            // `_wait` says — at the top, or inside a Branch — or, for a recipe that began to wait
            // before there was one, the step just behind the pointer.
            let recipe = RecipeStore::get(conn, recipe_id)?;
            let steps = RecipeStore::get_steps(conn, recipe_id);
            let waited = waited_on(&recipe, &steps, &RecipeStore::get_vars(conn, recipe_id))?;
            let Some(RecipeStep::AskUser { store_as, .. }) = &waited.on else { return None };
            RecipeStore::set_var(conn, recipe_id, store_as, &serde_json::Value::String(answer.clone()));
            if waited.inner.is_empty() {
                // Its own step's record says what was answered.
                RecipeStore::complete_step(conn, recipe_id, waited.step, answer);
            } else {
                Trail::note(conn, recipe_id, waited.step, "waiting", "answered");
            }
            RecipeStore::delete_var(conn, recipe_id, WAIT_VAR);
            RecipeStore::update_status(conn, recipe_id, &RecipeStatus::Running, recipe.current_step);
            Some(format!("Got it. Resuming recipe '{}'...", recipe.name))
        }
        Interjection::ModifyAndRestart { modification } => {
            Some(format!(
                "To modify and restart a recipe, use: run_recipe with updated variables. \
                 Your modification: {}",
                modification
            ))
        }
        Interjection::OutOfBandChat => {
            // Don't interfere — let normal message handling proceed
            None
        }
    }
}

/// Answer the question a waiting recipe asked — from the Recipes screen, `answer_recipe` on the
/// shell's control surface, or the chat ([`from_chat`]), all by way of `recipe_view::apply`.
///
/// [`handle`] with [`Interjection::AnswerAskUser`] does it, which
/// stores the answer under the step's `store_as`, marks the step done with it and sets the recipe
/// running. What this adds is the refusal when there is no question to answer, so a caller learns
/// that rather than having its text dropped. The caller signals the executor afterwards.
pub fn answer(conn: &Connection, recipe_id: &str, text: &str) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("the answer is empty".to_string());
    }
    let recipe = RecipeStore::get(conn, recipe_id).ok_or_else(|| format!("no recipe `{recipe_id}`"))?;
    let asking = recipe.status == RecipeStatus::Waiting
        && waited_on(&recipe, &RecipeStore::get_steps(conn, recipe_id), &RecipeStore::get_vars(conn, recipe_id))
            .is_some_and(|w| w.store_as().is_some());
    if !asking {
        return Err(format!(
            "`{}` is not waiting for an answer (it is {})",
            recipe.name,
            recipe.status.as_str()
        ));
    }
    handle(
        conn,
        &Interjection::AnswerAskUser { recipe_id: recipe_id.to_string(), answer: text.to_string() },
    )
    .ok_or_else(|| format!("`{}` could not take the answer", recipe.name))
}

// ── The chat: a line typed to the desktop, read against the recipes as the desk shows them ──

/// How long after a recipe asks a question with no choices a line typed to the desktop is taken
/// as its answer. Later than that, the Recipes screen answers it.
pub const ANSWER_WINDOW_SECS: f64 = 15.0 * 60.0;

/// What a line typed to the desktop says to a recipe, when it says anything to one.
///
/// Read from the recipes as published ([`RecipeView`]), not from the store, so the shell can ask
/// on its UI thread — before the line goes to any mind — without waiting on the companion worker.
/// What it says is done through [`crate::recipe_view::apply`], the Recipes screen's own path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatWord {
    /// Do this to that recipe.
    To { recipe_id: String, name: String, op: RecipeOp },
    /// Said to a recipe, but more than one could be meant.
    Which { verb: &'static str, names: Vec<String> },
}

impl ChatWord {
    /// The question back, for [`ChatWord::Which`].
    pub fn which_text(&self) -> Option<String> {
        let ChatWord::Which { verb, names } = self else { return None };
        let quoted: Vec<String> = names.iter().map(|n| format!("'{n}'")).collect();
        Some(if *verb == "answer" {
            format!(
                "More than one recipe is asking — {}. Answer the one you mean on the Recipes screen.",
                quoted.join(" and ")
            )
        } else {
            format!("Which one — {}? Say “{verb} the …” with its name.", quoted.join(" or "))
        })
    }
}

/// The words that pause, resume or cancel a recipe. `alone`: whether the verb by itself is
/// enough, with no recipe named — "stop", "continue" and "hold" alone are said to minds too often
/// to be taken as a recipe's.
const CONTROL: &[(&str, bool)] = &[
    ("pause", true),
    ("hold", false),
    ("resume", true),
    ("unpause", true),
    ("continue", false),
    ("cancel", true),
    ("stop", false),
    ("abort", false),
];

/// What stands for "the recipe" after a verb.
const THE_RECIPE: &[&str] = &["it", "that", "this", "the recipe", "this recipe", "that recipe", "recipe", "the run", "this run"];

/// Words that do not name a recipe.
const FILLER: &[&str] = &["the", "my", "a", "an", "recipe", "run"];

/// What a line says to a recipe, if anything.
///
/// - "pause the digest", "resume digest", "cancel the morning briefing" — the in-flight recipe
///   whose name has those words. A name that matches nothing in flight is not a recipe's word:
///   "stop the download" goes to the mind.
/// - "pause", "cancel it" — the one recipe that can take it; with several, which one.
/// - An answer: one of the choices a waiting question offers (its words, or its number), or —
///   when exactly one recipe asks a question with no choices, asked within
///   [`ANSWER_WINDOW_SECS`] — the line itself, unless it is a question.
pub fn from_chat(views: &[RecipeView], text: &str, now: f64) -> Option<ChatWord> {
    let said = text.trim();
    let lowered = said.trim_end_matches(['.', '!']).trim().to_lowercase();
    let line = lowered.strip_prefix("please ").unwrap_or(&lowered).trim();
    if line.is_empty() {
        return None;
    }
    let live: Vec<&RecipeView> = views.iter().filter(|v| !v.template && is_in_flight(v)).collect();
    if live.is_empty() {
        return None;
    }

    let (first, rest) = line.split_once(' ').unwrap_or((line, ""));
    if let Some(&(verb, alone)) = CONTROL.iter().find(|(v, _)| *v == first) {
        let op = match verb {
            "pause" | "hold" => RecipeOp::Pause,
            "resume" | "unpause" | "continue" => RecipeOp::Resume,
            _ => RecipeOp::Cancel,
        };
        let object = rest.trim();
        let meant: Vec<&RecipeView> = if object.is_empty() || THE_RECIPE.contains(&object) {
            if object.is_empty() && !alone {
                return None;
            }
            live.iter().copied().filter(|v| allows(v, &op)).collect()
        } else {
            named(&live, object)
        };
        return pick(verb, op, meant);
    }

    let asking: Vec<&RecipeView> = live.iter().copied().filter(|v| v.can.answer).collect();
    let by_choice: Vec<(&RecipeView, String)> = asking
        .iter()
        .filter_map(|v| {
            let q = v.question.as_ref()?;
            q.choices
                .iter()
                .enumerate()
                .find(|(i, c)| c.trim().to_lowercase() == line || line == (i + 1).to_string())
                .map(|(_, c)| (*v, c.clone()))
        })
        .collect();
    match by_choice.as_slice() {
        [(v, choice)] => return Some(to(v, RecipeOp::Answer(choice.clone()))),
        [_, _, ..] => return Some(ChatWord::Which { verb: "answer", names: by_choice.iter().map(|(v, _)| v.name.clone()).collect() }),
        [] => {}
    }
    if let [only] = asking.as_slice() {
        let open = only.question.as_ref().is_some_and(|q| q.choices.is_empty());
        if open && !said.ends_with('?') && now - only.updated_at <= ANSWER_WINDOW_SECS {
            return Some(to(only, RecipeOp::Answer(said.to_string())));
        }
    }
    None
}

fn allows(v: &RecipeView, op: &RecipeOp) -> bool {
    match op {
        RecipeOp::Pause => v.can.pause,
        RecipeOp::Resume => v.can.resume,
        RecipeOp::Cancel => v.can.cancel,
        RecipeOp::Answer(_) => v.can.answer,
    }
}

/// The in-flight recipes a name means: its id, or every word of the name in the recipe's name.
fn named<'a>(live: &[&'a RecipeView], object: &str) -> Vec<&'a RecipeView> {
    let words = |s: &str| -> Vec<String> {
        s.split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_lowercase)
            .filter(|w| !FILLER.contains(&w.as_str()))
            .collect()
    };
    let wanted = words(object);
    live.iter()
        .copied()
        .filter(|v| {
            if v.id.eq_ignore_ascii_case(object) {
                return true;
            }
            let has = words(&v.name);
            !wanted.is_empty() && wanted.iter().all(|w| has.contains(w))
        })
        .collect()
}

fn pick(verb: &'static str, op: RecipeOp, meant: Vec<&RecipeView>) -> Option<ChatWord> {
    match meant.as_slice() {
        [] => None,
        [one] => Some(to(one, op)),
        many => Some(ChatWord::Which { verb, names: many.iter().map(|v| v.name.clone()).collect() }),
    }
}

fn to(v: &RecipeView, op: RecipeOp) -> ChatWord {
    ChatWord::To { recipe_id: v.id.clone(), name: v.name.clone(), op }
}

// ── Pattern Matching ──

fn is_cancel(text: &str) -> bool {
    let exact = [
        "cancel",
        "stop",
        "abort",
        "nevermind",
        "never mind",
        "forget it",
        "quit",
        "exit",
        "stop it",
        "cancel that",
        "stop that",
        "cancel recipe",
        "stop recipe",
    ];
    if exact.iter().any(|p| text == *p) {
        return true;
    }

    // Verb + object, but only when the object is the running recipe itself.
    //
    // This was a bare `starts_with("cancel ")`, so every sentence opening with
    // the verb aborted the recipe: "cancel my appointment" and "stop the
    // download" are *new tasks* the user is asking for, and while a recipe
    // happened to be running they cancelled it instead and the request was
    // never heard. `classify` calls this first, ahead of every other branch,
    // so there was no second chance. The verb alone does not say what is being
    // cancelled; the object does.
    const SELF_OBJECTS: &[&str] = &[
        "that", "this", "it", "them", "all", "everything", "the whole thing",
        "the recipe", "this recipe", "the task", "this task",
        "the step", "this step", "the run", "this run",
        "what you're doing", "what you are doing",
    ];
    for verb in ["cancel", "stop", "abort"] {
        if let Some(rest) = text.strip_prefix(verb).and_then(|r| r.strip_prefix(' ')) {
            let object = rest.trim().trim_end_matches(['.', '!', '?']).trim();
            if SELF_OBJECTS.contains(&object) {
                return true;
            }
        }
    }

    false
}

fn is_pause(text: &str) -> bool {
    let exact = [
        "pause",
        "hold on",
        "hold",
        "wait",
        "not now",
        "later",
        "pause recipe",
        "hold that",
        "one sec",
        "one moment",
        "hang on",
    ];
    if exact.iter().any(|p| text == *p) {
        return true;
    }

    let prefixes = ["pause ", "hold on ", "wait "];
    if prefixes.iter().any(|p| text.starts_with(p)) {
        return true;
    }

    false
}

fn is_modify(text: &str) -> bool {
    let prefixes = [
        "actually ",
        "instead ",
        "change ",
        "modify ",
        "redo ",
        "restart with ",
        "try again with ",
        "change it to ",
        "use a different ",
    ];
    prefixes.iter().any(|p| text.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cancel_detection() {
        assert!(is_cancel("cancel"));
        assert!(is_cancel("stop"));
        assert!(is_cancel("abort"));
        assert!(is_cancel("nevermind"));
        assert!(is_cancel("cancel that"));
        assert!(is_cancel("cancel recipe"));
        assert!(is_cancel("stop the recipe"));
        assert!(is_cancel("abort everything"));
        // A verb the user aimed at something else. These are requests, not
        // recipe control, and must fall through to normal handling.
        assert!(!is_cancel("cancel my appointment"));
        assert!(!is_cancel("stop the download"));
        assert!(!is_cancel("how do i cancel")); // Not a cancel command
    }

    #[test]
    fn test_pause_detection() {
        assert!(is_pause("pause"));
        assert!(is_pause("hold on"));
        assert!(is_pause("not now"));
        assert!(is_pause("hang on"));
        assert!(!is_pause("how long will it pause"));
    }

    #[test]
    fn test_modify_detection() {
        assert!(is_modify("actually use python instead"));
        assert!(is_modify("change it to morning"));
        assert!(is_modify("redo with fewer steps"));
        assert!(!is_modify("that's actually great"));
    }

    // ── The chat's word to a recipe ──

    use crate::recipe_view::{Controls, QuestionView};

    fn desk(id: &str, name: &str, status: &str, question: Option<(&str, &[&str])>, updated_at: f64) -> RecipeView {
        RecipeView {
            id: id.into(),
            name: name.into(),
            description: String::new(),
            status: status.into(),
            current_step: 1,
            created_at: 0.0,
            updated_at,
            error: None,
            template: false,
            waiting_for: None,
            question: question.map(|(text, choices)| QuestionView {
                step: 0,
                text: text.into(),
                choices: choices.iter().map(|c| c.to_string()).collect(),
                store_as: "answer".into(),
            }),
            steps: vec![],
            can: Controls {
                answer: status == "waiting" && question.is_some(),
                pause: matches!(status, "running" | "waiting"),
                resume: status == "paused",
                cancel: matches!(status, "running" | "waiting" | "paused"),
            },
        }
    }

    fn to(id: &str, name: &str, op: RecipeOp) -> Option<ChatWord> {
        Some(ChatWord::To { recipe_id: id.into(), name: name.into(), op })
    }

    const NOW: f64 = 10_000.0;

    /// "pause the digest" in the Lens reaches the digest; a name that matches nothing in flight is
    /// not a recipe's word, and goes to the mind (#176).
    #[test]
    fn the_chat_pauses_resumes_and_cancels_a_recipe_by_name() {
        let views = [
            desk("rcp_d", "Morning email digest", "running", None, NOW),
            desk("rcp_t", "Tidy downloads", "paused", None, NOW),
            desk("builtin_x", "Digest template", "pending", None, NOW),
        ];
        assert_eq!(from_chat(&views, "pause the digest", NOW), to("rcp_d", "Morning email digest", RecipeOp::Pause));
        assert_eq!(from_chat(&views, "Please cancel the morning email digest recipe.", NOW), to("rcp_d", "Morning email digest", RecipeOp::Cancel));
        assert_eq!(from_chat(&views, "resume tidy downloads", NOW), to("rcp_t", "Tidy downloads", RecipeOp::Resume));
        assert_eq!(from_chat(&views, "continue the downloads", NOW), to("rcp_t", "Tidy downloads", RecipeOp::Resume));
        // Named, even when it cannot take it: `apply` says why, rather than the mind guessing.
        assert_eq!(from_chat(&views, "resume the digest", NOW), to("rcp_d", "Morning email digest", RecipeOp::Resume));
        // Not a recipe's word.
        assert_eq!(from_chat(&views, "stop the download", NOW), None, "no recipe in flight is named that");
        assert_eq!(from_chat(&views, "pause the music", NOW), None);
        assert_eq!(from_chat(&views, "stop", NOW), None, "a bare stop is said to minds too");
        assert_eq!(from_chat(&views, "hold on", NOW), None);
        assert_eq!(from_chat(&views, "how do I pause a recipe?", NOW), None);
        // With nothing in flight, nothing is a recipe's word.
        assert_eq!(from_chat(&views[2..], "pause", NOW), None);
    }

    /// "pause" or "cancel it" means the one recipe that can take it; with several, which one.
    #[test]
    fn a_bare_word_means_the_one_recipe_that_can_take_it() {
        let one = [desk("rcp_d", "Digest", "running", None, NOW), desk("rcp_t", "Tidy", "paused", None, NOW)];
        assert_eq!(from_chat(&one, "pause", NOW), to("rcp_d", "Digest", RecipeOp::Pause));
        assert_eq!(from_chat(&one, "resume", NOW), to("rcp_t", "Tidy", RecipeOp::Resume));
        assert_eq!(from_chat(&one, "cancel it", NOW), Some(ChatWord::Which { verb: "cancel", names: vec!["Digest".into(), "Tidy".into()] }));
        let which = from_chat(&one, "cancel it", NOW).and_then(|w| w.which_text()).expect("asks which");
        assert!(which.contains("'Digest' or 'Tidy'"), "{which}");
    }

    /// A question answered in the chat: by one of its choices — its words or its number — or, for
    /// a question with no choices asked a moment ago, by the line itself.
    #[test]
    fn the_chat_answers_the_question_a_recipe_waits_on() {
        let views = [desk("rcp_a", "Tidy downloads", "waiting", Some(("Move them where?", &["Archive", "Trash"])), NOW)];
        assert_eq!(from_chat(&views, "archive", NOW), to("rcp_a", "Tidy downloads", RecipeOp::Answer("Archive".into())));
        assert_eq!(from_chat(&views, "2", NOW), to("rcp_a", "Tidy downloads", RecipeOp::Answer("Trash".into())));
        assert_eq!(from_chat(&views, "what's the weather", NOW), None, "not a choice: the mind's");

        let open = [desk("rcp_b", "Draft reply", "waiting", Some(("What should it say?", &[])), NOW)];
        assert_eq!(from_chat(&open, "Tell them Friday works", NOW + 60.0), to("rcp_b", "Draft reply", RecipeOp::Answer("Tell them Friday works".into())));
        assert_eq!(from_chat(&open, "what time is it?", NOW + 60.0), None, "a question is not an answer");
        assert_eq!(from_chat(&open, "Tell them Friday works", NOW + ANSWER_WINDOW_SECS + 1.0), None, "asked long ago: the screen answers it");

        // Two asking with the same choice: which.
        let two = [
            desk("rcp_a", "Tidy downloads", "waiting", Some(("Go on?", &["yes", "no"])), NOW),
            desk("rcp_c", "Clean desktop", "waiting", Some(("Go on?", &["yes", "no"])), NOW),
        ];
        assert!(matches!(from_chat(&two, "yes", NOW), Some(ChatWord::Which { verb: "answer", .. })));
        // Paused while asking: not answerable until resumed.
        let paused = [desk("rcp_a", "Tidy downloads", "paused", Some(("Move them where?", &["Archive"])), NOW)];
        assert_eq!(from_chat(&paused, "archive", NOW), None);
    }
}
