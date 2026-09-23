//! What the desk shows of a recipe: every recipe with its status, and each of its steps as one
//! stage — what kind of step it is, one line saying what it does, where it stands, and the head of
//! what it produced.
//!
//! Pure mapping from the store's own types ([`Recipe`], [`StoredStep`] and the recipe's variables)
//! to a view model the shell draws: the Recipes screen, `describe shell` → `recipes`, and the mind
//! panel's one line per recipe in flight ([`one_line`]). Nothing here runs a step. The only writes
//! are [`apply`]'s — a person's answer, pause, resume or cancel — and each goes through the store's
//! own door for it.
//!
//! Open on purpose, for section 6 of design/desk-and-mind-2026-09-23.md: a step's `kind` is a
//! string, not an enum a reader must match in full, and a step can name the catalog agent that
//! runs it (`agent`). The `Agent { role, prompt, store_as }` step arrives as one more kind, drawn
//! by the same screen.

use std::collections::{HashMap, HashSet};

use rusqlite::Connection;
use serde::Serialize;

use crate::recipe::{
    AggregateOp, Condition, ErrorAction, FilterOp, Recipe, RecipeStatus, RecipeStep, RecipeStore,
    RenderFormat, StoredStep, WaitCondition, CANCELLED, PAUSED_FROM_VAR,
};

/// How many recipes the desk reads. The built-in definitions alone are about fifty.
pub const LIST_LIMIT: usize = 200;

/// How much of a step's result the view keeps.
pub const RESULT_HEAD: usize = 280;

/// How long a step's one line may be.
const SUMMARY_MAX: usize = 120;

/// How long one line of a step's opened definition may be.
const DETAIL_MAX: usize = 600;

/// What `get_steps` stands in for a step whose JSON it could not read.
const UNREADABLE: &str = "PARSE ERROR: ";

type Vars = HashMap<String, serde_json::Value>;

/// One recipe, as the desk shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RecipeView {
    pub id: String,
    pub name: String,
    pub description: String,
    /// pending | running | waiting | paused | done | failed | cancelled.
    ///
    /// `cancelled` is how the store keeps a cancel — failed, with [`CANCELLED`] — told apart,
    /// because a person's decision is not a fault and is not drawn as one.
    pub status: String,
    /// The store's step pointer, 0-based: the step running or next to run. A recipe waiting has
    /// already moved past the step it waits on, which is `current_step - 1`.
    pub current_step: usize,
    pub created_at: f64,
    pub updated_at: f64,
    pub error: Option<String>,
    /// One of the built-in definitions (`builtin_*`) that has never run.
    pub template: bool,
    /// What it waits on, in a few words, while it waits — or is paused while waiting.
    pub waiting_for: Option<String>,
    /// The question, when what it waits on is a person's answer.
    pub question: Option<QuestionView>,
    pub steps: Vec<StepView>,
    /// What a person may do to it now.
    pub can: Controls,
}

/// The question an `AskUser` step put, waiting for its answer.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QuestionView {
    /// The step that asked, 0-based.
    pub step: usize,
    pub text: String,
    /// Offered as buttons. Any text is still accepted as the answer.
    pub choices: Vec<String>,
    /// The variable the answer is kept in.
    pub store_as: String,
}

/// What a person may do to a recipe now. [`apply`] refuses the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct Controls {
    pub answer: bool,
    pub pause: bool,
    pub resume: bool,
    pub cancel: bool,
}

/// One step: one stage on the recipe's row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StepView {
    /// 0-based, as the store numbers it. People read `index + 1`.
    pub index: usize,
    /// tool | think | jump_if | wait_for | notify | ask_user | think_cited | validate | render |
    /// format | filter | sort | aggregate | extract | branch | unreadable. Open: more will come
    /// (`agent`), and a reader draws a kind it does not know as a plain stage.
    pub kind: String,
    /// The stage's name on the row: the tool's name, "Think", "Ask you", "Wait"…
    pub label: String,
    /// What it does, in one line. Placeholders stay as written, so `{{topic}}` shows as that.
    pub summary: String,
    /// The store's word for it: pending | done | failed | skipped.
    pub stored: String,
    /// Where it stands on the row: done | current | waiting | failed | skipped | pending |
    /// not_taken (passed over, or never reached) | paused (a paused recipe stopped before it) |
    /// stopped (a cancelled or failed recipe stopped before it) | unknown (the store no longer
    /// has its record: built-in steps are rewritten at every start).
    pub state: String,
    /// The head of what it produced: its output, its answer, or the error it failed with.
    pub result: Option<String>,
    /// JumpIf and Branch: the way the recipe went here, once it went.
    pub path: Option<String>,
    /// Inputs with no value — placeholders and input variables nothing has set or will set.
    /// A placeholder reaches the tool or the model as the literal `{{name}}` (#88).
    pub unbound: Vec<String>,
    pub store_as: Option<String>,
    /// The catalog agent that runs this step (design/desk-and-mind-2026-09-23.md, section 6).
    /// None for every kind today.
    pub agent: Option<String>,
    /// The whole definition, a line per field, for the opened step.
    pub detail: Vec<String>,
}

// ── Mapping ──

