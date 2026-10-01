//! "What runs on what", on screen: the map at the top of Settings → AI & Intelligence, the line
//! under "Answering" in the Minds panel, and each harness row's "Runs on". All three come from one
//! `runs_on::resolve`, so they cannot disagree.
//!
//! Published whenever the minds change (wire::harness::publish) and whenever the companion's own
//! address is resolved again (wire::ai_status::refresh).

use crate::runs_on::{self, MindFact, RunsOn};
use crate::App;
use yantrik_ui_slint::RunsOnRow;

/// What every mind runs on, right now.
pub(crate) fn current() -> Vec<RunsOn> {
    let minds: Vec<MindFact> = super::harness::host()
        .map(|h| h.list())
        .unwrap_or_default()
        .into_iter()
        .map(|e| MindFact { id: e.id, name: e.name, detail: e.detail, answering: e.active, builtin: e.builtin })
        .collect();
    let companion = super::ai_status::companion();
    runs_on::resolve(&minds, companion.as_ref())
}

/// The harness rows' line: "Runs on Ollama Cloud · deepseek-v4.1-flash · its own settings · as it
/// reported". Empty for a mind that is not attached, which says nothing about itself.
pub(crate) fn row_line(rows: &[RunsOn], id: &str) -> String {
    rows.iter()
        .find(|r| r.id == id && !r.builtin)
        .map(|r| format!("Runs on {} \u{b7} {}", r.runs_on(), r.source()))
        .unwrap_or_default()
}

pub(crate) fn publish(ui: &App) {
    let rows = current();
    let answering = rows
        .iter()
        .find(|r| r.answering)
        .map(|r| format!("{} \u{b7} {}", r.runs_on(), if r.reported { "as it reported" } else { "set by this desktop" }))
        .unwrap_or_default();
    ui.set_runs_on_summary(runs_on::summary(&rows).into());
    ui.set_runs_on_answering(answering.into());
    let model: Vec<RunsOnRow> = rows
        .iter()
        .map(|r| RunsOnRow {
            name: r.name.as_str().into(),
            state: r.state().into(),
            answering: r.answering,
            runs_on: r.runs_on().into(),
            source: r.source().into(),
        })
        .collect();
    if let Some(m) = crate::models::changed(ui.get_runs_on_rows(), model) {
        ui.set_runs_on_rows(m);
    }
}
