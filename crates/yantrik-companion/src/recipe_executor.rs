//! The recipe executor — the one that runs every recipe, in the shell and anywhere else.
//!
//! One step at a time. [`step`] runs the step a recipe stands at — or the next step inside the
//! Branch it stands in — records what came of it, and says whether there is more to run now
//! ([`Advance::Next`]), whether the recipe waits on its clock or on a person
//! ([`Advance::Blocked`]), or whether it has stopped ([`Advance::Stopped`]). The shell's companion
//! worker calls it once per `ProcessRecipeStep` and signals itself again on `Next`, so a person's
//! message is taken between any two steps, and its clock asks [`due`] which waits are over.
//! [`tick`] runs the same steps in a loop, for a host with no worker (`background::run_think_cycle`).
//!
//! There were two executors. The shell's worker had its own in its command loop and never ran
//! this one, so ThinkCited, Validate, Render and the data steps were passed through, and a Branch
//! was marked taken without either side running. That one is gone (#176): the shell runs this.
//!
//! Where a recipe is, beyond its step pointer, is kept in its variables, so a restart loses
//! nothing: `_wait` (what it waits on, and when a timer wakes), `_branch` (the Branch arms it is
//! inside), `_trail` (what each step did, run by run: the way a Branch went, a loop's rounds) and
//! `_since_wait` (steps run since it last waited, which [`STEP_BUDGET`] bounds).
//!
//! What a step needs from outside — the store, the tools, the model, a way to tell the person —
//! comes through [`RecipeHost`]. `CompanionService` is the host in the shell; a test fakes one.

use std::collections::HashMap;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::json;
use yantrik_ml::{ChatMessage, GenerationConfig};

use crate::companion::CompanionService;
use crate::recipe::{
    clock_text, resolve_vars, resolve_vars_in_json, waited_on, wakes_at, AggregateOp, ErrorAction, FilterOp, Recipe,
    RecipeStatus, RecipeStep, RecipeStore, StoredStep, Trail, WaitRecord, Waited, BRANCH_VAR, SINCE_WAIT_VAR,
    STEP_BUDGET_VAR, UNREADABLE, WAIT_VAR,
};

type Vars = HashMap<String, serde_json::Value>;

/// How many steps a recipe may run without a pause — a timer or a question — before it is
/// stopped as a loop that never ends. A recipe that needs more sets `_step_budget`.
pub const STEP_BUDGET: u64 = 100;

/// How often the worker's clock asks [`due`] for waits that are over, in seconds.
pub const CLOCK_SECS: u64 = 5;

/// Steps one recipe may take per [`tick`].
const MAX_STEPS_PER_TICK: usize = 10;

/// Maximum result size stored per step (bytes).
const MAX_RESULT_SIZE: usize = 4000;

/// What a step needs from outside the store's rows.
pub trait RecipeHost {
    /// The recipe store, for one call. Not held across a step: a tool the recipe runs opens it too.
    fn with_conn<R>(&self, f: impl FnOnce(&Connection) -> R) -> R;
    /// Run a tool by name, with no model in the loop.
    fn run_tool(&mut self, name: &str, args: &serde_json::Value) -> String;
    /// One call to the model. `precise` asks for a low temperature, for output that is parsed.
    fn generate(&mut self, system: &str, prompt: &str, precise: bool) -> Result<String, String>;
    /// Tell the person something, as the companion's own message.
    fn notify(&mut self, recipe_id: &str, text: &str);
    /// Who the model speaks as.
    fn persona(&self) -> String;
    /// Now, in unix seconds.
    fn now(&self) -> f64 {
        now_ts()
    }
}

/// What came of one call to [`step`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Advance {
    /// A step ran and there is more to run: signal again.
    Next,
    /// Waiting on its clock or on a person, or paused: the clock, the answer or a resume moves it.
    Blocked,
    /// Finished, failed, cancelled — or never started. Nothing to do.
    Stopped,
}

/// Run the next step of one recipe.
pub fn step<H: RecipeHost>(host: &mut H, recipe_id: &str) -> Advance {
    let loaded = host.with_conn(|c| {
        RecipeStore::get(c, recipe_id).map(|r| (r, RecipeStore::get_steps(c, recipe_id), RecipeStore::get_vars(c, recipe_id)))
    });
    let Some((recipe, steps, vars)) = loaded else {
        tracing::warn!(recipe_id, "Recipe not found");
        return Advance::Stopped;
    };
    let now = host.now();
    match recipe.status {
        RecipeStatus::Running => {}
        // Only its clock or its answer moves a waiting recipe. A signal left over from an earlier
        // chain used to walk it on at once: past a timer, or past a question with the answer unbound.
        RecipeStatus::Waiting => {
            let waited = waited_on(&recipe, &steps, &vars);
            if waited.as_ref().is_some_and(|w| !w.is_over(now)) {
                return Advance::Blocked;
            }
            wake(host, &recipe, waited.as_ref());
            return Advance::Next;
        }
        RecipeStatus::Paused => return Advance::Blocked,
        RecipeStatus::Pending | RecipeStatus::Done | RecipeStatus::Failed => return Advance::Stopped,
    }

    let cur = recipe.current_step;
    if cur >= steps.len() {
        finish(host, &recipe, &steps, &vars);
        return Advance::Stopped;
    }

    let since = vars.get(SINCE_WAIT_VAR).and_then(|v| v.as_u64()).unwrap_or(0);
    let budget = vars.get(STEP_BUDGET_VAR).and_then(|v| v.as_u64()).unwrap_or(STEP_BUDGET);
    if since >= budget {
        stop_spinning(host, &recipe, &steps, &vars, budget);
        return Advance::Stopped;
    }
    set(host, recipe_id, SINCE_WAIT_VAR, json!(since + 1));

    let here = steps[cur].step.clone();
    tracing::info!(recipe_id, step = cur, kind = crate::recipe_view::kind_of(&here), "Executing recipe step");
    if matches!(here, RecipeStep::Branch { .. }) {
        return branch(host, &recipe, &steps, cur, &here, &vars, now);
    }

    let mut trail = Trail::read(&vars);
    let first_visit = trail.runs(cur) == 0;
    trail.ran(cur);
    let did = perform(host, recipe_id, &here, &vars, first_visit, now);
    match &did {
        Did::Went(_) if matches!(here, RecipeStep::JumpIf { .. }) => trail.went(cur, "continued"),
        Did::Jump(_) => trail.jumped(cur),
        _ => {}
    }
    save_trail(host, recipe_id, &trail);

    match did {
        Did::Went(result) => {
            host.with_conn(|c| {
                RecipeStore::complete_step(c, recipe_id, cur, &result);
                RecipeStore::update_status(c, recipe_id, &RecipeStatus::Running, cur + 1);
            });
            Advance::Next
        }
        Did::Jump(target) => {
            host.with_conn(|c| RecipeStore::complete_step(c, recipe_id, cur, "jumped"));
            go_to(host, recipe_id, cur, target);
            Advance::Next
        }
        Did::Sleep(until) => {
            host.with_conn(|c| RecipeStore::complete_step(c, recipe_id, cur, "waiting"));
            begin_wait(host, recipe_id, WaitRecord { step: cur, inner: Vec::new(), since: now, until: Some(until) }, cur + 1);
            Advance::Blocked
        }
        Did::Ask => {
            host.with_conn(|c| RecipeStore::complete_step(c, recipe_id, cur, "asked"));
            begin_wait(host, recipe_id, WaitRecord { step: cur, inner: Vec::new(), since: now, until: None }, cur + 1);
            Advance::Blocked
        }
        Did::Failed(err) => recover(host, &recipe, &steps, cur, &err, &on_error_of(&here), &vars),
    }
}

/// Step one recipe until it waits, stops, or has taken `max` steps. Returns the steps it took.
pub fn run<H: RecipeHost>(host: &mut H, recipe_id: &str, max: usize) -> usize {
    let mut taken = 0;
    while taken < max {
        taken += 1;
        if step(host, recipe_id) != Advance::Next {
            break;
        }
    }
    taken
}

