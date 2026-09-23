//! My Surface: a to-do list a mind can read, add to, tick off and search.
//!
//! A starting point to copy — `README.md` says how to rename it. Everything that makes it a
//! surface is in [`surface_over`]: what it reports (the `describe` closure), what it offers (each
//! [`Action`], graded), and the handlers. The dispatch around them is `yantrik-surface`'s: it
//! checks the arguments against their declarations, refuses a call decided on a view that has
//! moved (`STALE:`), holds each action to the machine's ceiling and the person's mode, spends a
//! person's grant, and answers every act with the view after it. So the handlers only do the work.

use std::sync::{Arc, Mutex, MutexGuard};

use yantrik_surface::serde_json::{json, Value};
use yantrik_surface::{Action, Param, Surface, View};

/// The id this surface publishes as `app`, and binds as `app-my-surface.sock`. The `.desktop`
/// file's `X-Yantrik-Surface` says the same; a test holds the two together.
pub const APP_ID: &str = "my-surface";

/// One thing to do.
#[derive(Clone, Debug, PartialEq)]
pub struct Task {
    pub title: String,
    pub priority: String,
    pub done: bool,
}

/// Everything the list knows. A real program keeps its state however it likes; the surface reads
/// it in `describe` and changes it only through the handlers.
#[derive(Default)]
pub struct Tasks {
    pub items: Vec<Task>,
}

impl Tasks {
    /// What a mind reads first: one line a person could read, and a small state object in the
    /// app's own words. Everything here goes into the revision, so a caller can tell when it moved.
    fn view(&self) -> View {
        let done = self.items.iter().filter(|t| t.done).count();
        let tasks: Vec<Value> = self
            .items
            .iter()
            .enumerate()
            .map(|(index, t)| json!({ "index": index, "title": t.title, "priority": t.priority, "done": t.done }))
            .collect();
        View::new(format!(
            "My Surface — {} task{}, {done} done",
            self.items.len(),
            if self.items.len() == 1 { "" } else { "s" }
        ))
        .with("tasks", tasks)
    }

    /// The task at `index`, or the sentence a caller reads when there is none.
    fn at(&mut self, args: &Value) -> Result<&mut Task, String> {
        let count = self.items.len();
        let index = args["index"].as_u64().and_then(|i| usize::try_from(i).ok());
        match index.and_then(|i| self.items.get_mut(i)) {
            Some(task) => Ok(task),
            None if count == 0 => Err("the list is empty; `add` a task first".into()),
            None => Err(format!("there is no task {}; the list has {count}, indexed 0 to {}", args["index"], count - 1)),
        }
    }
}

fn lock(tasks: &Mutex<Tasks>) -> MutexGuard<'_, Tasks> {
    tasks.lock().unwrap_or_else(|e| e.into_inner())
}

/// The surface over an empty list.
pub fn surface() -> Surface {
    surface_over(Arc::new(Mutex::new(Tasks::default())))
}

/// The surface over `tasks`, which the caller may keep a handle on (the tests do).
pub fn surface_over(tasks: Arc<Mutex<Tasks>>) -> Surface {
    Surface::new(APP_ID)
        .describe({
            let tasks = tasks.clone();
            move || lock(&tasks).view()
        })
        // `standard` (the default grade): it changes the list, and `remove` takes it back.
        .action(
            Action::new("add", "Put a task on the list")
                .arg(Param::text("title").describe("What needs doing, in a few words"))
                .arg(
                    Param::one_of("priority", &["low", "normal", "high"])
                        .default("normal")
                        .describe("How soon it matters"),
                ),
            {
                let tasks = tasks.clone();
                move |args| {
                    // Present and text: the dispatch checked. Whether it says anything is ours to check.
                    let title = args["title"].as_str().unwrap_or_default().trim();
                    if title.is_empty() {
                        return Err("`title` is empty; say what needs doing".into());
                    }
                    let priority = args["priority"].as_str().unwrap_or("normal");
                    let mut tasks = lock(&tasks);
                    tasks.items.push(Task { title: title.into(), priority: priority.into(), done: false });
                    Ok(json!({ "index": tasks.items.len() - 1, "title": title, "priority": priority }))
                }
            },
        )
        // `standard`: `done=false` undoes it.
        .action(
            Action::new("complete", "Mark a task done, or not done again with `done` false")
                .arg(Param::integer("index").describe("The task's `index`, as describe lists it"))
                .arg(Param::flag("done").default(true).describe("false to mark it not done")),
            {
                let tasks = tasks.clone();
                move |args| {
                    let mut tasks = lock(&tasks);
                    let task = tasks.at(args)?;
                    task.done = args["done"].as_bool().unwrap_or(true);
                    Ok(json!({ "title": task.title, "done": task.done }))
                }
            },
        )
        // `safe`: it reads and changes nothing, so it runs in every mode, plan included.
        .action(
            Action::new("find", "Search the tasks' titles, changing nothing")
                .risk("safe")
                .arg(Param::text("query").describe("Words to look for, in any case")),
            {
                let tasks = tasks.clone();
                move |args| {
                    let query = args["query"].as_str().unwrap_or_default().to_lowercase();
                    let tasks = lock(&tasks);
                    let matches: Vec<Value> = tasks
                        .items
                        .iter()
                        .enumerate()
                        .filter(|(_, t)| t.title.to_lowercase().contains(&query))
                        .map(|(index, t)| json!({ "index": index, "title": t.title, "done": t.done }))
                        .collect();
                    Ok(json!({ "query": query, "matches": matches }))
                }
            },
        )
        // `sensitive`, and its description says it cannot be undone: in `ask` mode the person
        // sees a card first, and — because of those words — in `auto` mode too.
        .action(
            Action::new("remove", "Take a task off the list. It cannot be undone")
                .risk("sensitive")
                .arg(Param::integer("index").describe("The task's `index`, as describe lists it")),
            {
                let tasks = tasks.clone();
                move |args| {
                    let mut tasks = lock(&tasks);
                    let title = tasks.at(args)?.title.clone();
                    let index = args["index"].as_u64().unwrap_or_default() as usize;
                    tasks.items.remove(index);
                    Ok(json!({ "removed": title, "left": tasks.items.len() }))
                }
            },
        )
}
