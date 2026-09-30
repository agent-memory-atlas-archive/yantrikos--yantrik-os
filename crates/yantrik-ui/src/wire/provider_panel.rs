//! Settings' Add/Edit Provider panel: Connect, pick a model, Save.
//!
//! Connect is one request (`provider_models::list_models`) that checks the key
//! and fetches the provider's own model list; the person picks from what the
//! provider actually serves, and Save stores that model with the provider.
//! A provider row's Models action opens the same panel on that provider and
//! re-lists, so a saved model can be changed later.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use yantrik_ml::ProviderDescriptor;

use crate::app_context::AppContext;
use crate::bridge::CompanionBridge;
use crate::wire::provider_catalogue::{self, auth_type_for, default_model_for, provider_preset};
use crate::wire::provider_models::{list_models, pick_model, ListedModel};
use crate::wire::settings::{
    push_ai_status_to_ui, push_providers_to_ui, uuid_short, ProviderStore, ProviderStoreEntry,
};
use crate::App;

/// What the last Connect listed, for the search box to filter.
#[derive(Default)]
struct Listing {
    models: Vec<ListedModel>,
}

/// Every Connect gets a number; an answer to an older one is dropped, so a
/// slow provider cannot overwrite the list of the one the person moved on to.
static GENERATION: AtomicU64 = AtomicU64::new(0);

pub(crate) fn wire(ui: &App, ctx: &AppContext, providers: Arc<Mutex<ProviderStore>>) {
    let listing = Arc::new(Mutex::new(Listing::default()));
    provider_catalogue::push_presets(ui);

    // A preset fills the form with the catalogue's name and URL; the last
    // provider's models and model no longer apply.
    let ui_weak = ui.as_weak();
    let l = listing.clone();
    ui.on_provider_preset_selected(move |preset| {
        let Some(ui) = ui_weak.upgrade() else { return };
        let (name, url) = provider_preset(&preset);
        tracing::info!(preset = %preset, name, url, "Provider preset selected");
        clear_models(&ui, &l);
        ui.set_settings_provider_form_name(name.into());
        ui.set_settings_provider_form_url(url.into());
        ui.set_settings_provider_form_model(SharedString::default());
        ui.set_settings_provider_test_result(SharedString::default());
    });

    // Add Provider: a clean form.
    let ui_weak = ui.as_weak();
    let l = listing.clone();
    ui.on_add_provider(move || {
        let Some(ui) = ui_weak.upgrade() else { return };
        clear_models(&ui, &l);
        ui.set_settings_provider_form_model(SharedString::default());
        ui.set_settings_provider_test_result(SharedString::default());
    });

    // Connect: check the key and fetch the list in one request.
    let ui_weak = ui.as_weak();
    let l = listing.clone();
    let ps = providers.clone();
    ui.on_connect_provider(move |preset, url, key| {
        let Some(ui) = ui_weak.upgrade() else { return };
        let key = (!key.is_empty()).then(|| key.to_string());
        start_connect(&ui, &l, &ps, preset.as_str(), url.as_str(), key);
    });

    // Search the listed models.
    let ui_weak = ui.as_weak();
    let l = listing.clone();
    ui.on_provider_model_search(move |query| {
        let Some(ui) = ui_weak.upgrade() else { return };
        let ids = l.lock().map(|l| matching(&l.models, query.as_str())).unwrap_or_default();
        ui.set_settings_provider_form_models(ModelRc::new(VecModel::from(ids)));
    });

    // A row's Models action: the panel, on that provider, re-listing.
    let ui_weak = ui.as_weak();
    let l = listing.clone();
    let ps = providers.clone();
    ui.on_edit_provider(move |id| {
        let Some(ui) = ui_weak.upgrade() else { return };
        let Some(entry) = ps.lock().ok().and_then(|s| s.entries.iter().find(|e| e.id == id.as_str()).cloned()) else {
            return;
        };
        clear_models(&ui, &l);
        ui.set_settings_editing_provider_id(entry.id.clone().into());
        ui.set_settings_selected_preset(entry.provider_type.clone().into());
        ui.set_settings_provider_form_name(entry.name.clone().into());
        ui.set_settings_provider_form_url(entry.base_url.clone().into());
        ui.set_settings_provider_form_model(entry.model.clone().into());
        ui.set_settings_provider_panel_open(true);
        // The key field stays empty; an empty field means "keep the saved key".
        start_connect(&ui, &l, &ps, &entry.provider_type, &entry.base_url, None);
    });

    // Save: a new provider, or the one being edited, with its chosen model.
    let ui_weak = ui.as_weak();
    let ps = providers;
    let bridge = ctx.bridge.clone();
    ui.on_save_provider(move |name, ptype, url, key, auth, model| {
        let Some(ui) = ui_weak.upgrade() else { return };
        let ptype = if ptype.is_empty() { "custom".to_string() } else { ptype.to_string() };
        // A known provider's auth comes from the catalogue, not the form.
        let auth = if ProviderDescriptor::by_id(&ptype).is_some() { auth_type_for(&ptype).to_string() } else { auth.to_string() };
        let editing = ui.get_settings_editing_provider_id().to_string();
        let form = Form {
            name: name.to_string(),
            provider_type: ptype,
            base_url: url.to_string(),
            api_key: (!key.is_empty()).then(|| key.to_string()),
            auth_type: auth,
            model: model.to_string(),
        };
        let Ok(mut store) = ps.lock() else { return };
        let before = store.clone();
        let saved = apply_form(&mut store, &editing, form);
        if store.save().is_err() {
            *store = before;
            return;
        }
        tracing::info!(
            provider_type = %saved.provider_type,
            model = %saved.model,
            edited = !editing.is_empty(),
            has_key = saved.api_key.is_some(),
            "Saving provider"
        );
        ui.set_settings_editing_provider_id(SharedString::default());
        push_providers_to_ui(&ui, &store);
        push_ai_status_to_ui(&ui, &store, bridge.is_online());
        if saved.is_primary {
            if !saved.model.is_empty() {
                ui.set_settings_llm_api_model(saved.model.clone().into());
            }
            reload_primary(&bridge, &saved);
        }
    });
}