/// The view of one recipe, from what the store holds for it.
pub fn view(recipe: &Recipe, steps: &[StoredStep], vars: &Vars) -> RecipeView {
    let status = match recipe.status {
        RecipeStatus::Failed if recipe.error_message.as_deref() == Some(CANCELLED) => "cancelled".to_string(),
        ref s => s.as_str().to_string(),
    };
    let paused_from_waiting = recipe.status == RecipeStatus::Paused
        && vars.get(PAUSED_FROM_VAR).and_then(|v| v.as_str()) == Some("waiting");
    let blocked = recipe.status == RecipeStatus::Waiting || paused_from_waiting;
    let cur = recipe.current_step;

    // The step it waits on is the one it has just moved past, when that step is a wait or a
    // question. Both executors advance the pointer before they wait.
    let waiting_on = if blocked && cur > 0 {
        steps
            .get(cur - 1)
            .filter(|s| matches!(s.step, RecipeStep::WaitFor { .. } | RecipeStep::AskUser { .. }))
            .map(|s| s.step_index)
    } else {
        None
    };

    let finished = matches!(recipe.status, RecipeStatus::Done | RecipeStatus::Failed);
    let touched = |s: &StoredStep| s.status != "pending";
    let any_touched = steps.iter().any(touched);
    let last_touched = steps.iter().filter(|s| touched(s)).map(|s| s.step_index).max();
    // The steps a forward jump passed over, from the jumps that were taken.
    let jumped_over: HashSet<usize> = steps
        .iter()
        .filter(|s| s.status == "done" && s.result.as_deref().is_some_and(|r| r == "jumped" || r.starts_with("jump:")))
        .filter_map(|s| match s.step {
            RecipeStep::JumpIf { target_step, .. } if target_step > s.step_index + 1 => Some(s.step_index + 1..target_step),
            _ => None,
        })
        .flatten()
        .collect();

    let states: Vec<&'static str> = steps
        .iter()
        .map(|s| {
            let i = s.step_index;
            if waiting_on == Some(i) {
                return "waiting";
            }
            match s.status.as_str() {
                "done" => return "done",
                "failed" => return "failed",
                "skipped" => return "skipped",
                _ => {}
            }
            match recipe.status {
                RecipeStatus::Running if i == cur => "current",
                RecipeStatus::Paused if i == cur && waiting_on.is_none() => "paused",
                RecipeStatus::Failed if i == cur => "stopped",
                _ if finished && !any_touched => "unknown",
                _ if finished => "not_taken",
                RecipeStatus::Pending => "pending",
                // Behind the pointer and never run, with something after it run or a jump taken
                // over it: passed over.
                _ if i < cur && (jumped_over.contains(&i) || last_touched.is_some_and(|t| t > i)) => "not_taken",
                _ => "pending",
            }
        })
        .collect();

    // Which variables each step can count on: those set now, and — for a step still to run —
    // those a step before it that is also still to run will set.
    let mut to_be_set: HashSet<String> = HashSet::new();
    let mut step_views = Vec::with_capacity(steps.len());
    for (s, state) in steps.iter().zip(&states) {
        let ran = matches!(*state, "done" | "failed" | "skipped" | "waiting");
        let mut unbound = Vec::new();
        for name in inputs(&s.step) {
            let bound = vars.contains_key(&name) || (!ran && to_be_set.contains(&name));
            if !bound && !unbound.contains(&name) {
                unbound.push(name);
            }
        }
        // A step still to run — or a question still waiting for its answer — will set its own.
        if matches!(*state, "pending" | "current" | "paused" | "waiting") {
            to_be_set.extend(produces(&s.step));
        }
        step_views.push(step_view(s, state, unbound, vars));
    }

    let waiting_step = waiting_on.and_then(|i| steps.iter().find(|s| s.step_index == i));
    let question = waiting_step.and_then(|s| match &s.step {
        RecipeStep::AskUser { question, store_as, choices } => Some(QuestionView {
            step: s.step_index,
            text: crate::recipe::resolve_vars(question, vars),
            choices: choices
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|c| crate::recipe::resolve_vars(c, vars))
                .collect(),
            store_as: store_as.clone(),
        }),
        _ => None,
    });
    let waiting_for = if blocked {
        Some(match waiting_step.map(|s| &s.step) {
            Some(RecipeStep::AskUser { .. }) => "your answer".to_string(),
            Some(RecipeStep::WaitFor { condition, timeout_secs }) => wait_text(condition, *timeout_secs),
            // Waiting with no wait behind it. `get_expired_waiting` resumes these at the next
            // message the companion handles.
            _ => "the next message to resume it".to_string(),
        })
    } else {
        None
    };

    let can = Controls {
        answer: recipe.status == RecipeStatus::Waiting && question.is_some(),
        pause: matches!(recipe.status, RecipeStatus::Running | RecipeStatus::Waiting),
        resume: recipe.status == RecipeStatus::Paused,
        cancel: matches!(recipe.status, RecipeStatus::Running | RecipeStatus::Waiting | RecipeStatus::Paused),
    };

    RecipeView {
        id: recipe.id.clone(),
        name: recipe.name.clone(),
        description: recipe.description.clone(),
        status,
        current_step: cur,
        created_at: recipe.created_at,
        updated_at: recipe.updated_at,
        error: recipe.error_message.clone().filter(|e| !e.is_empty()),
        template: recipe.id.starts_with("builtin_") && recipe.status == RecipeStatus::Pending && !any_touched,
        waiting_for,
        question,
        steps: step_views,
        can,
    }
}

fn step_view(s: &StoredStep, state: &str, unbound: Vec<String>, vars: &Vars) -> StepView {
    let (kind, label, summary, detail) = describe_step(&s.step);
    let store_as = store_as_of(&s.step).map(str::to_string);

    let path = match (&s.step, s.result.as_deref()) {
        (RecipeStep::JumpIf { target_step, .. }, Some(r)) if r == "jumped" || r.starts_with("jump:") => {
            Some(format!("jumped to step {}", target_step + 1))
        }
        (RecipeStep::JumpIf { .. }, Some("continued" | "ok")) => Some(format!("went on to step {}", s.step_index + 2)),
        (RecipeStep::Branch { then_steps, .. }, Some("then")) => Some(format!("took then ({})", count(then_steps.len()))),
        (RecipeStep::Branch { else_steps, .. }, Some("else")) => Some(format!("took else ({})", count(else_steps.len()))),
        _ => None,
    };

    // What it produced. The store's `result` column is uneven — the tool's whole output from one
    // executor, "ok" from the other — so a step that keeps a variable is read from the variable,
    // and the markers the executors write in place of a result are not results.
    let result = if s.status == "failed" {
        s.result.as_deref().map(|r| head(r, RESULT_HEAD))
    } else if state == "waiting" {
        None
    } else if let Some(value) = store_as.as_deref().filter(|_| s.status == "done").and_then(|k| vars.get(k)) {
        Some(head(&value_text(value), RESULT_HEAD)).filter(|t| !t.is_empty())
    } else {
        s.result.as_deref().filter(|r| !is_marker(r)).map(|r| head(r, RESULT_HEAD))
    };

    StepView {
        index: s.step_index,
        kind: kind.to_string(),
        label,
        summary: head(&summary, SUMMARY_MAX),
        stored: s.status.clone(),
        state: state.to_string(),
        result,
        path,
        unbound,
        store_as,
        agent: None,
        detail: detail.into_iter().map(|l| clip(&l, DETAIL_MAX)).collect(),
    }
}