/// What the clock should move now: every recipe running — a signal lost to a restart is not a
/// recipe lost — and every one whose wait is over.
pub fn due(conn: &Connection) -> Vec<String> {
    let mut ids = RecipeStore::get_resumable(conn);
    for id in RecipeStore::get_expired_waiting(conn) {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

/// Run what is due, a few steps each, for a host with no worker to signal
/// (`background::run_think_cycle`). Returns the steps taken.
pub fn tick(service: &mut CompanionService) -> usize {
    let due = due(&service.db.conn());
    let mut taken = 0;
    for id in due {
        taken += run(service, &id, MAX_STEPS_PER_TICK);
    }
    if taken > 0 {
        tracing::info!(taken, "Recipe executor tick complete");
    }
    taken
}

// ── What a step did ──

enum Did {
    /// Done, with what to record as its result.
    Went(String),
    /// Go to this step.
    Jump(usize),
    /// Wait for the clock, until this unix time.
    Sleep(f64),
    /// Wait for a person's answer.
    Ask,
    Failed(String),
}

/// Run one step that is not a Branch.
fn perform<H: RecipeHost>(host: &mut H, id: &str, step: &RecipeStep, vars: &Vars, first_visit: bool, now: f64) -> Did {
    match step {
        RecipeStep::Tool { tool_name, args, store_as, .. } => {
            let out = host.run_tool(tool_name, &resolve_vars_in_json(args, vars));
            if is_tool_error(&out) {
                return Did::Failed(out);
            }
            let kept = truncate(&out);
            set(host, id, store_as, json!(kept));
            Did::Went(kept)
        }
        RecipeStep::Think { prompt, store_as, fallback_template } => {
            let system = format!(
                "You are {}, a personal AI companion. Answer based ONLY on the provided data. Never invent \
                 prices, ratings, or availability. If data is missing, say so. Be concise.",
                host.persona()
            );
            let answer = host
                .generate(&system, &resolve_vars(prompt, vars), false)
                .map(|t| strip_think_tags(&t))
                .and_then(|t| if t.is_empty() { Err("the model gave an empty answer".to_string()) } else { Ok(t) });
            let text = match (answer, fallback_template) {
                (Ok(text), _) => text,
                (Err(_), Some(template)) => resolve_vars(template, vars),
                (Err(e), None) => return Did::Failed(format!("The model did not answer: {e}")),
            };
            let kept = truncate(&text);
            set(host, id, store_as, json!(kept));
            Did::Went(kept)
        }
        RecipeStep::JumpIf { condition, target_step } => {
            if condition.evaluate(vars) {
                Did::Jump(*target_step)
            } else {
                Did::Went("continued".into())
            }
        }
        RecipeStep::WaitFor { condition, timeout_secs } => match wakes_at(condition, *timeout_secs, now) {
            Some(until) => Did::Sleep(until),
            None => Did::Went("its time had come".into()),
        },
        RecipeStep::Notify { message } if message.starts_with(UNREADABLE) => {
            Did::Failed(format!("This step's definition could not be read: {}", &message[UNREADABLE.len()..]))
        }
        RecipeStep::Notify { message } => {
            let said = resolve_vars(message, vars);
            host.notify(id, &said);
            Did::Went(truncate(&said))
        }
        RecipeStep::AskUser { question, store_as, choices } => {
            // Answered before it was reached — a caller's variables — on its first visit only: a
            // loop back to a question asks it again.
            if first_visit {
                if let Some(given) = vars.get(store_as).filter(|v| is_answer(v)) {
                    return Did::Went(value_text(given));
                }
            }
            let mut text = resolve_vars(question, vars);
            for (i, c) in choices.as_deref().unwrap_or_default().iter().enumerate() {
                text.push_str(&format!("\n{}. {}", i + 1, resolve_vars(c, vars)));
            }
            host.notify(id, &text);
            Did::Ask
        }
        RecipeStep::ThinkCited { prompt, store_as, source_vars } => think_cited(host, id, prompt, store_as, source_vars, vars),
        RecipeStep::Validate { input_var, store_as } => validate(host, id, input_var, store_as, vars),
        RecipeStep::Render { input_var, store_as, format } => render(host, id, input_var, store_as, format, vars),
        RecipeStep::Format { template, store_as, .. } => {
            let kept = truncate(&resolve_vars(template, vars));
            set(host, id, store_as, json!(kept));
            Did::Went(kept)
        }
        RecipeStep::Filter { input_var, field, op, value, store_as } => filter(host, id, input_var, field, op, value, store_as, vars),
        RecipeStep::Sort { input_var, by_field, descending, store_as } => sort(host, id, input_var, by_field, *descending, store_as, vars),
        RecipeStep::Aggregate { input_var, op, field, store_as } => aggregate(host, id, input_var, op, field.as_deref(), store_as, vars),
        RecipeStep::Extract { input_var, pattern, store_as } => extract(host, id, input_var, pattern, store_as, vars),
        RecipeStep::Branch { .. } => Did::Failed("a Branch is run by its arms, not as one step".into()),
    }
}

// ── Branch: its arms, one step at a time ──

/// One Branch the recipe is inside: where it sits in its list, the arm it took, and the index in
/// that arm of the step to run next. `_branch` keeps the stack, outermost first, so a question or
/// a timer inside an arm waits like any other and a restart comes back to the same place.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Frame {
    step: usize,
    arm: String,
    next: usize,
}

fn branch<H: RecipeHost>(host: &mut H, recipe: &Recipe, steps: &[StoredStep], cur: usize, top: &RecipeStep, vars: &Vars, now: f64) -> Advance {
    let id = recipe.id.as_str();
    let mut trail = Trail::read(vars);
    let mut frames: Vec<Frame> =
        vars.get(BRANCH_VAR).and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default();
    if frames.first().map(|f| f.step) != Some(cur) {
        let arm = choose(top, vars);
        trail.enter_branch(cur, arm);
        frames = vec![Frame { step: cur, arm: arm.to_string(), next: 0 }];
    }
    let arm = frames[0].arm.clone();

    let Some(sub) = descend(top, &mut frames, vars, &mut trail, cur) else {
        save_trail(host, id, &trail);
        close_branch(host, id, cur, &arm);
        return Advance::Next;
    };
    let depth = frames.len();
    let first_visit = trail.runs(cur) <= 1;
    let did = perform(host, id, &sub, vars, first_visit, now);

    match did {
        Did::Went(_) => {
            if let Some(f) = frames.last_mut() {
                f.next += 1;
            }
            if depth == 1 {
                trail.sub(cur, "done");
            }
            settle_frames(host, id, cur, top, &mut frames, &mut trail, &arm)
        }
        Did::Jump(target) => {
            trail.leave(cur, target);
            save_trail(host, id, &trail);
            host.with_conn(|c| {
                RecipeStore::delete_var(c, id, BRANCH_VAR);
                RecipeStore::complete_step(c, id, cur, &arm);
            });
            go_to(host, id, cur, target);
            Advance::Next
        }
        Did::Sleep(until) => wait_in_arm(host, id, cur, &mut frames, &mut trail, now, Some(until)),
        Did::Ask => wait_in_arm(host, id, cur, &mut frames, &mut trail, now, None),
        Did::Failed(err) => match on_error_of(&sub) {
            ErrorAction::Skip => {
                if let Some(f) = frames.last_mut() {
                    f.next += 1;
                }
                if depth == 1 {
                    trail.sub(cur, "skipped");
                }
                settle_frames(host, id, cur, top, &mut frames, &mut trail, &arm)
            }
            ErrorAction::Retry { max } => {
                let key = format!(
                    "_retry_{cur}_{}",
                    frames.iter().map(|f| f.next.to_string()).collect::<Vec<_>>().join("_")
                );
                let tried = vars.get(&key).and_then(|v| v.as_u64()).unwrap_or(0);
                if tried < max as u64 {
                    set(host, id, &key, json!(tried + 1));
                    set(host, id, BRANCH_VAR, json!(frames));
                    save_trail(host, id, &trail);
                    return Advance::Next;
                }
                if depth == 1 {
                    trail.sub(cur, "failed");
                }
                save_trail(host, id, &trail);
                host.with_conn(|c| RecipeStore::delete_var(c, id, BRANCH_VAR));
                fail(host, recipe, cur, &err, &format!("Failed after {max} retries: {err}"))
            }
            ErrorAction::JumpTo { step: target } => {
                if depth == 1 {
                    trail.sub(cur, "failed");
                }
                save_trail(host, id, &trail);
                host.with_conn(|c| {
                    RecipeStore::delete_var(c, id, BRANCH_VAR);
                    RecipeStore::fail_step(c, id, cur, &err);
                    RecipeStore::update_status(c, id, &RecipeStatus::Running, target);
                });
                Advance::Next
            }
            other => {
                if depth == 1 {
                    trail.sub(cur, "failed");
                }
                save_trail(host, id, &trail);
                host.with_conn(|c| RecipeStore::delete_var(c, id, BRANCH_VAR));
                recover(host, recipe, steps, cur, &err, &other, vars)
            }
        },
    }
}

/// A step inside an arm waits — a timer or a question. The frames move past it, as the pointer
/// moves past a wait at the top, and `_wait` says where it is: the Branch, and the arms and
/// indexes down to the step.
fn wait_in_arm<H: RecipeHost>(
    host: &mut H,
    id: &str,
    cur: usize,
    frames: &mut [Frame],
    trail: &mut Trail,
    now: f64,
    until: Option<f64>,
) -> Advance {
    let at = frames.last().map(|f| f.next).unwrap_or(0);
    let inner: Vec<(String, usize)> = frames
        .iter()
        .enumerate()
        .map(|(d, f)| (f.arm.clone(), frames.get(d + 1).map_or(at, |inner| inner.step)))
        .collect();
    if let Some(f) = frames.last_mut() {
        f.next += 1;
    }
    if frames.len() == 1 {
        trail.sub(cur, "waiting");
    }
    save_trail(host, id, trail);
    set(host, id, BRANCH_VAR, json!(frames));
    begin_wait(host, id, WaitRecord { step: cur, inner, since: now, until }, cur);
    Advance::Blocked
}

/// After a step in an arm: close the arms that are done, and the Branch with them when it is.
fn settle_frames<H: RecipeHost>(
    host: &mut H,
    id: &str,
    cur: usize,
    top: &RecipeStep,
    frames: &mut Vec<Frame>,
    trail: &mut Trail,
    arm: &str,
) -> Advance {
    let closed = close_finished(top, frames, trail, cur);
    save_trail(host, id, &trail);
    if closed {
        close_branch(host, id, cur, arm);
    } else {
        set(host, id, BRANCH_VAR, json!(frames));
    }
    Advance::Next
}

/// The step to run next inside the Branch: arms that are done are closed, and a Branch reached
/// inside an arm is entered. None when the whole Branch is done.
fn descend(top: &RecipeStep, frames: &mut Vec<Frame>, vars: &Vars, trail: &mut Trail, cur: usize) -> Option<RecipeStep> {
    loop {
        if close_finished(top, frames, trail, cur) {
            return None;
        }
        let at = frames.last()?.next;
        let sub = arm_list(top, frames).get(at)?.clone();
        if matches!(sub, RecipeStep::Branch { .. }) {
            frames.push(Frame { step: at, arm: choose(&sub, vars).to_string(), next: 0 });
            continue;
        }
        return Some(sub);
    }
}

/// Pop every arm whose steps have all run. True when the outermost one is done too.
fn close_finished(top: &RecipeStep, frames: &mut Vec<Frame>, trail: &mut Trail, cur: usize) -> bool {
    loop {
        let Some(last) = frames.last() else { return true };
        if last.next < arm_list(top, frames).len() {
            return false;
        }
        frames.pop();
        match frames.last_mut() {
            None => return true,
            Some(parent) => {
                parent.next += 1;
                if frames.len() == 1 {
                    // A Branch inside the arm is one of the arm's steps, and it is done.
                    trail.sub(cur, "done");
                }
            }
        }
    }
}

/// The steps of the arm the innermost frame is in.
fn arm_list<'a>(top: &'a RecipeStep, frames: &[Frame]) -> &'a [RecipeStep] {
    let mut at: &'a RecipeStep = top;
    let mut list: &'a [RecipeStep] = &[];
    for (depth, frame) in frames.iter().enumerate() {
        if depth > 0 {
            match list.get(frame.step) {
                Some(step) => at = step,
                None => return &[],
            }
        }
        list = match at {
            RecipeStep::Branch { then_steps, else_steps, .. } => {
                if frame.arm == "then" {
                    then_steps
                } else {
                    else_steps
                }
            }
            _ => &[],
        };
    }
    list
}

