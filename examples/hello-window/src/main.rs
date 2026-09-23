//! The counter of `examples/hello-surface`, on a window.
//!
//! A program with a window publishes its surface through `yantrik_app_runtime::control::App`
//! instead of `yantrik_surface::Surface`. It is the same dispatch — the same argument checks,
//! grades, revision guard and refusals — plus one thing a window needs: every `describe` and every
//! handler runs on the thread that owns the window, so a handler can read and change the UI
//! directly, and nothing moves the UI between the revision check and the handler.
//!
//! And one rule every window's `main` keeps: the event loop ends through
//! [`run_until_closed`](yantrik_app_runtime::run_until_closed), never `ui.run().unwrap()`. A
//! logout takes the display away, the loop returns an error, and an unwrap there turns every
//! logout into a crash record and skips whatever `main` does after its loop (#196).
//!
//! ```text
//! cargo run -p hello-window            # in a graphical session
//! yos describe hello-window            # "Hello, window — 0"
//! yos act hello-window increment by=2  # the number on the window changes
//! yos check hello-window
//! ```

use yantrik_app_runtime::control::{Action, App, Param, View};
use yantrik_app_runtime::prelude::*;

slint::slint! {
    import { Button } from "std-widgets.slint";

    export component HelloWindow inherits Window {
        in-out property <int> count: 0;
        callback add-one();
        title: "Hello, window";
        preferred-width: 280px;
        VerticalLayout {
            padding: 16px;
            spacing: 12px;
            Text { text: "Count: " + root.count; font-size: 24px; }
            Button { text: "Add one"; clicked => { root.add-one(); } }
        }
    }
}

fn main() {
    init_tracing("hello-window");
    let ui = match HelloWindow::new() {
        Ok(ui) => ui,
        Err(e) => {
            eprintln!("hello-window: no window to open: {e}");
            std::process::exit(1);
        }
    };

    // The person's button and the mind's action change the same property.
    let weak = ui.as_weak();
    ui.on_add_one({
        let weak = weak.clone();
        move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_count(ui.get_count().saturating_add(1));
            }
        }
    });

    App::new("hello-window")
        // On the window's thread: read the UI itself.
        .describe({
            let weak = weak.clone();
            move || {
                let count = weak.upgrade().map_or(0, |ui| ui.get_count());
                View::new(format!("Hello, window — {count}")).with("count", count)
            }
        })
        .action(
            Action::new("increment", "Add to the counter on the window")
                .arg(Param::integer("by").default(1).describe("How much to add; negative to subtract")),
            {
                let weak = weak.clone();
                move |args| {
                    let ui = weak.upgrade().ok_or("the window has closed")?;
                    let by = i32::try_from(args["by"].as_i64().unwrap_or(1)).map_err(|_| "`by` is too large")?;
                    let count = ui.get_count().checked_add(by).ok_or("that would overflow the counter")?;
                    ui.set_count(count);
                    Ok(serde_json::json!({ "count": count }))
                }
            },
        )
        .action(
            Action::new("reset", "Set the counter back to zero, forgetting what was counted").risk("sensitive"),
            {
                let weak = weak.clone();
                move |_| {
                    let ui = weak.upgrade().ok_or("the window has closed")?;
                    let was = ui.get_count();
                    ui.set_count(0);
                    Ok(serde_json::json!({ "was": was }))
                }
            },
        )
        // Before the loop, from the window's thread.
        .serve();

    // Not `ui.run().unwrap()`: a display that goes away is an ending, not a crash.
    let closed = run_until_closed(&ui, "hello-window");
    // Whatever a program does after its loop runs on every ending: save, stop helpers, say goodbye.
    eprintln!("hello-window: {}", if closed { "closed" } else { "the loop ended without a close; the log says why" });
}
