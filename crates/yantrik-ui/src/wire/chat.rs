//! Chat wiring — on_send_message + on_lens_submit.
//!
//! Both go through `dispatch`, which asks the harness host which mind is driving before it
//! sends anything. They used to call the builtin companion directly.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use slint::{ComponentHandle, Timer};

use crate::app_context::AppContext;
use crate::bridge::CompanionBridge;
use crate::{apps, lens, streaming, App};

/// What the desktop tells a mind about where a turn came from: facts about the machine, as JSON.
///
/// A mind keeps its own clock but has no way to know where the computer is. Asked "what is the
/// weather like right now?" on a fresh install, Yantrik Mind answered for London while this
/// desktop had already worked out it was in Bentonville. These are facts about the machine,
/// never configuration for the mind — the same things the status bar shows.
pub(crate) fn desktop_context(place: &super::settings::Place) -> String {
    let mut machine = serde_json::Map::new();
    if !place.city.trim().is_empty() {
        machine.insert(
            "place".into(),
            serde_json::json!({ "city": place.city, "region": place.region, "country": place.country }),
        );
    }
    if !place.timezone.trim().is_empty() {
        machine.insert("timezone".into(), place.timezone.clone().into());
    }
    serde_json::json!({ "machine": machine }).to_string()
}

/// Send what the person typed to whichever mind is actually driving.
///
/// This is the join between the body and the mind, and until now it did not exist. `chat.rs`
/// called `bridge.send_message` directly — the builtin companion — so `use_harness` switched a
/// name in the machine rail and a chip in the status bar while every word still went to the
/// builtin. An attached harness could appear in the picker, be chosen, be shown as active, and
/// never be asked anything.
///
/// The host already knew how to do this: `Host::send` routes to the builtin or queues for the
/// attached harness and hands back a stream either way. It was simply never called.
fn dispatch(
    ui_weak: &slint::Weak<App>,
    bridge: &Arc<CompanionBridge>,
    text: &str,
    slot: &Rc<RefCell<Option<Timer>>>,
) {
    let Some(host) = super::harness::host() else {
        // No host yet (very early boot). The builtin is the only thing that could answer.
        streaming::start_ai_stream(ui_weak.clone(), bridge, text, slot);
        return;
    };

    // The builtin keeps its own path: it carries tool calls, the __REPLACE__ convention and the
    // job board, none of which the harness protocol has or needs.
    if host.active_id() == super::harness::BUILTIN_ID {
        streaming::start_ai_stream(ui_weak.clone(), bridge, text, slot);
        return;
    }

    // An attached harness answers in Chunks. Adapt them to the token protocol the pump already
    // speaks, on a thread, because `Answer` is a blocking std channel and this is the UI thread.
    let answer = host.send(
        yantrik_harness::Turn::new(text.to_string()).with_context(desktop_context(&super::settings::place())),
    );
    let (tx, rx) = crossbeam_channel::unbounded::<String>();
    std::thread::spawn(move || {
        // A closed channel is the end of the turn — that is the protocol, and it is why this
        // loop ends on recv() failing rather than on a sentinel.
        while let Ok(chunk) = answer.recv() {
            let sent = match chunk {
                yantrik_harness::Chunk::Text(t) => tx.send(t),
                // Said, not swallowed. A stream that simply stopped would look identical to a
                // harness that had finished, and the person would be left with half an answer
                // and no reason.
                yantrik_harness::Chunk::Failed(why) => tx
                    .send("__REPLACE__".to_string())
                    .and_then(|_| tx.send(why)),
            };
            if sent.is_err() {
                return;
            }
        }
        let _ = tx.send("__DONE__".to_string());
    });
    streaming::stream_into(ui_weak.clone(), rx, text, slot);
}

/// Wire on_send_message and on_lens_submit callbacks.
pub fn wire(ui: &App, ctx: &AppContext) {
    wire_send_message(ui, ctx);
    wire_lens_submit(ui, ctx);
}

/// Direct chat: send message → stream response.
fn wire_send_message(ui: &App, ctx: &AppContext) {
    let bridge = ctx.bridge.clone();
    let ui_weak = ui.as_weak();
    let timer_slot: Rc<RefCell<Option<Timer>>> = Rc::new(RefCell::new(None));
    let slot = timer_slot.clone();

    ui.on_send_message(move |text| {
        let text = text.to_string();
        if text.is_empty() {
            return;
        }
        // V22: No offline guard — companion handles offline mode internally
        // via OfflineResponder (memory recall + pattern matching + templates)
        dispatch(&ui_weak, &bridge, &text, &slot);
    });
}