/// Which arm a Branch takes: `then` when its condition names a variable that is set and not
/// empty, false or 0.
fn choose(step: &RecipeStep, vars: &Vars) -> &'static str {
    match step {
        RecipeStep::Branch { condition, .. } if vars.get(condition).is_some_and(truthy) => "then",
        _ => "else",
    }
}

fn truthy(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        serde_json::Value::String(s) => !s.is_empty() && s != "false" && s != "0",
        serde_json::Value::Array(a) => !a.is_empty(),
        serde_json::Value::Object(o) => !o.is_empty(),
    }
}

fn close_branch<H: RecipeHost>(host: &mut H, id: &str, cur: usize, arm: &str) {
    host.with_conn(|c| {
        RecipeStore::delete_var(c, id, BRANCH_VAR);
        RecipeStore::complete_step(c, id, cur, arm);
        RecipeStore::update_status(c, id, &RecipeStatus::Running, cur + 1);
    });
}

// ── Moving on, waiting, and stopping ──

/// Move the pointer to `target`. A jump back is a loop: the steps it goes round again are marked
/// to come, so the row shows this round, not the last one's ticks. `_trail` keeps the count.
fn go_to<H: RecipeHost>(host: &mut H, id: &str, from: usize, target: usize) {
    host.with_conn(|c| {
        if target <= from {
            RecipeStore::reset_steps(c, id, target, from);
        }
        RecipeStore::update_status(c, id, &RecipeStatus::Running, target);
    });
}

fn begin_wait<H: RecipeHost>(host: &mut H, id: &str, record: WaitRecord, pointer: usize) {
    host.with_conn(|c| {
        RecipeStore::set_var(c, id, WAIT_VAR, &json!(record));
        RecipeStore::set_var(c, id, SINCE_WAIT_VAR, &json!(0));
        RecipeStore::update_status(c, id, &RecipeStatus::Waiting, pointer);
    });
}

/// A timer is over — or a recipe was left `waiting` with no wait behind it: running again, from
/// where it stands.
fn wake<H: RecipeHost>(host: &mut H, recipe: &Recipe, waited: Option<&Waited>) {
    let id = recipe.id.as_str();
    if let Some(w) = waited.filter(|w| matches!(w.on, Some(RecipeStep::WaitFor { .. }))) {
        if w.inner.is_empty() {
            let note = match w.until {
                Some(until) => format!("waited until {}", clock_text(until)),
                None => "waited".to_string(),
            };
            host.with_conn(|c| RecipeStore::complete_step(c, id, w.step, &note));
        } else {
            host.with_conn(|c| Trail::note(c, id, w.step, "waiting", "waited"));
        }
    }
    host.with_conn(|c| {
        RecipeStore::delete_var(c, id, WAIT_VAR);
        RecipeStore::update_status(c, id, &RecipeStatus::Running, recipe.current_step);
    });
    tracing::info!(recipe_id = id, "Recipe resumed: its wait is over");
}

/// Past the last step: done, and the person told, with what it came to.
fn finish<H: RecipeHost>(host: &mut H, recipe: &Recipe, steps: &[StoredStep], vars: &Vars) {
    let id = recipe.id.as_str();
    host.with_conn(|c| RecipeStore::update_status(c, id, &RecipeStatus::Done, recipe.current_step));
    let last = steps.last().and_then(|s| outcome_of(&s.step, vars)).unwrap_or_default();
    let text = if last.is_empty() {
        format!("Recipe completed: {}", recipe.name)
    } else {
        format!("Recipe completed: {}\n\nResult: {}", recipe.name, last)
    };
    host.notify(id, &text);
    tracing::info!(recipe_id = id, name = %recipe.name, steps = steps.len(), "Recipe completed");
}

/// What a last step came to, for the completion message.
fn outcome_of(step: &RecipeStep, vars: &Vars) -> Option<String> {
    match step {
        RecipeStep::Notify { message } => Some(resolve_vars(message, vars)),
        other => {
            let key = crate::recipe_view::store_as(other)?;
            let value = vars.get(key)?.as_str()?;
            Some(if value.len() > 500 { format!("{}...", &value[..value.floor_char_boundary(500)]) } else { value.to_string() })
        }
    }
}

/// The spin guard: too many steps without a pause.
fn stop_spinning<H: RecipeHost>(host: &mut H, recipe: &Recipe, steps: &[StoredStep], vars: &Vars, budget: u64) {
    let cur = recipe.current_step;
    let label = steps.get(cur).map(|s| crate::recipe_view::stage_label(&s.step)).unwrap_or_default();
    let looped = Trail::read(vars)
        .most_looped()
        .map(|(i, n)| format!("; step {} had gone back {n} times", i + 1))
        .unwrap_or_default();
    let why = format!(
        "{budget} steps ran without a pause (a timer or a question), and it was stopped before step {} ({label}){looped}. \
         A loop that never waits would run forever: give it a WaitFor, or set `{STEP_BUDGET_VAR}` if it needs more steps.",
        cur + 1
    );
    host.with_conn(|c| RecipeStore::set_error(c, &recipe.id, &format!("Stopped: {why}")));
    host.notify(&recipe.id, &format!("Recipe '{}' stopped: {why}", recipe.name));
    tracing::warn!(recipe_id = %recipe.id, budget, "Recipe stopped by its step budget");
}

