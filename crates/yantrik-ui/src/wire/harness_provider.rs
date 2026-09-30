//! Settings → Harnesses: "Use a provider" and "Revert provider" on a harness row, and the same
//! two for agents (`assign_provider`, `revert_provider` in control.rs).
//!
//! The card the person answers is the plan itself (`provider_handoff::Plan::card`): Apply writes
//! exactly the plan that was shown, held here between the two presses, so what was read is what
//! is done. The rows pick up the change on the page's own refresh.

use std::path::PathBuf;
use std::sync::Mutex;

use slint::ComponentHandle;

use crate::provider_handoff::{self, Plan};
use crate::wire::settings::ProviderStore;
use crate::App;

/// The plan on the card, between showing it and Apply.
static PENDING: Mutex<Option<Plan>> = Mutex::new(None);

fn home() -> PathBuf {
    std::env::var("HOME").map(PathBuf::from).unwrap_or_default()
}

/// The plan for giving `harness` the saved provider `provider_id`.
pub(crate) fn plan(harness: &str, provider_id: &str) -> Result<Plan, String> {
    let adapter = provider_handoff::adapter_for(harness)
        .ok_or_else(|| format!("{harness} cannot be given a provider from here yet — it keeps its own settings"))?;
    let store = ProviderStore::load();
    let provider = store
        .entries
        .iter()
        .find(|e| e.id == provider_id || e.name.eq_ignore_ascii_case(provider_id))
        .ok_or_else(|| format!("there is no saved provider `{provider_id}`"))?;
    adapter.plan(&home(), provider)
}

/// For agents: plan and apply in one call. The action is graded sensitive, so the person has
/// already answered the approval card by the time this runs. Answers with the card's text.
pub(crate) fn assign(harness: &str, provider_id: &str) -> Result<String, String> {
    let plan = plan(harness, provider_id)?;
    let home = home();
    let card = plan.card(&home);
    provider_handoff::apply(&home, &plan)?;
    Ok(card)
}

pub(crate) fn revert(harness: &str) -> Result<(), String> {
    provider_handoff::revert(&home(), harness)
}

pub(crate) fn wire(ui: &App) {
    {
        let weak = ui.as_weak();
        ui.on_plan_provider(move |harness, provider| {
            let Some(ui) = weak.upgrade() else { return };
            match plan(&harness, &provider) {
                Ok(p) => {
                    ui.set_settings_handoff_card_title(format!("Give {} {}?", p.harness_name, p.provider_name).into());
                    ui.set_settings_handoff_card_body(p.card(&home()).into());
                    *PENDING.lock().unwrap_or_else(|e| e.into_inner()) = Some(p);
                    ui.set_settings_handoff_card_open(true);
                }
                Err(e) => ui.set_harness_error(e.into()),
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_apply_provider(move || {
            let Some(ui) = weak.upgrade() else { return };
            ui.set_settings_handoff_card_open(false);
            ui.set_settings_assigning_harness("".into());
            let Some(plan) = PENDING.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
            if let Err(e) = provider_handoff::apply(&home(), &plan) {
                ui.set_harness_error(e.into());
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_cancel_provider(move || {
            let Some(ui) = weak.upgrade() else { return };
            ui.set_settings_handoff_card_open(false);
            PENDING.lock().unwrap_or_else(|e| e.into_inner()).take();
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_revert_provider(move |harness| {
            let Some(ui) = weak.upgrade() else { return };
            if let Err(e) = revert(&harness) {
                ui.set_harness_error(e.into());
            }
        });
    }
}