/// What the panel's form holds when Save is pressed.
struct Form {
    name: String,
    provider_type: String,
    base_url: String,
    api_key: Option<String>,
    auth_type: String,
    model: String,
}

/// Write the form into the store: over the entry being edited (keeping its
/// saved key when the key field was left empty), or as a new entry, primary
/// when it is the first. Returns the entry as saved.
fn apply_form(store: &mut ProviderStore, editing: &str, form: Form) -> ProviderStoreEntry {
    if let Some(e) = store.entries.iter_mut().find(|e| !editing.is_empty() && e.id == editing) {
        e.name = form.name;
        e.provider_type = form.provider_type;
        e.base_url = form.base_url;
        if form.api_key.is_some() {
            e.api_key = form.api_key;
        }
        e.auth_type = form.auth_type;
        e.model = form.model;
        return e.clone();
    }
    let entry = ProviderStoreEntry {
        id: format!("{}-{}", form.provider_type.to_lowercase(), uuid_short()),
        name: form.name,
        provider_type: form.provider_type,
        base_url: form.base_url,
        api_key: form.api_key,
        auth_type: form.auth_type,
        is_primary: store.entries.is_empty(),
        is_fallback: false,
        model: form.model,
    };
    store.entries.push(entry.clone());
    entry
}

/// Hot-reload the companion onto `entry`: its saved model, or the
/// catalogue's guess when none was chosen.
pub(crate) fn reload_primary(bridge: &CompanionBridge, entry: &ProviderStoreEntry) {
    let model = if entry.model.is_empty() {
        default_model_for(&entry.provider_type).to_string()
    } else {
        entry.model.clone()
    };
    // Ollama's native address needs /v1 for the OpenAI-compatible client.
    let base_url = if entry.provider_type == "ollama" && !entry.base_url.contains("/v1") {
        format!("{}/v1", entry.base_url.trim_end_matches('/'))
    } else {
        entry.base_url.clone()
    };
    tracing::info!(provider = %entry.provider_type, model = %model, "Hot-reloading LLM with the primary provider");
    bridge.reload_llm(entry.provider_type.clone(), base_url, entry.api_key.clone(), model);
}

fn clear_models(ui: &App, listing: &Arc<Mutex<Listing>>) {
    GENERATION.fetch_add(1, Ordering::SeqCst);
    if let Ok(mut l) = listing.lock() {
        l.models.clear();
    }
    ui.set_settings_provider_form_models(ModelRc::new(VecModel::from(Vec::<SharedString>::new())));
    ui.set_settings_provider_form_model_total(0);
    ui.set_settings_provider_form_connected(false);
}