/// A step failed: what its `on_error` says to do.
fn recover<H: RecipeHost>(
    host: &mut H,
    recipe: &Recipe,
    steps: &[StoredStep],
    cur: usize,
    err: &str,
    on_error: &ErrorAction,
    vars: &Vars,
) -> Advance {
    let id = recipe.id.as_str();
    match on_error {
        ErrorAction::Fail => fail(host, recipe, cur, err, err),
        ErrorAction::Skip => {
            host.with_conn(|c| {
                RecipeStore::skip_step(c, id, cur);
                RecipeStore::update_status(c, id, &RecipeStatus::Running, cur + 1);
            });
            Advance::Next
        }
        ErrorAction::Retry { max } => {
            let key = format!("_retry_{cur}");
            let tried = vars.get(&key).and_then(|v| v.as_u64()).unwrap_or(0);
            if tried < *max as u64 {
                set(host, id, &key, json!(tried + 1));
                tracing::info!(recipe_id = id, step = cur, retry = tried + 1, max = *max, "Recipe step retry");
                Advance::Next
            } else {
                fail(host, recipe, cur, err, &format!("Failed after {max} retries: {err}"))
            }
        }
        ErrorAction::JumpTo { step: target } => {
            host.with_conn(|c| {
                RecipeStore::fail_step(c, id, cur, err);
                RecipeStore::update_status(c, id, &RecipeStatus::Running, *target);
            });
            Advance::Next
        }
        ErrorAction::Replan => match replan(host, recipe, steps, cur, err) {
            Ok(n) => {
                host.notify(
                    id,
                    &format!("Recipe '{}' step {} failed ({}). Replanned with {} new steps.", recipe.name, cur + 1, err, n),
                );
                Advance::Next
            }
            Err(why) => fail(host, recipe, cur, err, &format!("Step {} failed and could not be replanned ({why}): {err}", cur + 1)),
        },
    }
}

fn fail<H: RecipeHost>(host: &mut H, recipe: &Recipe, cur: usize, step_error: &str, recipe_error: &str) -> Advance {
    host.with_conn(|c| {
        RecipeStore::fail_step(c, &recipe.id, cur, step_error);
        RecipeStore::set_error(c, &recipe.id, recipe_error);
    });
    host.notify(&recipe.id, &format!("Recipe '{}' failed at step {}: {}", recipe.name, cur + 1, recipe_error));
    tracing::warn!(recipe_id = %recipe.id, step = cur, error = %recipe_error, "Recipe failed");
    Advance::Stopped
}

/// Ask the model for steps to replace the ones after a failed step. The failed step keeps its
/// record; the new steps start after it. Returns how many there are.
fn replan<H: RecipeHost>(host: &mut H, recipe: &Recipe, steps: &[StoredStep], cur: usize, err: &str) -> Result<usize, String> {
    let done: Vec<String> = steps
        .iter()
        .take(cur)
        .map(|s| format!("Step {}: {} → {}", s.step_index + 1, crate::recipe_view::kind_of(&s.step), s.result.as_deref().unwrap_or("(no result)")))
        .collect();
    let failed = steps.get(cur).map(|s| serde_json::to_string(&s.step).unwrap_or_default()).unwrap_or_default();
    let remaining: Vec<String> = steps
        .iter()
        .skip(cur + 1)
        .map(|s| format!("Step {}: {}", s.step_index + 1, serde_json::to_string(&s.step).unwrap_or_default()))
        .collect();
    let prompt = format!(
        "Recipe '{}' failed at step {}.\nError: {}\n\nCompleted steps:\n{}\n\nFailed step: {}\n\n\
         Remaining planned steps:\n{}\n\nRecipe goal: {}\n\n\
         Analyze the failure and provide replacement steps as a JSON array. Each step must be one of:\n\
         - {{\"type\":\"Tool\",\"tool_name\":\"...\",\"args\":{{...}},\"store_as\":\"...\",\"on_error\":{{\"action\":\"Replan\"}}}}\n\
         - {{\"type\":\"Think\",\"prompt\":\"...\",\"store_as\":\"...\"}}\n\
         - {{\"type\":\"Notify\",\"message\":\"...\"}}\n\n\
         Reply with ONLY the JSON array of replacement steps. Fix the root cause, don't just retry the same thing. \
         If the failure is unrecoverable, reply with [].",
        recipe.name,
        cur + 1,
        err,
        if done.is_empty() { "(none)".to_string() } else { done.join("\n") },
        failed,
        if remaining.is_empty() { "(none)".to_string() } else { remaining.join("\n") },
        recipe.description,
    );
    let reply = host.generate("You are a recipe debugger. Output ONLY a JSON array of replacement steps.", &prompt, true)?;
    let new_steps: Vec<RecipeStep> = serde_json::from_str(&extract_json_array(&strip_think_tags(&reply)))
        .map_err(|e| format!("its steps could not be read: {e}"))?;
    if new_steps.is_empty() {
        return Err("the model offered no steps".into());
    }
    let tool = match steps.get(cur).map(|s| &s.step) {
        Some(RecipeStep::Tool { tool_name, .. }) => tool_name.clone(),
        Some(other) => crate::recipe_view::kind_of(other).to_string(),
        None => String::new(),
    };
    let id = recipe.id.as_str();
    host.with_conn(|c| {
        RecipeStore::fail_step(c, id, cur, err);
        RecipeStore::record_failure_learning(c, id, cur, &tool, err, &format!("Replanned with {} new steps", new_steps.len()));
        RecipeStore::replace_remaining_steps(c, id, cur + 1, &new_steps);
        RecipeStore::update_status(c, id, &RecipeStatus::Running, cur + 1);
    });
    tracing::info!(recipe_id = id, step = cur, new_steps = new_steps.len(), "Recipe replanned after failure");
    Ok(new_steps.len())
}

fn on_error_of(step: &RecipeStep) -> ErrorAction {
    match step {
        RecipeStep::Tool { on_error, .. } => on_error.clone(),
        _ => ErrorAction::Fail,
    }
}

/// How the tools say they failed. A tool returns a string either way.
fn is_tool_error(out: &str) -> bool {
    out.starts_with("Unknown tool:") || out.starts_with("Permission denied:") || out.starts_with("Failed") || out.starts_with("Error")
}

fn is_answer(v: &serde_json::Value) -> bool {
    !v.is_null() && v.as_str().map_or(true, |s| !s.trim().is_empty())
}

fn value_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn set<H: RecipeHost>(host: &H, id: &str, key: &str, value: serde_json::Value) {
    host.with_conn(|c| RecipeStore::set_var(c, id, key, &value));
}

fn save_trail<H: RecipeHost>(host: &H, id: &str, trail: &Trail) {
    host.with_conn(|c| trail.save_to(c, id));
}

// ── The shell's host ──

impl RecipeHost for CompanionService {
    fn with_conn<R>(&self, f: impl FnOnce(&Connection) -> R) -> R {
        f(&self.db.conn())
    }

    fn run_tool(&mut self, name: &str, args: &serde_json::Value) -> String {
        self.execute_tool_direct(name, args)
    }

    fn generate(&mut self, system: &str, prompt: &str, precise: bool) -> Result<String, String> {
        let messages = vec![ChatMessage::system(system), ChatMessage::user(prompt)];
        let config = GenerationConfig {
            max_tokens: self.config.llm.max_tokens,
            temperature: if precise { 0.2 } else { self.config.llm.temperature },
            ..Default::default()
        };
        self.llm.chat(&messages, &config, None).map(|r| r.text).map_err(|e| e.to_string())
    }

    fn notify(&mut self, recipe_id: &str, text: &str) {
        self.set_proactive_message(crate::types::ProactiveMessage {
            text: text.to_string(),
            urge_ids: vec![format!("recipe:{recipe_id}")],
            generated_at: now_ts(),
        });
    }

    fn persona(&self) -> String {
        self.config.personality.name.clone()
    }
}

// ── ThinkCited: LLM synthesis with per-claim citations ──

