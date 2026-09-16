//! Yantrik Text Editor — standalone app binary.
//!
//! Multi-tab code/text editor with file open/save, find/replace, and AI assist.

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use yantrik_app_runtime::prelude::*;

slint::include_modules!();

/// Fill the agent rail from the file in the buffer.
///
/// What a plain-text editor knows is the file: its name, its type, how long it is. All three
/// are on screen already and none needs asking.
fn refresh_agent_rail(ui: &TextEditorApp) {
    let name = ui.get_file_name().to_string();
    let mut context: Vec<AgentContextItem> = Vec::new();
    if !name.is_empty() {
        context.push(AgentContextItem {
            id: "file".into(),
            label: name.clone().into(),
            detail: format!("{}, {} lines", ui.get_file_type(), ui.get_total_lines()).into(),
            source: "file".into(),
        });
    }
    ui.set_agent_context(ModelRc::new(VecModel::from(context)));

    let has_text = !ui.get_file_content().to_string().trim().is_empty();
    let online = companion::is_online();
    let mut next: Vec<AgentSuggestion> = Vec::new();
    if online && has_text {
        next.push(AgentSuggestion {
            id: "explain".into(),
            label: "Explain this file".into(),
            detail: "what it is and what it does".into(),
            icon: "spark".into(),
            running: ui.get_proposal_working(),
            proposes: false,
        });
    }
    ui.set_agent_suggestions(ModelRc::new(VecModel::from(next)));
    ui.set_agent_unavailable(if online || !has_text {
        SharedString::new()
    } else {
        companion::OFFLINE_HINT.into()
    });
}

fn main() {
    init_tracing("yantrik-text-editor");

    let app = TextEditorApp::new().unwrap();
    wire(&app);
    // ── The agent layer ──
    {
        let weak = app.as_weak();
        app.on_agent_suggestion_activated(move |id| {
            let Some(ui) = weak.upgrade() else { return };
            if id != "explain" {
                return;
            }
            let name = ui.get_file_name().to_string();
            let kind = ui.get_file_type().to_string();
            // The head of the file, not all of it: the question is what this IS.
            let head: String = ui
                .get_file_content()
                .to_string()
                .lines()
                .take(80)
                .collect::<Vec<_>>()
                .join("\n");
            ui.set_proposal_working(true);
            ui.set_proposal(AgentProposal {
                title: "What this file is".into(),
                source: "from the open file".into(),
                ..Default::default()
            });
            let prompt = format!(
                "This is {name}, a {kind} file. In at most four short lines say what it is and \
                 what it does. Use only what is here.\n\n{head}"
            );
            let back = ui.as_weak();
            std::thread::spawn(move || {
                let outcome = companion::ask(&prompt);
                let _ = back.upgrade_in_event_loop(move |ui| {
                    ui.set_proposal_working(false);
                    match outcome {
                        Ok(text) => ui.set_proposal(AgentProposal {
                            title: "What this file is".into(),
                            body: text.into(),
                            source: "from the open file".into(),
                            verb: "Close".into(),
                            ..Default::default()
                        }),
                        Err(e) => ui.set_proposal(AgentProposal {
                            title: "The companion did not answer".into(),
                            body: format!("{e}").into(),
                            verb: "Close".into(),
                            ..Default::default()
                        }),
                    }
                });
            });
        });
    }
    {
        let weak = app.as_weak();
        app.on_proposal_dismissed(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_proposal(AgentProposal::default());
            }
        });
    }
    app.on_proposal_applied(|| {});
    app.on_agent_context_activated(|_| {});
    // The rail follows the app's state on a timer.
    //
    // Calling it once at startup was not enough: at that moment Weather has no reading yet and
    // Image Viewer has no file, so both rails computed "nothing to say", collapsed, and stayed
    // collapsed for the life of the process. Every app loads its content on some path of its own
    // and hooking each one is how a refresh gets missed; asking every few seconds is cheap and
    // cannot be forgotten.
    let rail_timer = slint::Timer::default();
    {
        let weak = app.as_weak();
        rail_timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_secs(4),
            move || {
                if let Some(ui) = weak.upgrade() {
                    refresh_agent_rail(&ui);
                }
            },
        );
    }
    refresh_agent_rail(&app);

    app.run().unwrap();
}

fn wire(app: &TextEditorApp) {
    // ── Save file ──
    {
        let weak = app.as_weak();
        app.on_save_file(move || {
            let Some(ui) = weak.upgrade() else { return };
            let content = ui.get_file_content().to_string();
            let name = ui.get_file_name().to_string();
            if name.is_empty() || name == "untitled" {
                tracing::info!("No file path set; use Save As");
                return;
            }
            match std::fs::write(&name, &content) {
                Ok(_) => {
                    ui.set_is_modified(false);
                    tracing::info!("Saved {name}");
                }
                Err(e) => tracing::error!("Save failed: {e}"),
            }
        });
    }

    // ── Save file as ──
    {
        let weak = app.as_weak();
        app.on_save_file_as(move |dir, filename| {
            let Some(ui) = weak.upgrade() else { return };
            let path = format!("{}/{}", dir.to_string().trim_end_matches('/'), filename);
            let content = ui.get_file_content().to_string();
            match std::fs::write(&path, &content) {
                Ok(_) => {
                    ui.set_file_name(path.into());
                    ui.set_is_modified(false);
                    ui.set_show_save_dialog(false);
                    tracing::info!("Saved as {}", ui.get_file_name());
                }
                Err(e) => {
                    ui.set_save_error(format!("Save failed: {e}").into());
                }
            }
        });
    }

    // ── Content changed ──
    {
        let weak = app.as_weak();
        app.on_content_changed(move |content| {
            let Some(ui) = weak.upgrade() else { return };
            ui.set_is_modified(true);
            let lines = content.as_str().lines().count().max(1);
            ui.set_total_lines(lines as i32);
            let line_nums: String = (1..=lines).map(|n| format!("{:>4}", n)).collect::<Vec<_>>().join("\n");
            ui.set_line_numbers_text(line_nums.into());
        });
    }

    // ── Tab management ──
    app.on_switch_tab(|idx| { tracing::info!("Switch to tab {idx}"); });
    app.on_new_tab(|| { tracing::info!("New tab requested"); });
    app.on_close_tab(|idx| { tracing::info!("Close tab {idx}"); });

    // ── Go-to-line ──
    app.on_goto_line(|line| { tracing::info!("Go to line {line}"); });

    // ── Find & Replace ──
    app.on_find_next(|| { tracing::info!("Find next"); });
    app.on_find_prev(|| { tracing::info!("Find prev"); });
    app.on_replace_current(|| { tracing::info!("Replace current"); });
    app.on_replace_all(|| { tracing::info!("Replace all"); });
    app.on_find_query_changed(|_q| {});

    // ── AI assist ──
    app.on_ai_request(|prompt| { tracing::info!("AI request: {prompt} (standalone mode)"); });
    app.on_ai_insert(|| { tracing::info!("AI insert"); });
    app.on_ai_dismiss(|| { tracing::info!("AI dismiss"); });

    // ── Encoding / line ending / minimap ──
    app.on_editor_set_encoding(|enc| { tracing::info!("Set encoding: {enc}"); });
    app.on_editor_set_line_ending(|le| { tracing::info!("Set line ending: {le}"); });
    app.on_editor_toggle_minimap(|| { tracing::info!("Toggle minimap"); });
}
