//! Yantrik Document Editor — standalone app binary.
//!
//! Rich document editing with comments, track changes, version history, AI assist.

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use yantrik_app_runtime::prelude::*;

slint::include_modules!();

/// Fill the agent rail from the document on screen.
///
/// Its own headings are the context: they are what the document IS, the app already computes
/// them for the outline panel, and a heading list is the one thing here that is true without
/// asking anybody. Memory is added when the companion is reachable, filtered by the shared
/// relevance floor -- see companion::recall_relevant for why that floor exists.
fn refresh_agent_rail(ui: &DocumentEditorApp) {
    let mut context: Vec<AgentContextItem> = Vec::new();
    for h in ui.get_doc_headings().iter().take(6) {
        context.push(AgentContextItem {
            id: format!("heading:{}", h.block_index).into(),
            label: h.title.clone(),
            detail: format!("H{}", h.level).into(),
            source: "outline".into(),
        });
    }
    ui.set_agent_context(ModelRc::new(VecModel::from(context.clone())));

    let has_text = ui.get_doc_word_count() > 0;
    let online = companion::is_online();
    let mut next: Vec<AgentSuggestion> = Vec::new();
    if has_text && online {
        next.push(AgentSuggestion {
            id: "summarize".into(),
            label: "Summarise it".into(),
            detail: "five bullets, from what it says".into(),
            icon: "template".into(),
            running: ui.get_proposal_working(),
            proposes: true,
        });
        next.push(AgentSuggestion {
            id: "tighten".into(),
            label: "Make it shorter".into(),
            detail: "clearer, keeping every fact".into(),
            icon: "spark".into(),
            running: ui.get_proposal_working(),
            proposes: true,
        });
    }
    ui.set_agent_suggestions(ModelRc::new(VecModel::from(next)));

    ui.set_agent_unavailable(if online || !has_text {
        SharedString::new()
    } else {
        companion::OFFLINE_HINT.into()
    });

    if online && has_text {
        let query = ui.get_doc_title().to_string();
        if query.trim().is_empty() {
            return;
        }
        let back = ui.as_weak();
        std::thread::spawn(move || {
            let found =
                companion::recall_relevant(&query, companion::RELEVANCE_FLOOR, 3);
            if found.is_empty() {
                return;
            }
            let _ = back.upgrade_in_event_loop(move |ui| {
                let mut rows = context;
                for m in found {
                    let line = m.text.lines().next().unwrap_or("").trim().to_string();
                    rows.push(AgentContextItem {
                        id: format!("memory:{}", m.rid).into(),
                        label: line.into(),
                        detail: format!("{}% match", (m.score * 100.0).round() as i64).into(),
                        source: "memory".into(),
                    });
                }
                ui.set_agent_context(ModelRc::new(VecModel::from(rows)));
            });
        });
    }
}

fn main() {
    init_tracing("yantrik-document-editor");

    // One window per app: a second launch defers to the running one (the shell focuses it).
    let Some(_instance) = instance::claim("document-editor") else { return };

    let app = DocumentEditorApp::new().unwrap();

    // Same dark/accent choice as the shell, read from the shell's settings file.
    let theme = theme::load();
    app.global::<ThemeMode>().set_dark(theme.dark);
    app.global::<AccentPreset>().set_index(theme.accent_index);

    wire(&app);
    app.run().unwrap();
}