fn think_cited<H: RecipeHost>(host: &mut H, id: &str, prompt: &str, store_as: &str, source_vars: &[String], vars: &Vars) -> Did {
    use crate::recipe::{CitedClaim, CitedOutput, EvidenceStatus};

    let mut source_context = String::new();
    for (i, name) in source_vars.iter().enumerate() {
        let content = vars.get(name).and_then(|v| v.as_str()).unwrap_or("(no data)");
        source_context.push_str(&format!("\n[SOURCE:{}] (from step '{}'): {}\n", i + 1, name, content));
    }
    let instruction = format!(
        "You have the following sources:\n{}\n\n{}\n\n\
         IMPORTANT: Output JSON with this exact structure:\n\
         {{\n  \"title\": \"<section title>\",\n  \"claims\": [\n    {{\"text\": \"<claim>\", \"sources\": [\"<source_var_name>\", ...]}},\n    ...\n  ]\n}}\n\
         Each claim MUST reference which source(s) support it by variable name.\n\
         If a fact has no source, do NOT include it.\nOutput ONLY the JSON, no other text.",
        source_context,
        resolve_vars(prompt, vars)
    );
    let system = format!(
        "You are {}. You produce citation-backed analysis. Every claim must reference its source. Never invent facts.",
        host.persona()
    );
    match host.generate(&system, &instruction, true) {
        Ok(reply) => {
            let text = strip_think_tags(&reply);
            let output = match serde_json::from_str::<CitedOutput>(&extract_json_object(&text)) {
                Ok(mut output) => {
                    for claim in &mut output.claims {
                        claim.confidence = match claim.sources.len() {
                            0 => "uncited",
                            1 => "low",
                            2 => "medium",
                            _ => "high",
                        }
                        .to_string();
                    }
                    output.evidence_status = compute_evidence_status(&output.claims, source_vars);
                    output
                }
                Err(_) => CitedOutput {
                    title: "Analysis".to_string(),
                    claims: vec![CitedClaim { text: truncate(&text), sources: vec![], confidence: "uncited".to_string() }],
                    evidence_status: EvidenceStatus::Insufficient,
                },
            };
            set(host, id, store_as, serde_json::to_value(&output).unwrap_or_default());
            Did::Went("done".into())
        }
        Err(e) => Did::Failed(format!("The model did not answer (ThinkCited): {e}")),
    }
}

fn compute_evidence_status(claims: &[crate::recipe::CitedClaim], source_vars: &[String]) -> crate::recipe::EvidenceStatus {
    use crate::recipe::EvidenceStatus;
    if claims.is_empty() {
        return EvidenceStatus::Insufficient;
    }
    let cited = claims.iter().filter(|c| !c.sources.is_empty()).count();
    if cited == 0 {
        return EvidenceStatus::Insufficient;
    }
    let unique: std::collections::HashSet<&str> = claims.iter().flat_map(|c| c.sources.iter().map(|s| s.as_str())).collect();
    let coverage = if source_vars.is_empty() { 0.0 } else { unique.len() as f64 / source_vars.len() as f64 };
    let cite_ratio = cited as f64 / claims.len() as f64;
    if cite_ratio >= 0.8 && coverage >= 0.6 {
        EvidenceStatus::Strong
    } else if cite_ratio >= 0.5 {
        EvidenceStatus::Moderate
    } else {
        EvidenceStatus::Thin
    }
}

// ── Validate: deterministic claim verification ──

fn validate<H: RecipeHost>(host: &mut H, id: &str, input_var: &str, store_as: &str, vars: &Vars) -> Did {
    use crate::recipe::{CitedClaim, CitedOutput, EvidenceStatus};

    let Some(input) = vars.get(input_var).cloned() else {
        return Did::Failed(format!("Validate: variable '{input_var}' not found"));
    };
    let mut output: CitedOutput = serde_json::from_value(input.clone()).unwrap_or_else(|_| CitedOutput {
        title: "Validation".to_string(),
        claims: vec![CitedClaim { text: input.as_str().unwrap_or("").to_string(), sources: vec![], confidence: "uncited".to_string() }],
        evidence_status: EvidenceStatus::Insufficient,
    });
    let before = output.claims.len();
    output.claims.retain(|c| !c.sources.is_empty());
    let cited = output.claims.len();
    output.evidence_status = match cited {
        0 => EvidenceStatus::Insufficient,
        1 => EvidenceStatus::Thin,
        2 => EvidenceStatus::Moderate,
        _ => EvidenceStatus::Strong,
    };
    let report = json!({
        "total_claims": before,
        "cited_claims": cited,
        "stripped_uncited": before - cited,
        "evidence_status": format!("{:?}", output.evidence_status),
    });
    set(host, id, &format!("{store_as}_report"), report);
    set(host, id, store_as, serde_json::to_value(&output).unwrap_or_default());
    Did::Went("done".into())
}

// ── Render: format validated data for presentation ──

fn render<H: RecipeHost>(host: &mut H, id: &str, input_var: &str, store_as: &str, format: &crate::recipe::RenderFormat, vars: &Vars) -> Did {
    let Some(input) = vars.get(input_var).cloned() else {
        return Did::Failed(format!("Render: variable '{input_var}' not found"));
    };
    let rendered = match serde_json::from_value::<crate::recipe::CitedOutput>(input.clone()) {
        Ok(output) => render_cited_output(&output, format),
        Err(_) => input.as_str().unwrap_or("(no data)").to_string(),
    };
    set(host, id, store_as, json!(rendered));
    Did::Went("done".into())
}

fn render_cited_output(output: &crate::recipe::CitedOutput, format: &crate::recipe::RenderFormat) -> String {
    use crate::recipe::{EvidenceStatus, RenderFormat};

    if output.claims.is_empty() {
        return format!("**{}**\n\nNo verified information available.", output.title);
    }
    let evidence = match &output.evidence_status {
        EvidenceStatus::Strong => "Well-supported",
        EvidenceStatus::Moderate => "Moderately supported",
        EvidenceStatus::Thin => "Limited evidence",
        EvidenceStatus::Conflicting => "Conflicting sources",
        EvidenceStatus::Insufficient => "Insufficient evidence",
    };
    let mut out = format!("**{}** _({})_\n\n", output.title, evidence);
    match format {
        RenderFormat::Summary => {
            for claim in &output.claims {
                let marker = match claim.confidence.as_str() {
                    "high" | "medium" => "",
                    "low" => " _(limited source)_",
                    _ => " _(unverified)_",
                };
                out.push_str(&format!("- {}{}\n", claim.text, marker));
            }
        }
        RenderFormat::Table => {
            out.push_str("| Finding | Confidence | Sources |\n|---|---|---|\n");
            for claim in &output.claims {
                out.push_str(&format!("| {} | {} | {} |\n", claim.text, claim.confidence, claim.sources.join(", ")));
            }
        }
        RenderFormat::Comparison => {
            for (i, claim) in output.claims.iter().enumerate() {
                let sources = if claim.sources.is_empty() { "none".to_string() } else { claim.sources.join(", ") };
                out.push_str(&format!("**{}. {}**\n  Sources: {}\n  Confidence: {}\n\n", i + 1, claim.text, sources, claim.confidence));
            }
        }
        RenderFormat::Cards => {
            for (i, claim) in output.claims.iter().enumerate() {
                out.push_str(&format!(
                    "┌─ {} ─────────────────────\n│ {}\n│ Sources: {} | Confidence: {}\n└─────────────────────────────\n\n",
                    i + 1,
                    claim.text,
                    claim.sources.join(", "),
                    claim.confidence
                ));
            }
        }
    }
    out
}

// ── The data steps: Filter, Sort, Aggregate, Extract ──

#[allow(clippy::too_many_arguments)]
fn filter<H: RecipeHost>(host: &mut H, id: &str, input_var: &str, field: &str, op: &FilterOp, value: &str, store_as: &str, vars: &Vars) -> Did {
    let Some(input) = vars.get(input_var) else {
        return Did::Failed(format!("Filter: variable '{input_var}' not found"));
    };
    let Some(rows) = parse_json_array(input) else {
        return Did::Failed(format!("Filter: '{input_var}' is not a JSON array"));
    };
    let kept: Vec<serde_json::Value> = rows
        .into_iter()
        .filter(|row| {
            let v = row.get(field);
            match op {
                FilterOp::Equals => v.is_some_and(|v| value_matches_str(v, value)),
                FilterOp::NotEquals => v.map_or(true, |v| !value_matches_str(v, value)),
                FilterOp::Contains => v.and_then(|v| v.as_str()).is_some_and(|s| s.contains(value)),
                FilterOp::GreaterThan => compare_field_value(v, value) == Some(std::cmp::Ordering::Greater),
                FilterOp::LessThan => compare_field_value(v, value) == Some(std::cmp::Ordering::Less),
            }
        })
        .collect();
    set(host, id, store_as, json!(truncate(&serde_json::to_string(&kept).unwrap_or_else(|_| "[]".into()))));
    Did::Went("done".into())
}