/// Kind, stage label, one-line summary and the opened definition of a step.
fn describe_step(step: &RecipeStep) -> (&'static str, String, String, Vec<String>) {
    let stores = |k: &str| format!("keeps it as: {k}");
    match step {
        RecipeStep::Tool { tool_name, args, store_as, on_error } => (
            "tool",
            tool_name.clone(),
            format!("{tool_name}{}", args_inline(args)),
            vec![
                format!("tool: {tool_name}"),
                format!("arguments: {}", serde_json::to_string(args).unwrap_or_default()),
                stores(store_as),
                format!("on error: {}", error_action_text(on_error)),
            ],
        ),
        RecipeStep::Think { prompt, store_as, fallback_template } => {
            let mut detail = vec![format!("prompt: {prompt}"), stores(store_as)];
            if let Some(t) = fallback_template {
                detail.push(format!("without a model: {t}"));
            }
            ("think", "Think".into(), prompt.clone(), detail)
        }
        RecipeStep::JumpIf { condition, target_step } => (
            "jump_if",
            "If".into(),
            format!("if {} → step {}", condition_text(condition), target_step + 1),
            vec![format!("if: {}", condition_text(condition)), format!("then: go to step {}", target_step + 1)],
        ),
        RecipeStep::WaitFor { condition, timeout_secs } => (
            "wait_for",
            "Wait".into(),
            format!("wait for {}", wait_text(condition, *timeout_secs)),
            vec![format!("waits for: {}", wait_text(condition, *timeout_secs))],
        ),
        RecipeStep::Notify { message } if message.starts_with(UNREADABLE) => (
            "unreadable",
            "Unreadable".into(),
            "this step's definition could not be read".into(),
            vec![message.clone()],
        ),
        RecipeStep::Notify { message } => ("notify", "Notify".into(), message.clone(), vec![format!("says: {message}")]),
        RecipeStep::AskUser { question, store_as, choices } => {
            let offered = choices.as_deref().unwrap_or_default();
            let summary = if offered.is_empty() { question.clone() } else { format!("{question} ({})", offered.join(" / ")) };
            let mut detail = vec![format!("asks: {question}")];
            if !offered.is_empty() {
                detail.push(format!("choices: {}", offered.join(" / ")));
            }
            detail.push(stores(store_as));
            ("ask_user", "Ask you".into(), summary, detail)
        }
        RecipeStep::ThinkCited { prompt, store_as, source_vars } => (
            "think_cited",
            "Cite".into(),
            format!("{prompt} — from {}", source_vars.join(", ")),
            vec![format!("prompt: {prompt}"), format!("sources: {}", source_vars.join(", ")), stores(store_as)],
        ),
        RecipeStep::Validate { input_var, store_as } => (
            "validate",
            "Validate".into(),
            format!("keep the cited claims in {input_var}"),
            vec![format!("checks: {input_var}"), format!("keeps it as: {store_as}, and its report as {store_as}_report")],
        ),
        RecipeStep::Render { input_var, store_as, format } => (
            "render",
            "Render".into(),
            format!("{} of {input_var}", render_text(format)),
            vec![format!("renders: {input_var} as {}", render_text(format)), stores(store_as)],
        ),
        RecipeStep::Format { template, store_as, .. } => {
            ("format", "Format".into(), template.clone(), vec![format!("template: {template}"), stores(store_as)])
        }
        RecipeStep::Filter { input_var, field, op, value, store_as } => {
            let what = format!("{input_var} where {field} {} {value}", filter_text(op));
            ("filter", "Filter".into(), what.clone(), vec![format!("keeps: {what}"), stores(store_as)])
        }
        RecipeStep::Sort { input_var, by_field, descending, store_as } => {
            let what = format!("{input_var} by {by_field}{}", if *descending { ", largest first" } else { "" });
            ("sort", "Sort".into(), what.clone(), vec![format!("sorts: {what}"), stores(store_as)])
        }
        RecipeStep::Aggregate { input_var, op, field, store_as } => {
            let of = match field {
                Some(f) => format!("{input_var}.{f}"),
                None => input_var.clone(),
            };
            let what = format!("{} of {of}", aggregate_text(op));
            ("aggregate", "Aggregate".into(), what.clone(), vec![format!("computes: {what}"), stores(store_as)])
        }
        RecipeStep::Extract { input_var, pattern, store_as } => {
            let what = format!("{pattern} from {input_var}");
            ("extract", "Extract".into(), what.clone(), vec![format!("extracts: {what}"), stores(store_as)])
        }
        RecipeStep::Branch { condition, then_steps, else_steps } => {
            let line = |steps: &[RecipeStep]| {
                if steps.is_empty() {
                    "nothing".to_string()
                } else {
                    steps.iter().map(|s| describe_step(s).1).collect::<Vec<_>>().join(" → ")
                }
            };
            (
                "branch",
                "Branch".into(),
                format!("if {condition}: {} else {}", count(then_steps.len()), count(else_steps.len())),
                vec![
                    format!("if: {condition} is set and not empty, false or 0"),
                    format!("then: {}", line(then_steps)),
                    format!("else: {}", line(else_steps)),
                ],
            )
        }
    }
}

/// The variables a step reads: every `{{name}}` in the text it sends, and the input variables it
/// takes by name. A Branch reads what its steps read.
fn inputs(step: &RecipeStep) -> Vec<String> {
    let mut texts: Vec<&str> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    match step {
        RecipeStep::Tool { args, .. } => {
            let mut out = Vec::new();
            json_strings(args, &mut out);
            return out.iter().flat_map(|s| placeholders(s)).collect();
        }
        RecipeStep::Think { prompt, fallback_template, .. } => {
            texts.push(prompt);
            if let Some(t) = fallback_template {
                texts.push(t);
            }
        }
        RecipeStep::Notify { message } if !message.starts_with(UNREADABLE) => texts.push(message),
        RecipeStep::AskUser { question, choices, .. } => {
            texts.push(question);
            texts.extend(choices.as_deref().unwrap_or_default().iter().map(String::as_str));
        }
        RecipeStep::ThinkCited { prompt, source_vars, .. } => {
            texts.push(prompt);
            names.extend(source_vars.iter().cloned());
        }
        // Format's `input_vars` are not read by either executor; only its template is.
        RecipeStep::Format { template, .. } => texts.push(template),
        RecipeStep::Validate { input_var, .. }
        | RecipeStep::Render { input_var, .. }
        | RecipeStep::Filter { input_var, .. }
        | RecipeStep::Sort { input_var, .. }
        | RecipeStep::Aggregate { input_var, .. }
        | RecipeStep::Extract { input_var, .. } => names.push(input_var.clone()),
        RecipeStep::Branch { then_steps, else_steps, .. } => {
            return then_steps.iter().chain(else_steps).flat_map(inputs).collect();
        }
        RecipeStep::JumpIf { .. } | RecipeStep::WaitFor { .. } | RecipeStep::Notify { .. } => {}
    }
    texts.iter().flat_map(|t| placeholders(t)).chain(names).collect()
}

/// The variables a step sets.
fn produces(step: &RecipeStep) -> Vec<String> {
    match step {
        RecipeStep::Validate { store_as, .. } => vec![store_as.clone(), format!("{store_as}_report")],
        RecipeStep::Branch { then_steps, else_steps, .. } => then_steps.iter().chain(else_steps).flat_map(produces).collect(),
        other => store_as_of(other).map(|s| vec![s.to_string()]).unwrap_or_default(),
    }
}

fn store_as_of(step: &RecipeStep) -> Option<&str> {
    match step {
        RecipeStep::Tool { store_as, .. }
        | RecipeStep::Think { store_as, .. }
        | RecipeStep::AskUser { store_as, .. }
        | RecipeStep::ThinkCited { store_as, .. }
        | RecipeStep::Validate { store_as, .. }
        | RecipeStep::Render { store_as, .. }
        | RecipeStep::Format { store_as, .. }
        | RecipeStep::Filter { store_as, .. }
        | RecipeStep::Sort { store_as, .. }
        | RecipeStep::Aggregate { store_as, .. }
        | RecipeStep::Extract { store_as, .. } => Some(store_as),
        RecipeStep::JumpIf { .. } | RecipeStep::WaitFor { .. } | RecipeStep::Notify { .. } | RecipeStep::Branch { .. } => None,
    }
}

/// Every `{{name}}` in a text, as `resolve_vars` would look it up: the name exactly as written.
pub fn placeholders(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find("{{") {
        let after = &rest[open + 2..];
        let Some(close) = after.find("}}") else { break };
        let name = &after[..close];
        if !name.is_empty() && !name.contains('{') && !name.contains('}') && !out.iter().any(|n| n == name) {
            out.push(name.to_string());
        }
        rest = &after[close + 2..];
    }
    out
}