/// Lens submit: try app launch first, fall back to AI streaming.
fn wire_lens_submit(ui: &App, ctx: &AppContext) {
    let bridge = ctx.bridge.clone();
    let ui_weak = ui.as_weak();
    let catalogue = ctx.installed_apps.clone();
    let timer_slot: Rc<RefCell<Option<Timer>>> = Rc::new(RefCell::new(None));
    let slot = timer_slot.clone();

    ui.on_lens_submit(move |query| {
        let query = query.to_string();
        if query.is_empty() {
            return;
        }

        tracing::info!(query = %query, "Lens submit");

        let lower = query.to_lowercase();

        // Check installed .desktop apps first
        // Bound, not inlined: `get()` hands back an Arc snapshot, and the search borrows
        // from it, so it has to outlive the call.
        let installed = catalogue.get();
        let app_matches = apps::search(&lower, &installed);
        if let Some(entry) = app_matches.first() {
            let parts: Vec<&str> = entry.exec.split_whitespace().collect();
            if let Some((bin, args)) = parts.split_first() {
                tracing::info!(exec = %entry.exec, name = %entry.name, "Launching app from Lens");
                match std::process::Command::new(bin).args(args).spawn() {
                    Ok(_) => tracing::info!(name = %entry.name, "App started"),
                    Err(e) => tracing::error!(name = %entry.name, error = %e, "Failed to launch"),
                }
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_lens_open(false);
                }
                return;
            }
        }

        // Fallback: hardcoded KNOWN_APPS
        for (app_id, cmd, _) in lens::KNOWN_APPS {
            if lower.contains(&format!("open {}", app_id))
                || lower.contains(app_id)
                || lower.contains(cmd)
            {
                tracing::info!(cmd, "Launching app from Lens (fallback)");
                match std::process::Command::new(cmd).spawn() {
                    Ok(_) => tracing::info!(cmd, "App started"),
                    Err(e) => tracing::error!(cmd, error = %e, "Failed to launch app"),
                }
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_lens_open(false);
                }
                return;
            }
        }

        // Not a launch and not a known app, so it is a question — and a question goes to
        // whichever mind is answering, the same as one typed into chat. This line called the
        // builtin directly, which meant the Lens (the primary way anyone talks to this desktop)
        // ignored the mind picker even after chat stopped doing so.
        dispatch(&ui_weak, &bridge, &query, &slot);
    });
}

#[cfg(test)]
mod tests {
    /// This file, read as text.
    ///
    /// The bug this guards was not a wrong line — it was a MISSING one, and no type, signature or
    /// call graph could have noticed. `on_send_message` called the builtin companion directly and
    /// compiled perfectly; the harness host, the socket, the picker and the status-bar chip were
    /// all built, all correct, and simply never consulted. Selecting a mind changed a label.
    ///
    /// Nothing observable was broken either: the desktop answered every question, because the
    /// builtin always answers. It took attaching a harness, watching it be chosen, and then
    /// watching its log stay empty to see it. So the property is asserted where it lives.
    const SELF_SRC: &str = include_str!("chat.rs");

    /// Everything below `dispatch` is the part of the file that decides where a message goes.
    /// Cut at the test module, or this test would read itself.
    fn callbacks() -> &'static str {
        let after = SELF_SRC
            .split_once("/// Wire on_send_message and on_lens_submit callbacks.")
            .expect("the wiring doc comment marks the end of dispatch")
            .1;
        &after[..after.find("#[cfg(test)]").unwrap_or(after.len())]
    }

    #[test]
    fn every_way_of_talking_to_this_desktop_asks_who_is_answering() {
        let body = callbacks();
        assert!(
            !body.contains("start_ai_stream"),
            "a callback sends straight to the builtin companion. Both ways of talking to this desktop — the chat panel and the Lens — must go through `dispatch`, which reads the harness host; otherwise choosing a mind changes a label and nothing else."
        );
        assert_eq!(
            body.matches("dispatch(").count(),
            2,
            "there are two entry points — on_send_message and the Lens fallback — and both should reach the chosen mind through `dispatch`"
        );
    }

    /// The builtin's own path still has to exist. `dispatch` sends to it by id rather than by
    /// being the default, so this is the one place the name may appear.
    #[test]
    fn the_builtin_is_chosen_by_name_not_by_default() {
        let before = SELF_SRC
            .split_once("/// Wire on_send_message and on_lens_submit callbacks.")
            .expect("the wiring doc comment marks the end of dispatch")
            .0;
        assert!(
            before.contains("harness::BUILTIN_ID"),
            "`dispatch` decides between the builtin and an attached harness by comparing the active id; without that comparison the builtin is simply whatever happens to run"
        );
    }
    #[test]
    fn a_turn_tells_the_mind_where_the_machine_is_and_nothing_more() {
        let place = crate::wire::settings::Place {
            city: "Bentonville".into(),
            region: "Arkansas".into(),
            country: "US".into(),
            lat: 36.37,
            lon: -94.2,
            timezone: "America/Chicago".into(),
            source: "detected".into(),
        };
        let v: serde_json::Value = serde_json::from_str(&super::desktop_context(&place)).unwrap();
        assert_eq!(v["machine"]["place"]["city"], "Bentonville");
        assert_eq!(v["machine"]["timezone"], "America/Chicago");
        // Coordinates and how the place was found stay on the machine.
        assert!(v["machine"].get("lat").is_none() && v["machine"]["place"].get("lat").is_none());
        assert!(v["machine"].get("source").is_none());

        let unknown: serde_json::Value =
            serde_json::from_str(&super::desktop_context(&Default::default())).unwrap();
        assert_eq!(unknown, serde_json::json!({ "machine": {} }));
    }

}