fn sort<H: RecipeHost>(host: &mut H, id: &str, input_var: &str, by_field: &str, descending: bool, store_as: &str, vars: &Vars) -> Did {
    let Some(input) = vars.get(input_var) else {
        return Did::Failed(format!("Sort: variable '{input_var}' not found"));
    };
    let Some(mut rows) = parse_json_array(input) else {
        return Did::Failed(format!("Sort: '{input_var}' is not a JSON array"));
    };
    rows.sort_by(|a, b| {
        let ord = compare_json_values(a.get(by_field), b.get(by_field));
        if descending { ord.reverse() } else { ord }
    });
    set(host, id, store_as, json!(truncate(&serde_json::to_string(&rows).unwrap_or_else(|_| "[]".into()))));
    Did::Went("done".into())
}

fn aggregate<H: RecipeHost>(host: &mut H, id: &str, input_var: &str, op: &AggregateOp, field: Option<&str>, store_as: &str, vars: &Vars) -> Did {
    let Some(input) = vars.get(input_var) else {
        return Did::Failed(format!("Aggregate: variable '{input_var}' not found"));
    };
    let Some(rows) = parse_json_array(input) else {
        return Did::Failed(format!("Aggregate: '{input_var}' is not a JSON array"));
    };
    let result = match op {
        AggregateOp::Count => rows.len().to_string(),
        _ => {
            let values: Vec<f64> = rows
                .iter()
                .filter_map(|row| match field {
                    Some(f) => row.get(f).and_then(json_to_f64),
                    None => json_to_f64(row),
                })
                .collect();
            if values.is_empty() {
                "0".to_string()
            } else {
                match op {
                    AggregateOp::Sum => values.iter().sum::<f64>().to_string(),
                    AggregateOp::Min => values.iter().cloned().fold(f64::INFINITY, f64::min).to_string(),
                    AggregateOp::Max => values.iter().cloned().fold(f64::NEG_INFINITY, f64::max).to_string(),
                    AggregateOp::Avg => (values.iter().sum::<f64>() / values.len() as f64).to_string(),
                    AggregateOp::Count => unreachable!("counted above"),
                }
            }
        }
    };
    set(host, id, store_as, json!(result));
    Did::Went("done".into())
}

fn extract<H: RecipeHost>(host: &mut H, id: &str, input_var: &str, pattern: &str, store_as: &str, vars: &Vars) -> Did {
    let Some(input) = vars.get(input_var).cloned() else {
        return Did::Failed(format!("Extract: variable '{input_var}' not found"));
    };
    if pattern.starts_with('/') {
        let source = pattern.trim_start_matches('/').trim_end_matches('/');
        return match regex::Regex::new(source) {
            Ok(re) => {
                let text = value_text(&input);
                let found = re.find(&text).map(|m| m.as_str().to_string()).unwrap_or_default();
                set(host, id, store_as, json!(found));
                Did::Went("done".into())
            }
            Err(e) => Did::Failed(format!("Extract: invalid regex '{source}': {e}")),
        };
    }
    // Dot-notation key path ("data.name", "items.0.title").
    let parsed = match &input {
        serde_json::Value::String(s) => serde_json::from_str::<serde_json::Value>(s).unwrap_or(input.clone()),
        other => other.clone(),
    };
    let mut at = &parsed;
    for key in pattern.split('.') {
        let next = match key.parse::<usize>() {
            Ok(i) => at.get(i),
            Err(_) => at.get(key),
        };
        match next {
            Some(v) => at = v,
            None => {
                set(host, id, store_as, json!(""));
                return Did::Went("done".into());
            }
        }
    }
    set(host, id, store_as, json!(truncate(&value_text(at))));
    Did::Went("done".into())
}

fn parse_json_array(val: &serde_json::Value) -> Option<Vec<serde_json::Value>> {
    match val {
        serde_json::Value::Array(a) => Some(a.clone()),
        serde_json::Value::String(s) => serde_json::from_str::<Vec<serde_json::Value>>(s).ok(),
        _ => None,
    }
}

fn value_matches_str(v: &serde_json::Value, s: &str) -> bool {
    match v {
        serde_json::Value::String(vs) => vs == s,
        serde_json::Value::Number(n) => n.to_string() == s,
        serde_json::Value::Bool(b) => b.to_string() == s,
        serde_json::Value::Null => s.is_empty() || s == "null",
        _ => false,
    }
}

fn compare_field_value(field: Option<&serde_json::Value>, threshold: &str) -> Option<std::cmp::Ordering> {
    json_to_f64(field?)?.partial_cmp(&threshold.parse::<f64>().ok()?)
}

fn compare_json_values(a: Option<&serde_json::Value>, b: Option<&serde_json::Value>) -> std::cmp::Ordering {
    match (a, b) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (Some(va), Some(vb)) => {
            if let (Some(na), Some(nb)) = (json_to_f64(va), json_to_f64(vb)) {
                return na.partial_cmp(&nb).unwrap_or(std::cmp::Ordering::Equal);
            }
            value_text(va).cmp(&value_text(vb))
        }
    }
}