fn json_strings<'a>(value: &'a serde_json::Value, out: &mut Vec<&'a str>) {
    match value {
        serde_json::Value::String(s) => out.push(s),
        serde_json::Value::Array(items) => items.iter().for_each(|v| json_strings(v, out)),
        serde_json::Value::Object(map) => map.values().for_each(|v| json_strings(v, out)),
        _ => {}
    }
}

/// ` key="value" key2=5`, the way the Agents screen writes a call on one line.
fn args_inline(args: &serde_json::Value) -> String {
    let Some(map) = args.as_object() else {
        return match args {
            serde_json::Value::Null => String::new(),
            other => format!(" {}", clip(&other.to_string(), 60)),
        };
    };
    map.iter()
        .map(|(k, v)| match v {
            serde_json::Value::String(s) => format!(" {k}=\"{}\"", clip(s, 40)),
            other => format!(" {k}={}", clip(&other.to_string(), 40)),
        })
        .collect()
}

fn condition_text(c: &Condition) -> String {
    let joined = |cs: &[Condition], word: &str| {
        let parts: Vec<String> = cs.iter().map(condition_text).collect();
        if parts.len() > 1 { format!("({})", parts.join(word)) } else { parts.concat() }
    };
    match c {
        Condition::VarEquals { var, value } => format!("{var} = {}", value_text(value)),
        Condition::VarContains { var, substring } => format!("{var} contains \"{substring}\""),
        Condition::VarExists { var } => format!("{var} is set"),
        Condition::VarGt { var, threshold } => format!("{var} > {threshold}"),
        Condition::VarEmpty { var } => format!("{var} is empty"),
        Condition::TimeAfter { hour, minute } => format!("after {hour:02}:{minute:02} UTC"),
        Condition::TimeBefore { hour, minute } => format!("before {hour:02}:{minute:02} UTC"),
        Condition::Not { inner } => format!("not {}", condition_text(inner)),
        Condition::And { conditions } => joined(conditions, " and "),
        Condition::Or { conditions } => joined(conditions, " or "),
    }
}

/// What a WaitFor waits on. Its clock is the store's `updated_at` and the executors read time in
/// UTC, so a time of day is said in UTC.
fn wait_text(condition: &WaitCondition, timeout: Option<u64>) -> String {
    let base = match condition {
        WaitCondition::Duration { seconds } => format!("{} to pass", duration(*seconds)),
        WaitCondition::Time { hour, minute } => format!("{hour:02}:{minute:02} UTC"),
    };
    match timeout {
        Some(t) => format!("{base}, {} at most", duration(t)),
        None => base,
    }
}

fn error_action_text(a: &ErrorAction) -> String {
    match a {
        ErrorAction::Fail => "stop the recipe".into(),
        ErrorAction::Skip => "skip the step".into(),
        ErrorAction::Retry { max } => format!("retry, {max} times at most"),
        ErrorAction::JumpTo { step } => format!("go to step {}", step + 1),
        ErrorAction::Replan => "ask the model for new steps".into(),
    }
}

fn render_text(f: &RenderFormat) -> &'static str {
    match f {
        RenderFormat::Summary => "a summary",
        RenderFormat::Table => "a table",
        RenderFormat::Comparison => "a comparison",
        RenderFormat::Cards => "cards",
    }
}

fn filter_text(op: &FilterOp) -> &'static str {
    match op {
        FilterOp::Equals => "=",
        FilterOp::NotEquals => "≠",
        FilterOp::Contains => "contains",
        FilterOp::GreaterThan => ">",
        FilterOp::LessThan => "<",
    }
}

fn aggregate_text(op: &AggregateOp) -> &'static str {
    match op {
        AggregateOp::Count => "count",
        AggregateOp::Sum => "sum",
        AggregateOp::Min => "least",
        AggregateOp::Max => "most",
        AggregateOp::Avg => "average",
    }
}

/// What the executors write in the result column in place of a result.
fn is_marker(result: &str) -> bool {
    matches!(
        result,
        "ok" | "done" | "waiting" | "asked" | "notified" | "validated" | "rendered" | "jumped" | "continued" | "then" | "else"
    ) || result.starts_with("jump:")
}

fn value_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn count(n: usize) -> String {
    if n == 1 { "1 step".into() } else { format!("{n} steps") }
}

/// Seconds as a person says them: 45s, 5m, 1h 30m, 2d.
pub fn duration(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 if secs % 60 == 0 => format!("{}m", secs / 60),
        60..=3599 => format!("{}m {}s", secs / 60, secs % 60),
        3600..=86_399 if secs % 3600 == 0 => format!("{}h", secs / 3600),
        3600..=86_399 => format!("{}h {}m", secs / 3600, (secs % 3600) / 60),
        _ if secs % 86_400 == 0 => format!("{}d", secs / 86_400),
        _ => format!("{}d {}h", secs / 86_400, (secs % 86_400) / 3600),
    }
}

/// One line: whitespace folded, cut at a character boundary with an ellipsis.
pub fn head(text: &str, max: usize) -> String {
    clip(&text.split_whitespace().collect::<Vec<_>>().join(" "), max)
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

// ── Reading the store ──

/// Every recipe the store holds, as the desk shows them, in the desk's order.
pub fn list(conn: &Connection) -> Vec<RecipeView> {
    let mut views: Vec<RecipeView> = RecipeStore::list(conn, None, LIST_LIMIT)
        .iter()
        .map(|r| view(r, &RecipeStore::get_steps(conn, &r.id), &RecipeStore::get_vars(conn, &r.id)))
        .collect();
    sort_for_desk(&mut views);
    views
}

/// In flight first, and of those the ones waiting for a person first; then the ones not started,
/// then the finished; the built-in definitions last. Newest first within each.
pub fn sort_for_desk(views: &mut [RecipeView]) {
    fn rank(v: &RecipeView) -> u8 {
        if v.template {
            return 6;
        }
        match v.status.as_str() {
            "waiting" if v.question.is_some() => 0,
            "running" => 1,
            "waiting" => 2,
            "paused" => 3,
            "pending" => 4,
            _ => 5,
        }
    }
    views.sort_by(|a, b| {
        rank(a)
            .cmp(&rank(b))
            .then_with(|| if a.template { a.name.cmp(&b.name) } else { b.updated_at.total_cmp(&a.updated_at) })
    });
}

/// Running, waiting or paused: what the mind panel lists under Working.
pub fn is_in_flight(view: &RecipeView) -> bool {
    matches!(view.status.as_str(), "running" | "waiting" | "paused")
}

/// The recipe in one line, for the mind panel: where it is and what it is doing there.
pub fn one_line(view: &RecipeView) -> String {
    let total = view.steps.len();
    let at = |i: usize| {
        let label = view.steps.get(i).map(|s| s.label.as_str()).unwrap_or("");
        if label.is_empty() {
            format!("step {} of {total}", i + 1)
        } else {
            format!("step {} of {total}, {label}", i + 1)
        }
    };
    let name = &view.name;
    match view.status.as_str() {
        "running" => format!("{name} — {}", at(view.current_step)),
        "waiting" => match &view.question {
            Some(q) => format!("{name} — waiting for your answer: {}", head(&q.text, 80)),
            None => format!(
                "{name} — {}, waiting for {}",
                at(view.current_step.saturating_sub(1)),
                view.waiting_for.as_deref().unwrap_or("its turn")
            ),
        },
        "paused" => format!("{name} — paused at {}", at(view.current_step.min(total.saturating_sub(1)))),
        "done" => format!("{name} — done"),
        "cancelled" => format!("{name} — cancelled"),
        "failed" => {
            let at_step = view.steps.iter().find(|s| s.state == "failed").map(|s| s.index).unwrap_or(view.current_step);
            format!(
                "{name} — failed at step {}: {}",
                at_step + 1,
                head(view.error.as_deref().unwrap_or("no error recorded"), 80)
            )
        }
        _ => format!("{name} — not started"),
    }
}

// ── What a person can do ──

/// A person's say over one recipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecipeOp {
    /// The answer to the question it is waiting on.
    Answer(String),
    Pause,
    Resume,
    Cancel,
}

