//! A recipe, operable over the shell's surface: answer the question one waits on, pause it,
//! resume it, cancel it.
//!
//! The same four things the Recipes screen's buttons do, through the same door — the companion
//! worker applies each with `yantrik_companion::recipe_view::apply`, which answers through the
//! chat's own AskUser path and pauses, resumes and cancels through the recipe store. What a recipe
//! is doing is read from `describe shell` → `recipes`; its `can` says which of these apply.
//!
//! Deferred, all four: the worker may be in the middle of a generation, and the shell's UI thread
//! never waits on it. A request is checked here against the recipes as last published, so a caller
//! is refused at once for a recipe that does not exist or cannot take it; what the worker then
//! made of it is in `describe shell` and on the Recipes screen.

use yantrik_app_runtime::control::{Action, App as ControlSurface, Param};
use yantrik_companion::recipe_view::{RecipeOp, RecipeView};

use crate::bridge::CompanionHandle;

/// Add the recipe actions to the shell's control surface.
pub fn actions(surface: ControlSurface, companion: CompanionHandle) -> ControlSurface {
    let recipe = || Param::text("recipe").describe("The recipe's id from `describe shell` → `recipes`, or its name");
    let (pause, resume, cancel) = (companion.clone(), companion.clone(), companion.clone());
    surface
        .action(
            Action::new(
                "answer_recipe",
                "Answer the question a recipe is waiting on, as the person would from the Recipes \
                 screen. Its `question` in describe has the text and the choices offered; any \
                 answer is taken. The recipe goes on with it",
            )
            .defers()
            .arg(recipe())
            .arg(Param::text("text").describe("The answer: one of the choices, or your own words")),
            move |args| {
                let text = args["text"].as_str().unwrap_or_default().trim().to_string();
                if text.is_empty() {
                    return Err("`text` is empty".into());
                }
                request(&companion, args, RecipeOp::Answer(text), |v| v.can.answer, "is not waiting for an answer")
            },
        )
        .action(
            Action::new("pause_recipe", "Hold a running or waiting recipe where it is, until resume_recipe")
                .defers()
                .arg(recipe()),
            move |args| request(&pause, args, RecipeOp::Pause, |v| v.can.pause, "is not running or waiting"),
        )
        .action(
            Action::new(
                "resume_recipe",
                "Let a paused recipe go on — running, or waiting again on what it was waiting for",
            )
            .defers()
            .arg(recipe()),
            move |args| request(&resume, args, RecipeOp::Resume, |v| v.can.resume, "is not paused"),
        )
        .action(
            Action::new("cancel_recipe", "Stop a running, waiting or paused recipe for good. It cannot be resumed")
                .defers()
                .arg(recipe()),
            move |args| request(&cancel, args, RecipeOp::Cancel, |v| v.can.cancel, "has nothing to cancel"),
        )
}

/// Check the request against the recipes as last published, then hand it to the worker.
fn request(
    companion: &CompanionHandle,
    args: &serde_json::Value,
    op: RecipeOp,
    allowed: impl Fn(&RecipeView) -> bool,
    refusal: &str,
) -> Result<serde_json::Value, String> {
    let want = args["recipe"].as_str().unwrap_or_default().trim();
    let view = resolve(want)?;
    if !allowed(&view) {
        return Err(format!("`{}` {refusal} — it is {}", view.name, view.status));
    }
    let verb = op.verb();
    companion.recipe(view.id.clone(), op)?;
    Ok(serde_json::json!({
        "recipe": view.id,
        "name": view.name,
        "requested": verb,
        "read_back": "describe shell → recipes: its status, and `can` for what it takes now",
    }))
}

/// A recipe by id, or failing that by name, among the recipes as last published.
fn resolve(want: &str) -> Result<RecipeView, String> {
    if want.is_empty() {
        return Err("`recipe` is empty".into());
    }
    let snap = crate::recipes::snapshot();
    if !snap.loaded {
        return Err("the companion has not published its recipes yet".into());
    }
    pick(&snap.views, want).cloned().ok_or_else(|| {
        let known: Vec<&str> = snap.views.iter().filter(|v| !v.template).map(|v| v.id.as_str()).take(12).collect();
        format!(
            "no recipe `{want}`; `describe shell` lists them under `recipes`{}",
            if known.is_empty() { String::new() } else { format!(" ({})", known.join(", ")) }
        )
    })
}

fn pick<'a>(views: &'a [RecipeView], want: &str) -> Option<&'a RecipeView> {
    views
        .iter()
        .find(|v| v.id == want)
        .or_else(|| views.iter().find(|v| v.name.eq_ignore_ascii_case(want)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(id: &str, name: &str) -> RecipeView {
        RecipeView {
            id: id.into(),
            name: name.into(),
            description: String::new(),
            status: "running".into(),
            current_step: 0,
            created_at: 0.0,
            updated_at: 0.0,
            error: None,
            template: false,
            waiting_for: None,
            question: None,
            steps: Vec::new(),
            can: Default::default(),
        }
    }

    #[test]
    fn a_recipe_is_found_by_its_id_or_its_name() {
        let views = vec![named("rcp_1", "Tidy downloads"), named("rcp_2", "rcp_1")];
        assert_eq!(pick(&views, "rcp_1").map(|v| v.name.as_str()), Some("Tidy downloads"), "the id wins");
        assert_eq!(pick(&views, "tidy DOWNLOADS").map(|v| v.id.as_str()), Some("rcp_1"));
        assert!(pick(&views, "nothing").is_none());
    }

    /// The four actions are on the surface, graded standard, and settle later — the worker does
    /// the work — with the arguments the design names.
    #[test]
    fn the_recipe_actions_are_published_as_designed() {
        let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/control_recipes.rs")).unwrap();
        let src = src.split("#[cfg(test)]").next().unwrap();
        for name in ["answer_recipe", "pause_recipe", "resume_recipe", "cancel_recipe"] {
            let at = src.find(&format!("\"{name}\"")).unwrap_or_else(|| panic!("{name} is published"));
            let spec = &src[at..at + src[at..].find("move |args|").expect("a handler")];
            assert!(spec.contains(".defers()"), "{name} settles later");
            assert!(!spec.contains(".risk("), "{name} is standard, the default");
            assert!(spec.contains(".arg(recipe())"), "{name} takes `recipe`");
        }
        let control = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/control.rs")).unwrap();
        assert!(control.contains("crate::control_recipes::actions(surface, ctx.bridge.handle())"), "the shell adds them");
    }
}