fn json_to_f64(v: &serde_json::Value) -> Option<f64> {
    match v {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

// ── Helpers ──

fn truncate(s: &str) -> String {
    if s.len() > MAX_RESULT_SIZE {
        format!("{}...(truncated)", &s[..s.floor_char_boundary(MAX_RESULT_SIZE)])
    } else {
        s.to_string()
    }
}

fn strip_think_tags(text: &str) -> String {
    let mut result = String::new();
    let mut remaining = text;
    while let Some(start) = remaining.find("<think>") {
        result.push_str(&remaining[..start]);
        let after = &remaining[start + "<think>".len()..];
        match after.find("</think>") {
            Some(end) => remaining = &after[end + "</think>".len()..],
            None => {
                remaining = "";
                break;
            }
        }
    }
    result.push_str(remaining);
    result.trim().to_string()
}

fn extract_json_array(text: &str) -> String {
    match (text.find('['), text.rfind(']')) {
        (Some(start), Some(end)) if end > start => text[start..=end].to_string(),
        _ => "[]".to_string(),
    }
}

fn extract_json_object(text: &str) -> String {
    match (text.find('{'), text.rfind('}')) {
        (Some(start), Some(end)) if end > start => text[start..=end].to_string(),
        _ => "{}".to_string(),
    }
}

fn now_ts() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs_f64()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipe::{Condition, WaitCondition};
    use crate::recipe_view::{self, RecipeOp};
    use std::collections::VecDeque;

    /// 2026-09-23 08:00:00 UTC.
    const EIGHT_AM: f64 = 1_790_150_400.0;

    /// A desk to run recipes on: the store in memory, tools and a model that answer from a
    /// script, what the recipe told the person, and a clock that moves when the test moves it.
    struct Desk {
        conn: Connection,
        clock: f64,
        tools: HashMap<String, VecDeque<String>>,
        called: Vec<String>,
        model: VecDeque<Result<String, String>>,
        said: Vec<String>,
    }

    impl Desk {
        fn new() -> Self {
            let conn = Connection::open_in_memory().expect("in-memory sqlite");
            RecipeStore::ensure_tables(&conn);
            Self { conn, clock: EIGHT_AM, tools: HashMap::new(), called: Vec::new(), model: VecDeque::new(), said: Vec::new() }
        }

        /// A tool answers these in turn, and the last one from then on.
        fn tool_says(&mut self, name: &str, replies: &[&str]) {
            self.tools.insert(name.into(), replies.iter().map(|s| s.to_string()).collect());
        }

        /// A recipe of these steps, started.
        fn start(&self, steps: &[RecipeStep]) -> String {
            let id = RecipeStore::create(&self.conn, "Digest", "", steps, None);
            RecipeStore::update_status(&self.conn, &id, &RecipeStatus::Running, 0);
            id
        }

        fn status(&self, id: &str) -> (RecipeStatus, usize) {
            let r = RecipeStore::get(&self.conn, id).expect("the recipe");
            (r.status, r.current_step)
        }

        fn view(&self, id: &str) -> recipe_view::RecipeView {
            recipe_view::list(&self.conn).into_iter().find(|v| v.id == id).expect("the recipe's view")
        }

        fn var(&self, id: &str, key: &str) -> Option<serde_json::Value> {
            RecipeStore::get_vars(&self.conn, id).get(key).cloned()
        }
    }

    impl RecipeHost for Desk {
        fn with_conn<R>(&self, f: impl FnOnce(&Connection) -> R) -> R {
            f(&self.conn)
        }
        fn run_tool(&mut self, name: &str, _args: &serde_json::Value) -> String {
            self.called.push(name.to_string());
            match self.tools.get_mut(name) {
                Some(q) if q.len() > 1 => q.pop_front().unwrap_or_default(),
                Some(q) => q.front().cloned().unwrap_or_default(),
                None => format!("ok:{name}"),
            }
        }
        fn generate(&mut self, _system: &str, _prompt: &str, _precise: bool) -> Result<String, String> {
            self.model.pop_front().unwrap_or_else(|| Err("no model here".into()))
        }
        fn notify(&mut self, _recipe_id: &str, text: &str) {
            self.said.push(text.to_string());
        }
        fn persona(&self) -> String {
            "Yantrik".into()
        }
        fn now(&self) -> f64 {
            self.clock
        }
    }

    fn tool(name: &str, store_as: &str) -> RecipeStep {
        RecipeStep::Tool { tool_name: name.into(), args: json!({}), store_as: store_as.into(), on_error: ErrorAction::Fail }
    }

    fn notify(message: &str) -> RecipeStep {
        RecipeStep::Notify { message: message.into() }
    }

    fn ask(question: &str, choices: &[&str], store_as: &str) -> RecipeStep {
        RecipeStep::AskUser {
            question: question.into(),
            store_as: store_as.into(),
            choices: Some(choices.iter().map(|c| c.to_string()).collect()),
        }
    }

    /// A Branch runs the steps of the side it takes, and only those (#176). The shell's own
    /// executor marked the Branch `then` or `else` and went on, running neither side.
    #[test]
    fn a_branch_runs_the_side_it_takes() {
        let steps = [
            tool("check", "urgent"),
            RecipeStep::Branch {
                condition: "urgent".into(),
                then_steps: vec![tool("page", "paged"), notify("Paged: {{paged}}")],
                else_steps: vec![tool("file", "filed")],
            },
            notify("Done."),
        ];
        let mut desk = Desk::new();
        desk.tool_says("check", &["yes"]);
        let id = desk.start(&steps);
        run(&mut desk, &id, 20);
        assert_eq!(desk.status(&id).0, RecipeStatus::Done);
        assert_eq!(desk.called, ["check", "page"], "the then side ran, and the else side did not");
        assert!(desk.said.iter().any(|s| s == "Paged: ok:page"), "{:?}", desk.said);
        assert_eq!(desk.var(&id, "filed"), None);
        let v = desk.view(&id);
        assert_eq!(v.steps[1].state, "done");
        assert_eq!(v.steps[1].path.as_deref(), Some("took then: page → Notify"));

        // The other way.
        let mut desk = Desk::new();
        desk.tool_says("check", &[""]);
        let id = desk.start(&steps);
        run(&mut desk, &id, 20);
        assert_eq!(desk.called, ["check", "file"]);
        assert_eq!(desk.var(&id, "filed"), Some(json!("ok:file")));
        assert_eq!(desk.view(&id).steps[1].path.as_deref(), Some("took else: file"));
    }

    /// A question inside a Branch waits for its answer like any other — the clock and a stray
    /// signal leave it alone — and the answer, by the screen's path, carries the arm on.
    #[test]
    fn a_question_inside_a_branch_waits_for_its_answer() {
        let steps = [
            RecipeStep::Branch {
                condition: "draft".into(),
                then_steps: vec![ask("Send {{draft}}?", &["yes", "no"], "send"), notify("send={{send}}")],
                else_steps: vec![],
            },
            notify("End."),
        ];
        let mut desk = Desk::new();
        let id = desk.start(&steps);
        RecipeStore::set_var(&desk.conn, &id, "draft", &json!("the memo"));
        assert_eq!(run(&mut desk, &id, 20), 1, "it asks, and waits");
        assert_eq!(desk.status(&id), (RecipeStatus::Waiting, 0), "at the Branch, which is not done");
        assert_eq!(desk.said.last().map(String::as_str), Some("Send the memo?\n1. yes\n2. no"));
        assert!(!RecipeStore::get_expired_waiting_at(&desk.conn, EIGHT_AM + 86_400.0).contains(&id), "no clock ends a question");
        assert_eq!(step(&mut desk, &id), Advance::Blocked, "nor does a stray signal");

        let v = desk.view(&id);
        assert!(v.can.answer);
        assert_eq!(v.question.as_ref().map(|q| q.text.as_str()), Some("Send the memo?"));
        assert!(recipe_view::apply(&desk.conn, &id, &RecipeOp::Answer("yes".into())).expect("answered").run_now);
        run(&mut desk, &id, 20);
        assert_eq!(desk.status(&id).0, RecipeStatus::Done);
        assert!(desk.said.iter().any(|s| s == "send=yes"), "{:?}", desk.said);
        assert_eq!(desk.view(&id).steps[0].path.as_deref(), Some("took then: Ask you (answered) → Notify"));
    }

    /// A JumpIf back is a loop: it goes round until its condition says stop, and the view counts
    /// the rounds.
    #[test]
    fn a_loop_goes_round_until_its_condition_and_counts_its_rounds() {
        let steps = [
            tool("count", "n"),
            RecipeStep::JumpIf {
                condition: Condition::Not { inner: Box::new(Condition::VarEquals { var: "n".into(), value: json!("3") }) },
                target_step: 0,
            },
            notify("n={{n}}"),
        ];
        let mut desk = Desk::new();
        desk.tool_says("count", &["1", "2", "3"]);
        let id = desk.start(&steps);
        run(&mut desk, &id, 50);
        assert_eq!(desk.status(&id).0, RecipeStatus::Done);
        assert_eq!(desk.called.len(), 3, "three rounds");
        assert!(desk.said.iter().any(|s| s == "n=3"), "{:?}", desk.said);
        let v = desk.view(&id);
        assert_eq!(v.steps[1].path.as_deref(), Some("went on to step 3 after looping back 2 times"));
        assert!(v.steps.iter().all(|s| s.state == "done"), "{:?}", v.steps.iter().map(|s| &s.state).collect::<Vec<_>>());
    }

    /// A loop that never waits is stopped, with a reason a person can act on — not left to spin
    /// the worker for ever.
    #[test]
    fn a_loop_that_never_waits_is_stopped_and_says_why() {
        let steps = [tool("poll", "x"), RecipeStep::JumpIf { condition: Condition::VarExists { var: "x".into() }, target_step: 0 }];
        let mut desk = Desk::new();
        let id = desk.start(&steps);
        run(&mut desk, &id, 10_000);
        let r = RecipeStore::get(&desk.conn, &id).expect("the recipe");
        assert_eq!(r.status, RecipeStatus::Failed);
        let why = r.error_message.expect("a reason");
        assert!(why.starts_with("Stopped: 100 steps ran without a pause"), "{why}");
        assert!(why.contains("step 2 had gone back 50 times"), "{why}");
        assert_eq!(desk.called.len(), 50);
        assert!(desk.said.last().is_some_and(|s| s.starts_with("Recipe 'Digest' stopped: 100 steps")), "{:?}", desk.said.last());
        assert_eq!(desk.view(&id).status, "failed");

        // A recipe that needs more, or fewer, says so.
        let mut desk = Desk::new();
        let id = desk.start(&steps);
        RecipeStore::set_var(&desk.conn, &id, STEP_BUDGET_VAR, &json!(6));
        run(&mut desk, &id, 10_000);
        assert_eq!(desk.called.len(), 3);
        assert_eq!(desk.status(&id).0, RecipeStatus::Failed);
    }

    /// A loop that waits each round is not spinning, however many rounds it goes.
    #[test]
    fn a_loop_that_waits_each_round_runs_on() {
        let steps = [
            tool("poll", "x"),
            RecipeStep::WaitFor { condition: WaitCondition::Duration { seconds: 60 }, timeout_secs: None },
            RecipeStep::JumpIf { condition: Condition::VarExists { var: "x".into() }, target_step: 0 },
        ];
        let mut desk = Desk::new();
        let id = desk.start(&steps);
        run(&mut desk, &id, 10);
        for _ in 0..60 {
            desk.clock += 60.0;
            assert_eq!(RecipeStore::get_expired_waiting_at(&desk.conn, desk.clock), vec![id.clone()]);
            run(&mut desk, &id, 10);
        }
        assert_eq!(desk.status(&id), (RecipeStatus::Waiting, 2));
        assert_eq!(desk.called.len(), 61);
    }

    /// A timer wakes on the clock — with nobody talking to the companion — and not before, and a
    /// stray signal does not walk the recipe past it (#176).
    #[test]
    fn a_timer_wakes_on_the_clock_and_not_before() {
        let steps = [
            notify("Starting."),
            RecipeStep::WaitFor { condition: WaitCondition::Duration { seconds: 900 }, timeout_secs: None },
            notify("Later."),
        ];
        let mut desk = Desk::new();
        let id = desk.start(&steps);
        run(&mut desk, &id, 20);
        assert_eq!(desk.status(&id), (RecipeStatus::Waiting, 2));
        assert_eq!(desk.view(&id).waiting_for.as_deref(), Some("15m to pass, until 08:15 UTC"));
        // A second chain's signal, or a chat turn's sweep, finds it still waiting.
        assert_eq!(step(&mut desk, &id), Advance::Blocked);
        assert_eq!(desk.status(&id), (RecipeStatus::Waiting, 2));
        assert!(!desk.said.iter().any(|s| s == "Later."));

        // Not a second early.
        assert!(RecipeStore::get_expired_waiting_at(&desk.conn, EIGHT_AM + 899.0).is_empty());
        // Paused past its time and resumed: due at once. Its time stands; it does not start over.
        recipe_view::apply(&desk.conn, &id, &RecipeOp::Pause).expect("paused");
        desk.clock = EIGHT_AM + 1_000.0;
        assert!(RecipeStore::get_expired_waiting_at(&desk.conn, desk.clock).is_empty(), "paused");
        assert!(!recipe_view::apply(&desk.conn, &id, &RecipeOp::Resume).expect("resumed").run_now);
        assert_eq!(RecipeStore::get_expired_waiting_at(&desk.conn, desk.clock), vec![id.clone()]);

        run(&mut desk, &id, 20);
        assert_eq!(desk.status(&id).0, RecipeStatus::Done);
        assert!(desk.said.iter().any(|s| s == "Later."));
        assert_eq!(desk.view(&id).steps[1].result.as_deref(), Some("waited until 08:15 UTC"));
    }

    /// What the worker's clock moves: the recipes running, and the waits that are over — not a
    /// question, and not a timer still counting.
    #[test]
    fn the_clock_moves_what_is_due() {
        let desk = Desk::new();
        let running = desk.start(&[notify("a")]);
        let asking = desk.start(&[ask("Which?", &["a", "b"], "which"), notify("b")]);
        RecipeStore::complete_step(&desk.conn, &asking, 0, "asked");
        RecipeStore::update_status(&desk.conn, &asking, &RecipeStatus::Waiting, 1);
        let timed = desk.start(&[RecipeStep::WaitFor { condition: WaitCondition::Duration { seconds: 0 }, timeout_secs: None }]);
        let far = json!({"step": 0, "since": now_ts(), "until": now_ts() + 3_600.0});
        RecipeStore::set_var(&desk.conn, &timed, WAIT_VAR, &far);
        RecipeStore::update_status(&desk.conn, &timed, &RecipeStatus::Waiting, 1);
        let over = desk.start(&[RecipeStep::WaitFor { condition: WaitCondition::Duration { seconds: 0 }, timeout_secs: None }]);
        RecipeStore::set_var(&desk.conn, &over, WAIT_VAR, &json!({"step": 0, "since": 1.0, "until": 2.0}));
        RecipeStore::update_status(&desk.conn, &over, &RecipeStatus::Waiting, 1);
        let mut moved = due(&desk.conn);
        moved.sort();
        let mut expected = vec![running, over];
        expected.sort();
        assert_eq!(moved, expected);
    }

    /// A tool's failure goes where its `on_error` says: skipped here and the recipe runs on;
    /// failed there, and the recipe stops with the tool's words.
    #[test]
    fn a_failed_tool_does_what_its_on_error_says() {
        let skip = RecipeStep::Tool { tool_name: "flaky".into(), args: json!({}), store_as: "f".into(), on_error: ErrorAction::Skip };
        let mut desk = Desk::new();
        desk.tool_says("flaky", &["Error: no network"]);
        let id = desk.start(&[skip, notify("on")]);
        run(&mut desk, &id, 20);
        assert_eq!(desk.status(&id).0, RecipeStatus::Done);
        assert_eq!(desk.view(&id).steps[0].state, "skipped");

        let mut desk = Desk::new();
        desk.tool_says("flaky", &["Error: no network"]);
        let id = desk.start(&[tool("flaky", "f"), notify("never")]);
        run(&mut desk, &id, 20);
        let r = RecipeStore::get(&desk.conn, &id).expect("the recipe");
        assert_eq!((r.status, r.error_message.as_deref()), (RecipeStatus::Failed, Some("Error: no network")));
        assert!(!desk.said.iter().any(|s| s == "never"));
    }

    /// The shell's host: the companion runs a recipe's tools from its registry, its Think step
    /// through its model, the data steps for real, and says what the recipe came to as its own
    /// message — the executor the shell's worker calls, end to end.
    #[test]
    fn the_companion_runs_a_recipe_with_its_own_tools_and_model() {
        use yantrik_ml::{LLMBackend, LLMResponse};

        struct Scripted;
        impl LLMBackend for Scripted {
            fn chat(&self, _m: &[ChatMessage], _c: &GenerationConfig, _t: Option<&[serde_json::Value]>) -> anyhow::Result<LLMResponse> {
                Ok(LLMResponse {
                    text: "<think>hm</think>Two recipes, both fine.".into(),
                    prompt_tokens: 0,
                    completion_tokens: 1,
                    tool_calls: vec![],
                    api_tool_calls: vec![],
                    stop_reason: "stop".into(),
                })
            }
            fn chat_streaming(
                &self,
                m: &[ChatMessage],
                c: &GenerationConfig,
                t: Option<&[serde_json::Value]>,
                _on_token: &mut dyn FnMut(&str),
            ) -> anyhow::Result<LLMResponse> {
                self.chat(m, c, t)
            }
            fn count_tokens(&self, text: &str) -> anyhow::Result<usize> {
                Ok(text.len())
            }
            fn backend_name(&self) -> &str {
                "scripted"
            }
        }

        let db = yantrikdb_core::YantrikDB::new(":memory:", 384).expect("in-memory database");
        let mut config = crate::config::CompanionConfig::default();
        config.tools.enabled = false;
        let mut companion = CompanionService::new(db, std::sync::Arc::new(Scripted), config);
        let steps = [
            RecipeStep::Tool { tool_name: "list_recipes".into(), args: json!({}), store_as: "listed".into(), on_error: ErrorAction::Fail },
            RecipeStep::Branch {
                condition: "listed".into(),
                then_steps: vec![RecipeStep::Think { prompt: "Summarise {{listed}}".into(), store_as: "summary".into(), fallback_template: None }],
                else_steps: vec![],
            },
            RecipeStep::Extract { input_var: "summary".into(), pattern: "/[A-Z][a-z]+ recipes/".into(), store_as: "head".into() },
            notify("{{head}}: {{summary}}"),
        ];
        let id = RecipeStore::create(&companion.db.conn(), "Recipe check", "", &steps, None);
        RecipeStore::update_status(&companion.db.conn(), &id, &RecipeStatus::Running, 0);
        run(&mut companion, &id, 20);

        let (status, vars) = {
            let conn = companion.db.conn();
            (RecipeStore::get(&conn, &id).map(|r| r.status), RecipeStore::get_vars(&conn, &id))
        };
        assert_eq!(status, Some(RecipeStatus::Done));
        assert!(vars["listed"].as_str().is_some_and(|s| s.starts_with("Recipes (")), "{:?}", vars["listed"]);
        assert_eq!(vars["summary"], json!("Two recipes, both fine."), "the model's answer, its thinking stripped");
        assert_eq!(vars["head"], json!("Two recipes"), "Extract ran for real, not passed through");
        let said = companion.take_proactive_message().expect("the companion tells the person");
        assert_eq!(said.urge_ids, [format!("recipe:{id}")]);
        assert!(said.text.contains("Two recipes: Two recipes, both fine."), "{}", said.text);
    }
}