/// List `url`'s models on a thread and fill the picker with the answer.
/// With no key typed while editing, the saved provider's key is used.
fn start_connect(
    ui: &App,
    listing: &Arc<Mutex<Listing>>,
    providers: &Arc<Mutex<ProviderStore>>,
    preset: &str,
    url: &str,
    key: Option<String>,
) {
    clear_models(ui, listing);
    let generation = GENERATION.load(Ordering::SeqCst);
    let editing = ui.get_settings_editing_provider_id().to_string();
    let saved = providers
        .lock()
        .ok()
        .and_then(|s| s.entries.iter().find(|e| !editing.is_empty() && e.id == editing).cloned());
    let key = key.or_else(|| saved.as_ref().and_then(|e| e.api_key.clone()));
    let auth = match (ProviderDescriptor::by_id(preset), &saved) {
        (Some(_), _) => auth_type_for(preset).to_string(),
        (None, Some(e)) => e.auth_type.clone(),
        (None, None) => "bearer".to_string(),
    };
    // Keep the model already chosen when the provider still serves it,
    // otherwise the catalogue's default, otherwise the list's first.
    let chosen = ui.get_settings_provider_form_model().to_string();
    let catalogue_default = ProviderDescriptor::by_id(preset).map_or("", |p| p.default_model);
    ui.set_settings_provider_test_result("testing".into());

    let url = url.to_string();
    let weak = ui.as_weak();
    let listing = listing.clone();
    std::thread::spawn(move || {
        let result = list_models(&url, key.as_deref(), &auth);
        let _ = slint::invoke_from_event_loop(move || {
            if GENERATION.load(Ordering::SeqCst) != generation {
                return;
            }
            let Some(ui) = weak.upgrade() else { return };
            match result {
                Ok(models) => {
                    let pick = pick_model(&models, &[&chosen, catalogue_default]).unwrap_or("").to_string();
                    ui.set_settings_provider_form_model_total(models.len() as i32);
                    ui.set_settings_provider_form_models(ModelRc::new(VecModel::from(matching(&models, ""))));
                    ui.set_settings_provider_form_model(pick.into());
                    ui.set_settings_provider_form_connected(true);
                    ui.set_settings_provider_test_result("success".into());
                    if let Ok(mut l) = listing.lock() {
                        l.models = models;
                    }
                }
                Err(e) => ui.set_settings_provider_test_result(e.to_string().into()),
            }
        });
    });
}

/// The ids whose id or name contains `query`, ignoring case.
fn matching(models: &[ListedModel], query: &str) -> Vec<SharedString> {
    let q = query.trim().to_lowercase();
    models
        .iter()
        .filter(|m| q.is_empty() || m.id.to_lowercase().contains(&q) || m.name.to_lowercase().contains(&q))
        .map(|m| SharedString::from(m.id.as_str()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(id: &str, name: &str) -> ListedModel {
        ListedModel { id: id.into(), name: name.into(), size_bytes: 0 }
    }

    fn form(model: &str, key: Option<&str>) -> Form {
        Form {
            name: "OpenRouter".into(),
            provider_type: "openrouter".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            api_key: key.map(str::to_string),
            auth_type: "bearer".into(),
            model: model.into(),
        }
    }

    #[test]
    fn search_matches_ids_and_names_ignoring_case() {
        let models = [model("anthropic/claude-sonnet-5-5", "Claude Sonnet 5.5"), model("openai/gpt-4o", "GPT-4o")];
        assert_eq!(matching(&models, "SONNET"), ["anthropic/claude-sonnet-5-5"]);
        assert_eq!(matching(&models, "gpt"), ["openai/gpt-4o"]);
        assert_eq!(matching(&models, "").len(), 2);
    }

    #[test]
    fn a_new_provider_is_saved_with_its_model_and_the_first_is_primary() {
        let mut store = ProviderStore::default();
        let saved = apply_form(&mut store, "", form("openai/gpt-4o", Some("sk-or-1")));
        assert!(saved.is_primary);
        assert_eq!(saved.model, "openai/gpt-4o");
        let second = apply_form(&mut store, "", form("x", None));
        assert!(!second.is_primary);
        assert_eq!(store.entries.len(), 2);
    }

    #[test]
    fn editing_changes_the_model_and_keeps_the_key_when_none_is_typed() {
        let mut store = ProviderStore::default();
        let first = apply_form(&mut store, "", form("openai/gpt-4o", Some("sk-or-1")));
        let edited = apply_form(&mut store, &first.id, form("anthropic/claude-sonnet-5-5", None));
        assert_eq!(store.entries.len(), 1, "an edit is not a second provider");
        assert_eq!(edited.model, "anthropic/claude-sonnet-5-5");
        assert_eq!(edited.api_key.as_deref(), Some("sk-or-1"));
        assert!(edited.is_primary);
    }
}