fn wire(app: &DocumentEditorApp) {
    // ── Document operations ──
    {
        let weak = app.as_weak();
        app.on_doc_new(move || {
            let Some(ui) = weak.upgrade() else { return };
            ui.set_doc_title("Untitled".into());
            ui.set_doc_content("".into());
            ui.set_doc_is_modified(false);
            ui.set_doc_word_count(0);
            refresh_agent_rail(&ui);
            ui.set_doc_char_count(0);
        });
    }

    {
        let weak = app.as_weak();
        app.on_doc_save(move || {
            let Some(ui) = weak.upgrade() else { return };
            let path = ui.get_doc_file_path().to_string();
            if path.is_empty() {
                tracing::info!("No file path set");
                return;
            }
            let content = ui.get_doc_content().to_string();
            match std::fs::write(&path, &content) {
                Ok(_) => {
                    ui.set_doc_is_modified(false);
                    ui.set_doc_save_status("Saved".into());
                    tracing::info!("Saved document to {path}");
                }
                Err(e) => {
                    ui.set_doc_save_status(format!("Save failed: {e}").into());
                }
            }
        });
    }

    app.on_doc_open(|| { tracing::info!("Open document"); });

    // ── Content changed ──
    {
        let weak = app.as_weak();
        app.on_doc_content_changed(move |content| {
            let Some(ui) = weak.upgrade() else { return };
            ui.set_doc_is_modified(true);
            let text = content.to_string();
            ui.set_doc_word_count(text.split_whitespace().count() as i32);
            refresh_agent_rail(&ui);
            ui.set_doc_char_count(text.len() as i32);
        });
    }

    // ── Formatting ──
    app.on_doc_format_bold(|| { tracing::info!("Format bold"); });
    app.on_doc_format_italic(|| { tracing::info!("Format italic"); });
    app.on_doc_format_underline(|| { tracing::info!("Format underline"); });
    app.on_doc_format_heading(|level| { tracing::info!("Format heading level {level}"); });
    app.on_doc_format_bullet(|| { tracing::info!("Format bullet"); });
    app.on_doc_format_checklist(|| { tracing::info!("Format checklist"); });
    app.on_doc_format_quote(|| { tracing::info!("Format quote"); });
    app.on_doc_format_code(|| { tracing::info!("Format code"); });
    app.on_doc_format_divider(|| { tracing::info!("Format divider"); });
    app.on_doc_format_strikethrough(|| { tracing::info!("Format strikethrough"); });
    app.on_doc_format_highlight(|| { tracing::info!("Format highlight"); });
    app.on_doc_format_link(|url| { tracing::info!("Format link: {url}"); });
    app.on_doc_format_inline_code(|| { tracing::info!("Format inline code"); });

    // ── Undo / Redo ──
    app.on_doc_undo(|| { tracing::info!("Undo"); });
    app.on_doc_redo(|| { tracing::info!("Redo"); });

    // ── Find / Replace ──
    app.on_doc_find_next(|| { tracing::info!("Find next"); });
    app.on_doc_find_prev(|| { tracing::info!("Find prev"); });
    app.on_doc_replace_one(|| { tracing::info!("Replace one"); });
    app.on_doc_replace_all(|| { tracing::info!("Replace all"); });

    // ── Heading navigation ──
    app.on_doc_heading_clicked(|idx| { tracing::info!("Heading clicked: {idx}"); });

    // ── Import / Export ──
    app.on_doc_import_md(|| { tracing::info!("Import markdown"); });
    app.on_doc_export_md(|| { tracing::info!("Export markdown"); });
    app.on_doc_export_pdf(|| { tracing::info!("Export PDF"); });
    app.on_doc_export_html(|| { tracing::info!("Export HTML"); });

    // ── Comments ──
    app.on_doc_add_comment(|text| { tracing::info!("Add comment: {text}"); });
    app.on_doc_delete_comment(|id| { tracing::info!("Delete comment {id}"); });
    app.on_doc_resolve_comment(|id| { tracing::info!("Resolve comment {id}"); });

    // ── Track changes ──
    app.on_doc_toggle_track_changes(|| { tracing::info!("Toggle track changes"); });
    app.on_doc_accept_change(|id| { tracing::info!("Accept change {id}"); });
    app.on_doc_reject_change(|id| { tracing::info!("Reject change {id}"); });
    app.on_doc_accept_all_changes(|| { tracing::info!("Accept all changes"); });
    app.on_doc_reject_all_changes(|| { tracing::info!("Reject all changes"); });

    // ── Version history ──
    app.on_doc_save_version(|label| { tracing::info!("Save version: {label}"); });
    app.on_doc_list_versions(|| { tracing::info!("List versions"); });
    app.on_doc_restore_version(|idx| { tracing::info!("Restore version {idx}"); });

    // ── Tables ──
    app.on_doc_insert_table(|rows, cols| { tracing::info!("Insert table {rows}x{cols}"); });
    app.on_doc_add_table_row(|| { tracing::info!("Add table row"); });
    app.on_doc_add_table_col(|| { tracing::info!("Add table col"); });

    // ── Insert ──
    app.on_doc_insert_toc(|| { tracing::info!("Insert TOC"); });
    app.on_doc_insert_footnote(|| { tracing::info!("Insert footnote"); });
    app.on_doc_insert_image(|path| { tracing::info!("Insert image: {path}"); });

    // ── Templates ──
    app.on_doc_use_template(|idx| { tracing::info!("Use template {idx}"); });

    // ── Page layout ──
    app.on_doc_set_page_layout(|size| { tracing::info!("Set page layout: {size}"); });
    app.on_doc_print_preview(|| { tracing::info!("Print preview"); });

    // ── AI assist ──
    // ── The agent layer ──
    //
    // The rewritten document waits here between the answer arriving and the person pressing
    // Replace. It is not a UI property because nothing draws it: the card shows the text, and
    // this is what gets written if the card is accepted.
    let pending: std::sync::Arc<std::sync::Mutex<String>> = Default::default();

    // doc_ai_submit logged the prompt and returned. It asks the companion now, and the answer
    // comes back as a proposal that says what applying it would do -- which here is "replace
    // the document", so it says that before the button is pressed.
    {
        let weak = app.as_weak();
        let pending_w = pending.clone();
        app.on_doc_ai_submit(move |prompt| {
            let Some(ui) = weak.upgrade() else { return };
            let body = ui.get_doc_content().to_string();
            if body.trim().is_empty() {
                ui.set_proposal(AgentProposal {
                    title: "Nothing to work on".into(),
                    body: "This document is empty.".into(),
                    verb: "Close".into(),
                    ..Default::default()
                });
                return;
            }

            ui.set_proposal_working(true);
            ui.set_proposal(AgentProposal {
                title: "Rewriting".into(),
                source: "from this document".into(),
                ..Default::default()
            });

            let ask = format!(
                "{prompt}\n\nHere is the document. Use only what it says; invent nothing. \
                 Reply with the rewritten document only.\n\n{body}"
            );
            let back = ui.as_weak();
            let pending_w = pending_w.clone();
            std::thread::spawn(move || {
                let outcome = companion::ask(&ask);
                let _ = back.upgrade_in_event_loop(move |ui| {
                    ui.set_proposal_working(false);
                    match outcome {
                        Ok(text) => {
                            let words = text.split_whitespace().count();
                            ui.set_proposal(AgentProposal {
                                title: "Rewritten document".into(),
                                body: text.clone().into(),
                                source: "from this document".into(),
                                // This one really does replace what is on screen, so it says so
                                // and says how big the replacement is.
                                impact: format!("Replaces the document with {words} words, unsaved")
                                    .into(),
                                destructive: false,
                                verb: "Replace".into(),
                            });
                            if let Ok(mut p) = pending_w.lock() { *p = text; }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "Companion call failed");
                            ui.set_proposal(AgentProposal {
                                title: "The companion did not answer".into(),
                                body: format!("{e}\n\nIs the Yantrik shell running?").into(),
                                verb: "Close".into(),
                                ..Default::default()
                            });
                        }
                    }
                });
            });
        });
    }
    {
        let weak = app.as_weak();
        let pending_r = pending.clone();
        app.on_proposal_applied(move || {
            let Some(ui) = weak.upgrade() else { return };
            let text = pending_r.lock().map(|p| p.clone()).unwrap_or_default();
            if text.is_empty() {
                return;
            }
            ui.set_doc_content(text.into());
            // Left unsaved on purpose, the same as Notes: generated text is looked at before
            // it is kept.
            ui.set_doc_is_modified(true);
            ui.set_proposal(AgentProposal::default());
            refresh_agent_rail(&ui);
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
    {
        let weak = app.as_weak();
        app.on_agent_suggestion_activated(move |id| {
            let Some(ui) = weak.upgrade() else { return };
            match id.as_str() {
                "summarize" => ui.invoke_doc_ai_submit(
                    "Summarise this document in at most five bullet points.".into(),
                ),
                "tighten" => ui.invoke_doc_ai_submit(
                    "Rewrite this document to be shorter and clearer, keeping every fact.".into(),
                ),
                other => tracing::warn!(id = other, "unknown rail suggestion"),
            }
        });
    }
    app.on_agent_context_activated(|_| {});
    app.on_doc_ai_apply(|| { tracing::info!("AI apply"); });
    app.on_doc_ai_dismiss(|| { tracing::info!("AI dismiss"); });
    app.on_doc_ai_draft(|topic| { tracing::info!("AI draft: {topic}"); });
    app.on_doc_ai_summarize(|| { tracing::info!("AI summarize"); });
    app.on_doc_ai_improve(|| { tracing::info!("AI improve"); });
    app.on_doc_ai_translate(|lang| { tracing::info!("AI translate to {lang}"); });
    app.on_doc_ai_insights(|| { tracing::info!("AI insights"); });
}
