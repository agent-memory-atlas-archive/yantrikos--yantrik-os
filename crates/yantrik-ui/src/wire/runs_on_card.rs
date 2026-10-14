//! "What runs on what", on screen: the map at the top of Settings → AI & Intelligence, the line
//! under "Answering" in the Minds panel, each harness row's "Runs on", and the line under the chat
//! composer. All of them come from one `runs_on::resolve`, so they cannot disagree.
//!
//! Published whenever the minds change (wire::harness::publish) and whenever the companion's own
//! address is resolved again (wire::ai_status::refresh).

use std::sync::Mutex;

use crate::runs_on::{self, GatewayRoute, MindFact, RunsOn};
use crate::App;
use yantrik_ui_slint::{InUseRow, RunsOnRow};

/// The addresses of the providers saved in Settings, kept up to date by push_providers_to_ui, so
/// the Providers list can tell what is in use but not saved without reading the file again.
static SAVED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// What every mind runs on, right now.
pub(crate) fn current() -> Vec<RunsOn> {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_else(|| "/root".into());
    let minds: Vec<MindFact> = super::harness::host()
        .map(|h| h.list())
        .unwrap_or_default()
        .into_iter()
        .map(|e| {
            let gateway = (!e.builtin && crate::provider_handoff::uses_gateway(&home, &e.id)).then(|| gateway_route(&e.id));
            MindFact { id: e.id, name: e.name, detail: e.detail, answering: e.active, builtin: e.builtin, gateway }
        })
        .collect();
    let companion = super::ai_status::companion();
    runs_on::resolve(&minds, companion.as_ref())
}

/// Where the gateway sends `harness`'s next call, decided as the gateway decides it: the model its
/// `picked` resolves to (`harness_provider::starting_model`), on that model's account, unless the
/// mind carries private context to an account the person keeps it off, when the private lane
/// answers instead (`yantrik_gateway::policy::decide`). An empty route when there is no model.
fn gateway_route(harness: &str) -> GatewayRoute {
    use crate::ai_accounts::{self, consent};
    use yantrik_gateway::route;
    let Some(picked) = super::harness_provider::starting_model(harness).ok().and_then(|m| route::parse(&m)) else {
        return GatewayRoute::default();
    };
    let accounts = ai_accounts::accounts();
    let Some(account) = accounts.iter().find(|a| a.id == picked.account) else { return GatewayRoute::default() };
    let carries_private = crate::provider_handoff::adapter_for(harness).map(|a| a.private_context()).unwrap_or(true);
    let allowed = consent::load(&consent::path());
    let laned = carries_private && !account.local && crate::gateway::sharing(account, &allowed) == consent::Sharing::Lane;
    let lane = laned
        .then(|| consent::lane(&allowed, &accounts).and_then(|l| route::parse(&l)))
        .flatten()
        .and_then(|l| accounts.iter().find(|a| a.id == l.account).map(|a| (a, l.model)));
    let (account, model) = lane.unwrap_or((account, picked.model));
    GatewayRoute { account: account.name.clone(), base_url: account.base_url.clone(), model, provider_type: account.provider_type.clone() }
}

/// The saved providers changed: remember their addresses and show the lists again.
pub(crate) fn set_saved(ui: &App, urls: Vec<String>) {
    *SAVED.lock().unwrap_or_else(|e| e.into_inner()) = urls;
    publish(ui);
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
    // The chat composer's line: where words typed to the answering mind go, or "" when that is
    // not known (the composer then says nothing rather than guess).
    let destination = rows.iter().find(|r| r.answering).map(|r| r.destination()).unwrap_or_default();
    if ui.get_lens_destination() != destination.as_str() {
        ui.set_lens_destination(destination.into());
    }
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

    let saved = SAVED.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let companion = super::ai_status::companion();
    let in_use: Vec<InUseRow> = runs_on::in_use_not_saved(&rows, companion.as_ref(), &saved)
        .into_iter()
        .map(|u| InUseRow {
            label: u.label.into(),
            used_by: format!("Used by {}", u.used_by.join(", ")).into(),
            where_set: u.where_set.into(),
            models: u.models.join(", ").into(),
            preset: u.preset.into(),
            base_url: u.base_url.into(),
            model: u.model.into(),
        })
        .collect();
    if let Some(m) = crate::models::changed(ui.get_in_use_providers(), in_use) {
        ui.set_in_use_providers(m);
    }
}