impl RecipeOp {
    pub fn verb(&self) -> &'static str {
        match self {
            RecipeOp::Answer(_) => "answer",
            RecipeOp::Pause => "pause",
            RecipeOp::Resume => "resume",
            RecipeOp::Cancel => "cancel",
        }
    }
}

/// What [`apply`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    /// A sentence for the person.
    pub message: String,
    /// The recipe is running again: signal the executor now.
    pub run_now: bool,
}

/// Answer, pause, resume or cancel one recipe, through the store's own door for each: the answer
/// through the chat's AskUser path ([`crate::interjection::answer`]), the rest through
/// [`RecipeStore::pause`], [`RecipeStore::resume`] and [`RecipeStore::cancel`].
pub fn apply(conn: &Connection, recipe_id: &str, op: &RecipeOp) -> Result<Applied, String> {
    let name = RecipeStore::get(conn, recipe_id)
        .map(|r| r.name)
        .ok_or_else(|| format!("no recipe `{recipe_id}`"))?;
    match op {
        RecipeOp::Answer(text) => {
            crate::interjection::answer(conn, recipe_id, text)?;
            Ok(Applied { message: format!("Answered — '{name}' goes on."), run_now: true })
        }
        RecipeOp::Pause => {
            RecipeStore::pause(conn, recipe_id)?;
            Ok(Applied { message: format!("Paused '{name}'."), run_now: false })
        }
        RecipeOp::Resume => {
            let now = RecipeStore::resume(conn, recipe_id)?;
            let run_now = now == RecipeStatus::Running;
            let message = if run_now {
                format!("Resumed '{name}'.")
            } else {
                format!("Resumed '{name}' — it is waiting again.")
            };
            Ok(Applied { message, run_now })
        }
        RecipeOp::Cancel => {
            RecipeStore::cancel(conn, recipe_id)?;
            Ok(Applied { message: format!("Cancelled '{name}'."), run_now: false })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn recipe(status: RecipeStatus, current_step: usize) -> Recipe {
        Recipe {
            id: "rcp_test".into(),
            name: "Tidy downloads".into(),
            description: String::new(),
            status,
            current_step,
            created_at: 100.0,
            updated_at: 200.0,
            enabled: true,
            error_message: None,
        }
    }

    fn stored(steps: Vec<RecipeStep>, statuses: &[&str]) -> Vec<StoredStep> {
        steps
            .into_iter()
            .enumerate()
            .map(|(i, step)| StoredStep {
                step_index: i,
                step,
                status: statuses.get(i).copied().unwrap_or("pending").to_string(),
                result: None,
            })
            .collect()
    }

    fn tool(name: &str, args: serde_json::Value, store_as: &str) -> RecipeStep {
        RecipeStep::Tool { tool_name: name.into(), args, store_as: store_as.into(), on_error: ErrorAction::Fail }
    }

    fn ask(q: &str, choices: &[&str], store_as: &str) -> RecipeStep {
        RecipeStep::AskUser {
            question: q.into(),
            store_as: store_as.into(),
            choices: if choices.is_empty() { None } else { Some(choices.iter().map(|c| c.to_string()).collect()) },
        }
    }

    fn states(v: &RecipeView) -> Vec<&str> {
        v.steps.iter().map(|s| s.state.as_str()).collect()
    }

    /// Every kind the engine has, each with its kind, its stage label and a line that says what it
    /// does. A kind added to `RecipeStep` fails to compile in `describe_step` until it is drawn.
    #[test]
    fn every_step_kind_has_a_kind_a_label_and_a_line() {
        let all = vec![
            tool("web_search", json!({"query": "{{topic}} news", "limit": 5}), "hits"),
            RecipeStep::Think { prompt: "Summarise {{hits}}".into(), store_as: "summary".into(), fallback_template: None },
            RecipeStep::JumpIf {
                condition: Condition::And {
                    conditions: vec![Condition::VarExists { var: "summary".into() }, Condition::VarGt { var: "n".into(), threshold: 3.0 }],
                },
                target_step: 6,
            },
            RecipeStep::WaitFor { condition: WaitCondition::Duration { seconds: 300 }, timeout_secs: Some(3600) },
            RecipeStep::Notify { message: "Found {{summary}}".into() },
            ask("Which folder?", &["Downloads", "Desktop"], "folder"),
            RecipeStep::ThinkCited { prompt: "What changed?".into(), store_as: "cited".into(), source_vars: vec!["hits".into()] },
            RecipeStep::Validate { input_var: "cited".into(), store_as: "checked".into() },
            RecipeStep::Render { input_var: "checked".into(), store_as: "shown".into(), format: RenderFormat::Table },
            RecipeStep::Format { input_vars: vec![], template: "{{shown}} ({{folder}})".into(), store_as: "text".into() },
            RecipeStep::Filter { input_var: "hits".into(), field: "score".into(), op: FilterOp::GreaterThan, value: "0.5".into(), store_as: "good".into() },
            RecipeStep::Sort { input_var: "good".into(), by_field: "date".into(), descending: true, store_as: "sorted".into() },
            RecipeStep::Aggregate { input_var: "sorted".into(), op: AggregateOp::Count, field: None, store_as: "n".into() },
            RecipeStep::Extract { input_var: "sorted".into(), pattern: "0.title".into(), store_as: "top".into() },
            RecipeStep::Branch {
                condition: "top".into(),
                then_steps: vec![RecipeStep::Notify { message: "{{top}}".into() }],
                else_steps: vec![],
            },
            RecipeStep::Notify { message: "PARSE ERROR: {\"type\":\"Agent\"}".into() },
        ];
        let v = view(&recipe(RecipeStatus::Pending, 0), &stored(all, &[]), &Vars::new());
        let got: Vec<(&str, &str)> = v.steps.iter().map(|s| (s.kind.as_str(), s.label.as_str())).collect();
        assert_eq!(
            got,
            vec![
                ("tool", "web_search"),
                ("think", "Think"),
                ("jump_if", "If"),
                ("wait_for", "Wait"),
                ("notify", "Notify"),
                ("ask_user", "Ask you"),
                ("think_cited", "Cite"),
                ("validate", "Validate"),
                ("render", "Render"),
                ("format", "Format"),
                ("filter", "Filter"),
                ("sort", "Sort"),
                ("aggregate", "Aggregate"),
                ("extract", "Extract"),
                ("branch", "Branch"),
                ("unreadable", "Unreadable"),
            ]
        );
        let line = |i: usize| v.steps[i].summary.as_str();
        assert!(line(0).starts_with("web_search ") && line(0).contains(r#" query="{{topic}} news""#) && line(0).contains(" limit=5"), "{}", line(0));
        assert_eq!(line(1), "Summarise {{hits}}");
        assert_eq!(line(2), "if (summary is set and n > 3) → step 7");
        assert_eq!(line(3), "wait for 5m to pass, 1h at most");
        assert_eq!(line(5), "Which folder? (Downloads / Desktop)");
        assert_eq!(line(6), "What changed? — from hits");
        assert_eq!(line(8), "a table of checked");
        assert_eq!(line(10), "hits where score > 0.5");
        assert_eq!(line(11), "good by date, largest first");
        assert_eq!(line(12), "count of sorted");
        assert_eq!(line(13), "0.title from sorted");
        assert_eq!(line(14), "if top: 1 step else 0 steps");
        assert_eq!(line(15), "this step's definition could not be read");
        assert!(v.steps[14].detail.iter().any(|d| d == "then: Notify"), "{:?}", v.steps[14].detail);
        assert!(v.steps[0].detail.iter().any(|d| d == "on error: stop the recipe"));
        // No kind names an agent yet; the field is there for the Agent step.
        assert!(v.steps.iter().all(|s| s.agent.is_none()));
        assert_eq!(v.steps[0].store_as.as_deref(), Some("hits"));
    }

    /// Each status the store has, and the one the view derives, with where the steps stand.
    #[test]
    fn every_status_and_where_its_steps_stand() {
        let steps = || vec![tool("a", json!({}), "x"), tool("b", json!({}), "y"), tool("c", json!({}), "z")];

        let pending = view(&recipe(RecipeStatus::Pending, 0), &stored(steps(), &[]), &Vars::new());
        assert_eq!(pending.status, "pending");
        assert_eq!(states(&pending), ["pending", "pending", "pending"]);
        assert_eq!(pending.can, Controls::default());

        let running = view(&recipe(RecipeStatus::Running, 1), &stored(steps(), &["done"]), &Vars::new());
        assert_eq!(running.status, "running");
        assert_eq!(states(&running), ["done", "current", "pending"]);
        assert_eq!(running.can, Controls { answer: false, pause: true, resume: false, cancel: true });

        let paused = view(&recipe(RecipeStatus::Paused, 1), &stored(steps(), &["done"]), &Vars::new());
        assert_eq!(paused.status, "paused");
        assert_eq!(states(&paused), ["done", "paused", "pending"]);
        assert_eq!(paused.can, Controls { answer: false, pause: false, resume: true, cancel: true });
        assert_eq!(paused.waiting_for, None);

        let done = view(&recipe(RecipeStatus::Done, 3), &stored(steps(), &["done", "done", "done"]), &Vars::new());
        assert_eq!(done.status, "done");
        assert_eq!(states(&done), ["done", "done", "done"]);
        assert_eq!(done.can, Controls::default());

        let mut failed_r = recipe(RecipeStatus::Failed, 1);
        failed_r.error_message = Some("Unknown tool: b".into());
        let mut failed_s = stored(steps(), &["done", "failed"]);
        failed_s[1].result = Some("Unknown tool: b".into());
        let failed = view(&failed_r, &failed_s, &Vars::new());
        assert_eq!(failed.status, "failed");
        assert_eq!(states(&failed), ["done", "failed", "not_taken"]);
        assert_eq!(failed.steps[1].result.as_deref(), Some("Unknown tool: b"));
        assert_eq!(failed.error.as_deref(), Some("Unknown tool: b"));

        let mut cancelled_r = recipe(RecipeStatus::Failed, 1);
        cancelled_r.error_message = Some(CANCELLED.into());
        let cancelled = view(&cancelled_r, &stored(steps(), &["done"]), &Vars::new());
        assert_eq!(cancelled.status, "cancelled");
        assert_eq!(states(&cancelled), ["done", "stopped", "not_taken"]);

        // A finished built-in whose step record the next start rewrote: said so, not "not run".
        let reset = view(&recipe(RecipeStatus::Done, 3), &stored(steps(), &[]), &Vars::new());
        assert_eq!(states(&reset), ["unknown", "unknown", "unknown"]);

        // A built-in definition that never ran is a template.
        let mut builtin = recipe(RecipeStatus::Pending, 0);
        builtin.id = "builtin_morning_briefing".into();
        assert!(view(&builtin, &stored(steps(), &[]), &Vars::new()).template);
        assert!(!pending.template);
    }

    /// A question draws itself: what it asks, its choices, and an answer box that is open only
    /// while the recipe waits on it. The executors advance past the AskUser before waiting.
    #[test]
    fn a_waiting_question_is_the_step_it_waits_on() {
        let steps = vec![
            tool("list_dir", json!({"path": "~/Downloads"}), "files"),
            ask("Move {{count}} files to which folder?", &["Archive", "Trash"], "folder"),
            RecipeStep::Notify { message: "Moved to {{folder}}".into() },
        ];
        let mut s = stored(steps, &["done", "done"]);
        s[1].result = Some("asked".into());
        let vars = Vars::from([("files".into(), json!("a.zip\nb.pdf")), ("count".into(), json!(2))]);
        let v = view(&recipe(RecipeStatus::Waiting, 2), &s, &vars);
        assert_eq!(states(&v), ["done", "waiting", "pending"]);
        assert_eq!(v.waiting_for.as_deref(), Some("your answer"));
        let q = v.question.as_ref().expect("the question");
        assert_eq!((q.step, q.text.as_str(), q.store_as.as_str()), (1, "Move 2 files to which folder?", "folder"));
        assert_eq!(q.choices, ["Archive", "Trash"]);
        assert!(v.can.answer && v.can.pause && v.can.cancel);
        // "asked" is the executor's marker, not an answer.
        assert_eq!(v.steps[1].result, None);
        // The notification's {{folder}} is what the question will set: not unbound while it waits.
        assert!(v.steps[2].unbound.is_empty(), "{:?}", v.steps[2].unbound);
        assert_eq!(v.steps[0].result.as_deref(), Some("a.zip b.pdf"));

        // Paused while waiting: still the question, and no answer box until it is resumed.
        let mut paused_vars = vars.clone();
        paused_vars.insert(PAUSED_FROM_VAR.into(), json!("waiting"));
        let p = view(&recipe(RecipeStatus::Paused, 2), &s, &paused_vars);
        assert_eq!(states(&p), ["done", "waiting", "pending"]);
        assert!(p.question.is_some());
        assert!(!p.can.answer && p.can.resume);

        // A timer says what it counts.
        let timer = stored(vec![RecipeStep::WaitFor { condition: WaitCondition::Time { hour: 9, minute: 0 }, timeout_secs: None }, tool("x", json!({}), "x")], &["done"]);
        let t = view(&recipe(RecipeStatus::Waiting, 1), &timer, &Vars::new());
        assert_eq!(t.waiting_for.as_deref(), Some("09:00 UTC"));
        assert!(t.question.is_none() && !t.can.answer);
    }

    /// #88's symptom, made visible: a placeholder nothing binds is listed as unbound and stays in
    /// the step's line as written, because that is what reaches the tool.
    #[test]
    fn a_placeholder_with_no_value_is_shown_as_such() {
        let steps = vec![
            tool("recall", json!({"query": "{{topic}}"}), "found"),
            RecipeStep::Think { prompt: "Summarise {{found}} about {{topic}}".into(), store_as: "summary".into(), fallback_template: None },
            RecipeStep::Filter { input_var: "rows".into(), field: "x".into(), op: FilterOp::Equals, value: "1".into(), store_as: "kept".into() },
        ];
        // Never run: {{topic}} has no value and nothing will set it; {{found}} will be set by step 1.
        let template = view(&recipe(RecipeStatus::Pending, 0), &stored(steps.clone(), &[]), &Vars::new());
        assert_eq!(template.steps[0].unbound, ["topic"]);
        assert_eq!(template.steps[0].summary, r#"recall query="{{topic}}""#);
        assert_eq!(template.steps[1].unbound, ["topic"]);
        assert_eq!(template.steps[2].unbound, ["rows"], "an input variable nothing sets");

        // Bound by the caller: nothing is unbound.
        let bound = Vars::from([("topic".into(), json!("rust")), ("rows".into(), json!("[]"))]);
        let v = view(&recipe(RecipeStatus::Pending, 0), &stored(steps.clone(), &[]), &bound);
        assert!(v.steps.iter().all(|s| s.unbound.is_empty()));

        // Ran without it: the step that ran is marked, with the literal it was given.
        let ran = Vars::from([("found".into(), json!("Found memories: none"))]);
        let v = view(&recipe(RecipeStatus::Running, 1), &stored(steps, &["done"]), &ran);
        assert_eq!(v.steps[0].unbound, ["topic"]);
        assert_eq!(v.steps[0].result.as_deref(), Some("Found memories: none"));
        assert_eq!(v.steps[1].unbound, ["topic"]);
        assert_eq!(placeholders("{{a}} {{ b }} {{a}} {{}} {{c"), ["a", " b "]);
    }

    /// JumpIf and Branch say which way they went, and the steps a jump passed over are drawn as
    /// not taken rather than as still to come.
    #[test]
    fn the_path_taken_is_shown() {
        let steps = vec![
            RecipeStep::JumpIf { condition: Condition::VarEmpty { var: "inbox".into() }, target_step: 3 },
            tool("summarise", json!({}), "s"),
            RecipeStep::Notify { message: "{{s}}".into() },
            RecipeStep::Branch { condition: "urgent".into(), then_steps: vec![RecipeStep::Notify { message: "!".into() }], else_steps: vec![] },
            RecipeStep::Notify { message: "bye".into() },
        ];
        let mut s = stored(steps.clone(), &["done", "pending", "pending", "done", "done"]);
        s[0].result = Some("jumped".into());
        s[3].result = Some("else".into());
        s[4].result = Some("notified".into());
        let v = view(&recipe(RecipeStatus::Done, 5), &s, &Vars::new());
        assert_eq!(states(&v), ["done", "not_taken", "not_taken", "done", "done"]);
        assert_eq!(v.steps[0].path.as_deref(), Some("jumped to step 4"));
        assert_eq!(v.steps[3].path.as_deref(), Some("took else (0 steps)"));
        assert_eq!(v.steps[4].result, None, "a marker is not a result");

        // Still running, just after the jump: the passed-over steps are not taken, the target is lit.
        let mut s = stored(steps.clone(), &["done"]);
        s[0].result = Some("jump:3".into());
        let v = view(&recipe(RecipeStatus::Running, 3), &s, &Vars::new());
        assert_eq!(states(&v), ["done", "not_taken", "not_taken", "current", "pending"]);
        let mut s = stored(steps, &["done", "pending", "pending", "done"]);
        s[0].result = Some("jumped".into());
        s[3].result = Some("then".into());
        let v = view(&recipe(RecipeStatus::Running, 4), &s, &Vars::new());
        assert_eq!(states(&v), ["done", "not_taken", "not_taken", "done", "current"]);
        assert_eq!(v.steps[3].path.as_deref(), Some("took then (1 step)"));
        let mut went_on = stored(vec![RecipeStep::JumpIf { condition: Condition::VarExists { var: "x".into() }, target_step: 0 }], &["done"]);
        went_on[0].result = Some("continued".into());
        assert_eq!(view(&recipe(RecipeStatus::Done, 1), &went_on, &Vars::new()).steps[0].path.as_deref(), Some("went on to step 2"));
    }

    #[test]
    fn the_desk_order_puts_what_needs_you_and_what_runs_first() {
        let mk = |id: &str, status: &str, question: bool, template: bool, updated_at: f64| RecipeView {
            id: id.into(),
            name: id.into(),
            description: String::new(),
            status: status.into(),
            current_step: 0,
            created_at: 0.0,
            updated_at,
            error: None,
            template,
            waiting_for: None,
            question: question.then(|| QuestionView { step: 0, text: "?".into(), choices: vec![], store_as: "a".into() }),
            steps: vec![],
            can: Controls::default(),
        };
        let mut all = vec![
            mk("t_b", "pending", false, true, 9.0),
            mk("done_old", "done", false, false, 1.0),
            mk("run", "running", false, false, 2.0),
            mk("t_a", "pending", false, true, 9.0),
            mk("ask", "waiting", true, false, 1.0),
            mk("timer", "waiting", false, false, 5.0),
            mk("done_new", "failed", false, false, 8.0),
            mk("held", "paused", false, false, 3.0),
            mk("new", "pending", false, false, 4.0),
        ];
        sort_for_desk(&mut all);
        let order: Vec<&str> = all.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(order, ["ask", "run", "timer", "held", "new", "done_new", "done_old", "t_a", "t_b"]);
        assert_eq!(all.iter().filter(|v| is_in_flight(v)).count(), 4);
    }

    #[test]
    fn one_line_says_where_it_is() {
        let steps = vec![tool("list_dir", json!({}), "f"), ask("Which folder?", &[], "folder"), RecipeStep::Notify { message: "ok".into() }];
        let running = view(&recipe(RecipeStatus::Running, 0), &stored(steps.clone(), &[]), &Vars::new());
        assert_eq!(one_line(&running), "Tidy downloads — step 1 of 3, list_dir");
        let waiting = view(&recipe(RecipeStatus::Waiting, 2), &stored(steps.clone(), &["done", "done"]), &Vars::new());
        assert_eq!(one_line(&waiting), "Tidy downloads — waiting for your answer: Which folder?");
        let paused = view(&recipe(RecipeStatus::Paused, 2), &stored(steps.clone(), &["done", "done"]), &Vars::new());
        assert_eq!(one_line(&paused), "Tidy downloads — paused at step 3 of 3, Notify");
        let mut r = recipe(RecipeStatus::Failed, 0);
        r.error_message = Some("Unknown tool: list_dir".into());
        let failed = view(&r, &stored(steps, &["failed"]), &Vars::new());
        assert_eq!(one_line(&failed), "Tidy downloads — failed at step 1: Unknown tool: list_dir");
        assert_eq!(duration(90), "1m 30s");
        assert_eq!(duration(7200), "2h");
        assert_eq!(head("a\n  b\tc", 10), "a b c");
        assert_eq!(head("abcdefghijkl", 6), "abcde…");
    }

    // ── The store, in memory ──

    fn store() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory sqlite");
        RecipeStore::ensure_tables(&conn);
        conn
    }

    /// What both executors do on an AskUser: mark it, advance past it, and wait.
    fn put_the_question(conn: &Connection, id: &str, step: usize) {
        RecipeStore::complete_step(conn, id, step, "asked");
        RecipeStore::update_status(conn, id, &RecipeStatus::Waiting, step + 1);
    }

    /// The whole round from the screen: a recipe asks, waits — and keeps waiting through the
    /// executor's expiry sweep — until the answer comes, and then runs on with it bound.
    #[test]
    fn answering_a_question_resumes_the_recipe() {
        let conn = store();
        let id = RecipeStore::create(
            &conn,
            "Tidy downloads",
            "",
            &[
                tool("list_dir", json!({"path": "~/Downloads"}), "files"),
                ask("Which folder?", &["Archive", "Trash"], "folder"),
                RecipeStep::Notify { message: "Moved to {{folder}}".into() },
            ],
            None,
        );
        RecipeStore::complete_step(&conn, &id, 0, "a.zip");
        RecipeStore::set_var(&conn, &id, "files", &json!("a.zip"));
        put_the_question(&conn, &id, 1);

        let waiting = list(&conn).into_iter().find(|v| v.id == id).unwrap();
        assert_eq!(waiting.status, "waiting");
        assert!(waiting.can.answer);
        assert_eq!(waiting.question.as_ref().map(|q| q.text.as_str()), Some("Which folder?"));

        // The executors' sweep must leave a question alone until it is answered.
        assert!(!RecipeStore::get_expired_waiting(&conn).contains(&id), "a question is not a timer");
        assert!(RecipeStore::get_resumable(&conn).is_empty());

        // An empty answer, and an answer to a recipe that is not asking, are refused.
        assert!(apply(&conn, &id, &RecipeOp::Answer("  ".into())).is_err());
        let applied = apply(&conn, &id, &RecipeOp::Answer("Archive".into())).expect("answered");
        assert!(applied.run_now);
        assert_eq!(RecipeStore::get_resumable(&conn), vec![id.clone()], "running again, for the executor to take");

        let vars = RecipeStore::get_vars(&conn, &id);
        assert_eq!(vars.get("folder"), Some(&json!("Archive")));
        assert_eq!(crate::recipe::resolve_vars("Moved to {{folder}}", &vars), "Moved to Archive");
        let after = list(&conn).into_iter().find(|v| v.id == id).unwrap();
        assert_eq!(after.status, "running");
        assert_eq!(states(&after), ["done", "done", "current"]);
        assert_eq!(after.steps[1].result.as_deref(), Some("Archive"));
        assert!(after.steps[2].unbound.is_empty());
        assert!(after.question.is_none() && !after.can.answer);
        let again = apply(&conn, &id, &RecipeOp::Answer("Trash".into())).unwrap_err();
        assert!(again.contains("not waiting for an answer"), "{again}");
    }

    #[test]
    fn pause_resume_and_cancel_go_through_the_store() {
        let conn = store();
        let id = RecipeStore::create(&conn, "Digest", "", &[tool("a", json!({}), "x"), ask("Send it?", &["yes", "no"], "ok")], None);
        RecipeStore::update_status(&conn, &id, &RecipeStatus::Running, 0);

        // Running → paused: out of the executor's hands, and back in on resume.
        assert!(apply(&conn, &id, &RecipeOp::Resume).is_err(), "only a paused recipe resumes");
        assert!(!apply(&conn, &id, &RecipeOp::Pause).unwrap().run_now);
        assert_eq!(RecipeStore::get(&conn, &id).unwrap().status, RecipeStatus::Paused);
        assert!(RecipeStore::get_resumable(&conn).is_empty());
        assert!(RecipeStore::get_expired_waiting(&conn).is_empty());
        assert!(apply(&conn, &id, &RecipeOp::Pause).is_err(), "already paused");
        assert!(apply(&conn, &id, &RecipeOp::Resume).unwrap().run_now);
        assert_eq!(RecipeStore::get_resumable(&conn), vec![id.clone()]);

        // Waiting on a question → paused → resumed: waiting on the same question again.
        RecipeStore::complete_step(&conn, &id, 0, "done");
        put_the_question(&conn, &id, 1);
        apply(&conn, &id, &RecipeOp::Pause).unwrap();
        assert!(apply(&conn, &id, &RecipeOp::Answer("yes".into())).is_err(), "resume first");
        let resumed = apply(&conn, &id, &RecipeOp::Resume).unwrap();
        assert!(!resumed.run_now);
        let r = RecipeStore::get(&conn, &id).unwrap();
        assert_eq!((r.status, r.current_step), (RecipeStatus::Waiting, 2));
        assert!(list(&conn)[0].can.answer);

        // Cancel: failed with the cancel's own words, drawn as cancelled, and then final.
        apply(&conn, &id, &RecipeOp::Cancel).unwrap();
        let v = list(&conn).into_iter().find(|v| v.id == id).unwrap();
        assert_eq!(v.status, "cancelled");
        assert_eq!(v.can, Controls::default());
        assert!(apply(&conn, &id, &RecipeOp::Cancel).is_err());
        assert!(apply(&conn, &id, &RecipeOp::Pause).is_err());
        assert!(apply(&conn, "rcp_nope", &RecipeOp::Pause).unwrap_err().contains("no recipe"));
        assert_eq!(RecipeStatus::from_str("paused"), RecipeStatus::Paused);
    }

    /// A hundred recipes made in one breath each get an id of their own (#173): the id was the
    /// UUID's clock prefix, the same for about a minute, and the second insert panicked.
    #[test]
    fn recipes_made_together_get_ids_of_their_own() {
        let conn = store();
        let ids: HashSet<String> = (0..100)
            .map(|n| RecipeStore::create(&conn, &format!("council seat {n}"), "", &[tool("a", json!({}), "x")], None))
            .collect();
        assert_eq!(ids.len(), 100);
        assert_eq!(RecipeStore::list(&conn, None, 200).len(), 100);
    }

    /// The chat's "pause" holds now: it used to write `waiting`, which the expiry sweep resumed at
    /// the next message.
    #[test]
    fn the_chat_s_pause_holds_until_resumed() {
        let conn = store();
        let id = RecipeStore::create(&conn, "Digest", "", &[tool("a", json!({}), "x"), tool("b", json!({}), "y")], None);
        RecipeStore::complete_step(&conn, &id, 0, "done");
        RecipeStore::update_status(&conn, &id, &RecipeStatus::Running, 1);
        crate::interjection::handle(&conn, &crate::interjection::Interjection::Pause).expect("a reply");
        assert_eq!(RecipeStore::get(&conn, &id).unwrap().status, RecipeStatus::Paused);
        assert!(RecipeStore::get_expired_waiting(&conn).is_empty());
        assert!(RecipeStore::get_resumable(&conn).is_empty());
    }
}
