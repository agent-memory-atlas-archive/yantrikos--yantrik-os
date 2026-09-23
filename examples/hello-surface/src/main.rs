//! The smallest complete surface, so the size of the job is not a matter of opinion.
//!
//! A counter. A mind can read it (`describe` is free: reading an app costs nobody anything), add
//! to it (`increment`, graded `standard`: it changes something, so it is not `safe`, but nothing
//! that cannot be undone), and set it back to zero (`reset`, graded `sensitive`: it throws away
//! what was counted, so in `ask` mode the person sees a card and presses Allow before it runs).
//! Everything here that is not the counter is the entire cost of putting something where a mind
//! can find it — the Rust twin of `crates/yantrik-harness/examples/echo_harness.rs`, and of
//! `examples/hello_surface.py` in Python.
//!
//! Note what is absent. No argument parsing, no type checks, no permission code, no revision
//! bookkeeping, no JSON-RPC: the dispatch refuses `by=two` before the handler runs, answers every
//! act with the state after it, refuses an act decided on a state the counter has left, and holds
//! `reset` for the person's Allow. The handlers only count.
//!
//! ```text
//! cargo run -p hello-surface
//!
//! yos ls                               # app-counter is there
//! yos describe counter                 # "Counter — 0, last changed by nobody yet"
//! yos act counter increment by=2       # accepted, settled, and the view after it
//! yos act counter increment            # `by` defaults to 1
//! yos act counter increment by=two     # refused: `by` must be an integer
//! yos act counter reset                # sensitive: a card first, in ask mode
//! yos check counter                    # does it keep the protocol? (it does: tests/yos_check.rs)
//! ```
//!
//! Running, it is found by its socket. To be found while it is not — listed, opened by name — it
//! would ship a `.desktop` file with `X-Yantrik-Surface=counter`: docs/sdk/found-while-closed.md,
//! and `templates/rust-surface`, which is this program grown into something to copy.

use std::sync::{Arc, Mutex};

use yantrik_surface::{caller, Action, Param, Surface, View};

/// Everything the counter knows. Behind a lock because the socket answers on more than one
/// thread; a real app keeps its own state however it likes, and the surface only reads it.
#[derive(Default)]
struct Counter {
    count: i64,
    /// Who changed it last, as the kernel reported them — never as they said.
    last_by: Option<i32>,
}

fn main() {
    let counter = Arc::new(Mutex::new(Counter::default()));

    let surface = Surface::new("counter")
        // What a mind reads first: one line a person could read, and a small state object.
        .describe({
            let counter = counter.clone();
            move || {
                let c = counter.lock().unwrap_or_else(|e| e.into_inner());
                let by = c.last_by.map_or("nobody yet".to_string(), |pid| format!("pid {pid}"));
                View::new(format!("Counter — {}, last changed by {by}", c.count))
                    .with("count", c.count)
                    .with("last_changed_by_pid", c.last_by)
            }
        })
        .action(
            Action::new("increment", "Add to the counter")
                .arg(Param::integer("by").default(1).describe("How much to add; negative to subtract")),
            {
                let counter = counter.clone();
                move |args| {
                    // Present and an integer: the dispatch checked it, and filled in the default.
                    let by = args["by"].as_i64().unwrap_or(1);
                    let mut c = counter.lock().unwrap_or_else(|e| e.into_inner());
                    c.count = c.count.checked_add(by).ok_or("that would overflow the counter")?;
                    c.last_by = caller().map(|who| who.pid);
                    Ok(serde_json::json!({ "count": c.count }))
                }
            },
        )
        .action(
            Action::new("reset", "Set the counter back to zero, forgetting what was counted")
                .risk("sensitive"),
            {
                let counter = counter.clone();
                move |_| {
                    let mut c = counter.lock().unwrap_or_else(|e| e.into_inner());
                    let was = std::mem::take(&mut c.count);
                    c.last_by = caller().map(|who| who.pid);
                    Ok(serde_json::json!({ "was": was }))
                }
            },
        );

    // An author's own check: every declaration is one the dispatch can enforce.
    for problem in surface.registry().problems() {
        eprintln!("hello-surface: {problem}");
    }
    eprintln!("hello-surface: serving `counter` on {}; ctrl-c to stop", surface.address());
    if let Err(e) = surface.serve() {
        eprintln!("hello-surface: could not serve: {e}");
        std::process::exit(1);
    }
}
