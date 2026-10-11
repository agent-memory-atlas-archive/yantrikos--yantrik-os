//! The first run, wired to what is true (design/experience-final-2026-10-10.md, "First run,
//! minute by minute"; the screens are `onboarding_first_run.slint`).
//!
//! What this file decides, and nothing else draws:
//! - **The cards.** A few named cards, each with one plain line and its honest requirement, and
//!   behind them every provider the catalogue knows (`yantrik_ml::KNOWN_PROVIDERS`), never a
//!   shortened list. "On this computer" is a card only when a model is actually here (or the
//!   private-lane backend says one can be downloaded), and it says its size.
//! - **The check rows.** Not checked yet → Checking (started h:mm) → Passed / Needs attention:
//!   <reason>, from what each check returned. No percentage, no step counter pretending to know.
//! - **The sharing choice.** Share / Private lane / Off, recorded through #675's onboarding calls
//!   (`ai_accounts::onboarding`: `agree_to_share`, `keep_personal_things_on`), and the model the
//!   connection was proven with becomes the one pick (`choose_model`).
//!
//! The provider is chosen once: a choice made through these screens in the installer (carried to
//! the new home in `first-run.json`) skips "Who should do the thinking?" and "What your minds
//! share" at first boot. A provider merely present (the `--with-llm` image's own account) does not.
//!
//! Functions not built yet sit behind a small trait with a stub marked `STUB(#…)` that never
//! reports a pass it did not see:
//! - [`Proofs`], the memory and desktop proofs and the list of minds (#677,
//!   feat/677-memory-and-pairing);
//! - [`Lane`], browser sign-in and a downloadable local model (no branch owns them yet).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use yantrik_ml::{ProviderDescriptor, ProviderKind, KNOWN_PROVIDERS};

use crate::app_context::AppContext;
use crate::wire::provider_catalogue::{auth_type_for, default_model_for};
use crate::wire::settings::{ProviderStore, ProviderStoreEntry};
use crate::{App, FirstRunCheck, FirstRunMind, FirstRunProvider, TextScale};

// ── The cards ──────────────────────────────────────────────────────────────────────────────

/// The named cards, in the order shown. Everything else is one press away, under "Show every
/// provider".
const FEATURED: [&str; 6] = ["anthropic", "openai", "gemini", "deepseek", "openrouter", "nvidia-nim"];

/// The bundled model the ISO puts on the machine (deploy/yantrik-os/build-debian-iso.sh), served
/// by llama-server on the loopback port below.
const LOCAL_MODEL_DIR: &str = "/opt/yantrik/models/llm";
const LOCAL_ENDPOINT: &str = "http://127.0.0.1:8341/v1";
const THIS_COMPUTER: &str = "this-computer";
/// Settings, and its Harnesses section (wire/harness.rs), where Add another mind goes.
const SETTINGS_SCREEN: i32 = 7;
const HARNESSES_SECTION: i32 = 8;

/// A model on this machine, or one the private-lane backend can download.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalModel {
    pub bytes: u64,
    /// On the disk now; otherwise downloadable.
    pub present: bool,
}

/// What a person sees on a card. `Card` → `FirstRunProvider` is a field-for-field copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Card {
    pub id: String,
    pub name: String,
    pub line: String,
    pub needs: String,
    pub group: String,
    pub key_placeholder: String,
    pub browser_sign_in: bool,
    pub key_help: bool,
}

/// The name on the card. Anthropic's models are known by their own name.
fn card_name(p: &ProviderDescriptor) -> String {
    match p.id {
        "anthropic" => "Claude".into(),
        _ => p.display_name.into(),
    }
}

/// The one plain line for the named cards; the catalogue's own description for the rest.
fn plain_line(p: &ProviderDescriptor) -> String {
    match p.id {
        "anthropic" => "online, careful with words".into(),
        "openai" => "online, a good all-rounder".into(),
        "gemini" => "online, reads long documents".into(),
        "deepseek" => "online, strong at reasoning, low cost".into(),
        "openrouter" => "online, one account for many models".into(),
        "nvidia-nim" => "online, many open models".into(),
        _ if p.kind == ProviderKind::Local => "on a computer you run it on".into(),
        _ => p.description.trim_end_matches('.').into(),
    }
}

/// The honest requirement. A cloud needs an account with its maker; a runtime the person hosts
/// needs to be running; neither is disguised as effortless.
fn needs(p: &ProviderDescriptor) -> String {
    match p.kind {
        ProviderKind::Local => format!("needs {} running on this computer or your network", p.short_name),
        _ if p.key_placeholder.is_empty() => "needs no key".into(),
        _ => {
            let who = match p.id {
                "anthropic" => "Anthropic",
                "gemini" => "Google",
                "nvidia-nim" => "NVIDIA",
                _ => p.display_name,
            };
            let a = if who.starts_with(['A', 'E', 'I', 'O', 'U']) { "an" } else { "a" };
            format!("needs {a} {who} account")
        }
    }
}

/// Where the provider shows a person their key. Only pages known to exist; a provider without
/// one gets no "Show me where to find it" rather than a guess.
pub fn key_page(id: &str) -> Option<&'static str> {
    Some(match id {
        "anthropic" => "https://console.anthropic.com/settings/keys",
        "openai" => "https://platform.openai.com/api-keys",
        "gemini" => "https://aistudio.google.com/apikey",
        "deepseek" => "https://platform.deepseek.com/api_keys",
        "openrouter" => "https://openrouter.ai/settings/keys",
        "nvidia-nim" => "https://build.nvidia.com/settings/api-keys",
        "groq" => "https://console.groq.com/keys",
        "mistral" => "https://console.mistral.ai/api-keys",
        "huggingface" => "https://huggingface.co/settings/tokens",
        "together" => "https://api.together.ai/settings/api-keys",
        "cohere" => "https://dashboard.cohere.com/api-keys",
        "ollama-cloud" => "https://ollama.com/settings/keys",
        _ => return None,
    })
}

/// Gigabytes, one decimal, the way a person reads a download: "2.5 GB".
pub fn gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

fn card(p: &ProviderDescriptor, lane: &dyn Lane) -> Card {
    Card {
        id: p.id.into(),
        name: card_name(p),
        line: plain_line(p),
        needs: needs(p),
        group: match p.kind {
            ProviderKind::Cloud => "cloud",
            ProviderKind::Aggregator => "aggregator",
            ProviderKind::Local => "local",
        }
        .into(),
        key_placeholder: p.key_placeholder.into(),
        browser_sign_in: lane.browser_sign_in(p.id),
        key_help: key_page(p.id).is_some(),
    }
}

/// "On this computer", only for a model that is here or can be fetched, with its size.
pub fn this_computer_card(local: &LocalModel) -> Card {
    Card {
        id: THIS_COMPUTER.into(),
        name: "On this computer".into(),
        line: "private, slower".into(),
        needs: if local.present {
            format!("already here ({})", gb(local.bytes))
        } else {
            format!("downloads {}", gb(local.bytes))
        },
        group: THIS_COMPUTER.into(),
        key_placeholder: String::new(),
        browser_sign_in: false,
        key_help: false,
    }
}

pub fn featured_cards(local: Option<&LocalModel>, lane: &dyn Lane) -> Vec<Card> {
    let mut cards: Vec<Card> = FEATURED
        .iter()
        .filter_map(|id| ProviderDescriptor::by_id(id))
        .map(|p| card(p, lane))
        .collect();
    if let Some(l) = local {
        cards.push(this_computer_card(l));
    }
    cards
}

/// Every provider the catalogue knows, in its order: the full selection, never a simplified one.
pub fn every_card(lane: &dyn Lane) -> Vec<Card> {
    KNOWN_PROVIDERS.iter().filter(|p| !p.id.is_empty()).map(|p| card(p, lane)).collect()
}

fn find_card(id: &str, local: Option<&LocalModel>, lane: &dyn Lane) -> Option<Card> {
    if id == THIS_COMPUTER {
        return local.map(this_computer_card);
    }
    ProviderDescriptor::by_id(id).map(|p| card(p, lane))
}

/// A GGUF in the bundled model directory, by its size on disk.
pub fn local_model_in(dir: &Path) -> Option<LocalModel> {
    let bytes = std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "gguf"))
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .max()?;
    Some(LocalModel { bytes, present: true })
}

// ── The check rows ─────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckKind {
    Answer,
    Remember,
    Desktop,
}

impl CheckKind {
    pub const ALL: [CheckKind; 3] = [CheckKind::Answer, CheckKind::Remember, CheckKind::Desktop];
    pub fn id(self) -> &'static str {
        match self {
            CheckKind::Answer => "answer",
            CheckKind::Remember => "remember",
            CheckKind::Desktop => "desktop",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            CheckKind::Answer => "Can it answer?",
            CheckKind::Remember => "Can it remember?",
            CheckKind::Desktop => "Can it reach the desktop?",
        }
    }
}

/// What a check found: the proof for Details, or the plain reason it needs attention.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Proof {
    Passed { details: String },
    Attention { reason: String, details: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckState {
    NotChecked,
    /// Started at this wall-clock time, "2:16".
    Checking(String),
    Done(Proof),
}

impl CheckState {
    pub fn state(&self) -> &'static str {
        match self {
            CheckState::NotChecked => "not-checked",
            CheckState::Checking(_) => "checking",
            CheckState::Done(Proof::Passed { .. }) => "passed",
            CheckState::Done(Proof::Attention { .. }) => "attention",
        }
    }
    pub fn words(&self) -> String {
        match self {
            CheckState::NotChecked => "Not checked yet".into(),
            CheckState::Checking(at) => format!("Checking (started {at})"),
            CheckState::Done(Proof::Passed { .. }) => "Passed".into(),
            CheckState::Done(Proof::Attention { reason, .. }) => format!("Needs attention: {reason}"),
        }
    }
    fn details(&self) -> String {
        match self {
            CheckState::Done(Proof::Passed { details } | Proof::Attention { details, .. }) => details.clone(),
            _ => String::new(),
        }
    }
}

/// "2:16", the clock a person reads: hour without a leading zero, no seconds.
pub fn clock(t: chrono::DateTime<chrono::Local>) -> String {
    t.format("%-I:%M").to_string()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MindRows {
    pub id: String,
    pub name: String,
    pub checks: Vec<(CheckKind, CheckState)>,
}

impl MindRows {
    pub fn new(id: &str, name: &str) -> Self {
        Self { id: id.into(), name: name.into(), checks: CheckKind::ALL.iter().map(|k| (*k, CheckState::NotChecked)).collect() }
    }
    fn done(&self) -> bool {
        self.checks.iter().all(|(_, s)| matches!(s, CheckState::Done(_)))
    }
    fn ok(&self) -> bool {
        self.checks.iter().all(|(_, s)| matches!(s, CheckState::Done(Proof::Passed { .. })))
    }
    /// The one line on "Your minds": what was seen, never what was assumed.
    pub fn words(&self) -> String {
        if self.ok() {
            return "Ready".into();
        }
        if let Some(r) = self.checks.iter().find_map(|(_, s)| match s {
            CheckState::Done(Proof::Attention { reason, .. }) => Some(reason.clone()),
            _ => None,
        }) {
            return format!("Needs attention: {r}");
        }
        if self.checks.iter().any(|(_, s)| matches!(s, CheckState::Checking(_))) {
            return "Checking".into();
        }
        "Not checked yet".into()
    }
    fn set(&mut self, kind: CheckKind, state: CheckState) {
        if let Some(slot) = self.checks.iter_mut().find(|(k, _)| *k == kind) {
            slot.1 = state;
        }
    }
    fn to_ui(&self) -> FirstRunMind {
        let checks: Vec<FirstRunCheck> = self
            .checks
            .iter()
            .map(|(k, s)| FirstRunCheck {
                id: k.id().into(),
                label: k.label().into(),
                state: s.state().into(),
                words: s.words().into(),
                details: s.details().into(),
            })
            .collect();
        FirstRunMind {
            id: self.id.clone().into(),
            name: self.name.clone().into(),
            words: self.words().into(),
            ok: self.ok(),
            checks: ModelRc::new(VecModel::from(checks)),
        }
    }
}

// ── What the minds share ───────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sharing {
    /// The person's context goes to the chosen provider with their consent.
    Share,
    /// Calls that carry it go to a model on this machine instead.
    PrivateLane,
    /// Neither: such calls are refused, with the fix named. Nothing is sent quietly.
    Off,
}

impl Sharing {
    pub fn parse(s: &str) -> Option<Sharing> {
        match s {
            "share" => Some(Sharing::Share),
            "private-lane" => Some(Sharing::PrivateLane),
            "off" => Some(Sharing::Off),
            _ => None,
        }
    }
}

/// "What's shared?", in plain words.
pub const SHARED_ITEMS: [&str; 4] = [
    "Your name",
    "Your town",
    "The app you're in",
    "What your minds remember about you",
];

/// The private-lane offer under the sharing sentence: its size, and that answers may be slower.
/// Empty when there is no model here and none can be fetched; the screen then says so.
pub fn private_lane_words(local: Option<&LocalModel>) -> String {
    match local {
        Some(l) if l.present => format!("Uses the {} model already on this computer. Answers about you may be slower.", gb(l.bytes)),
        Some(l) => format!("Downloads a {} model to this computer. Answers about you may be slower.", gb(l.bytes)),
        None => String::new(),
    }
}

/// The AI-accounts id of a saved provider: its name as a slug, as `ai_accounts::account` makes it.
pub fn account_id(entry: &ProviderStoreEntry) -> String {
    let s = crate::ai_accounts::account::slug(&entry.name);
    if s.is_empty() { crate::ai_accounts::account::slug(&entry.provider_type) } else { s }
}

/// The local model that keeps personal things here: the bundled one when it is listed (it is on
/// this machine, not only the network), else the first local model.
pub fn lane_model(choices: &[(String, String)]) -> Option<String> {
    choices
        .iter()
        .find(|(_, url)| url.contains("127.0.0.1") || url.contains("localhost"))
        .or_else(|| choices.first())
        .map(|(id, _)| id.clone())
}

/// Share / Private lane / Off, through #675's onboarding calls. A private lane with no local
/// model to keep it is refused with the reason, and sharing stays off: nothing is sent quietly.
fn apply_sharing(sharing: Sharing) -> Result<(), String> {
    use crate::ai_accounts::onboarding as ob;
    match sharing {
        Sharing::Share => ob::agree_to_share(true),
        Sharing::Off => ob::agree_to_share(false),
        Sharing::PrivateLane => {
            ob::agree_to_share(false)?;
            let accounts = crate::ai_accounts::accounts();
            let choices: Vec<(String, String)> = ob::private_lane_choices()
                .into_iter()
                .map(|m| {
                    let url = accounts.iter().find(|a| a.id == m.account).map(|a| a.base_url.clone()).unwrap_or_default();
                    (m.id, url)
                })
                .collect();
            let lane = lane_model(&choices).ok_or("no model on this computer is ready to keep personal things yet")?;
            ob::keep_personal_things_on(&lane)
        }
    }
}

// ── What other branches provide ───────────────────────────────────────────────────────────

/// The minds and their three proofs (#677: memory write-then-recall, an attach seen on the
/// harness socket). The answer proof is ours: a one-token call to the chosen account.
pub trait Proofs: Send + Sync {
    /// Every mind to check, `(id, name)`.
    fn minds(&self) -> Vec<(String, String)>;
    /// Write a temporary note to the mind's memory, read it back, delete it.
    fn remember(&self, mind: &str) -> Proof;
    /// See the mind attached to the desktop's harness socket.
    fn desktop(&self, mind: &str) -> Proof;
}

/// STUB(#677): replace with the three-proofs function from feat/677-memory-and-pairing. It never
/// reports a pass: until the proof exists, the row says it could not be checked, and why.
pub struct PendingProofs;

impl Proofs for PendingProofs {
    fn minds(&self) -> Vec<(String, String)> {
        // The built-in companion is the one mind every install has; the harness minds come with
        // #677's list.
        vec![("companion".into(), "Yantrik Companion".into())]
    }
    fn remember(&self, _mind: &str) -> Proof {
        Proof::Attention {
            reason: "this build cannot check memory yet".into(),
            details: "memory proof not built in (waiting on #677: write a note, recall it, delete it)".into(),
        }
    }
    fn desktop(&self, _mind: &str) -> Proof {
        Proof::Attention {
            reason: "this build cannot check the desktop link yet".into(),
            details: "pairing proof not built in (waiting on #677: an attach seen on the harness socket)".into(),
        }
    }
}

/// Browser sign-in and a downloadable local model: AI accounts' backend, not built yet.
pub trait Lane: Send + Sync {
    /// The provider lets a person sign in with the browser instead of pasting a key.
    fn browser_sign_in(&self, provider: &str) -> bool;
    /// Sign in with the browser; the key arrives in AI accounts. Returns the plain outcome.
    fn sign_in(&self, provider: &str) -> Result<(), String>;
    /// A model the private lane can download, when none is on the machine.
    fn downloadable(&self) -> Option<LocalModel>;
}

/// STUB(#675 follow-up): no browser sign-in and nothing to download. Every answer here is the
/// honest "not available", so no card offers what cannot happen.
pub struct PendingLane;

impl Lane for PendingLane {
    fn browser_sign_in(&self, _provider: &str) -> bool {
        false
    }
    fn sign_in(&self, _provider: &str) -> Result<(), String> {
        Err("browser sign-in is not available in this build; paste a key instead".into())
    }
    fn downloadable(&self) -> Option<LocalModel> {
        None
    }
}

// ── What the first run remembers ──────────────────────────────────────────────────────────

/// `~/.config/yantrik/first-run.json`: the text size and the sharing choice. No key.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Saved {
    pub text_scale: f32,
    /// "share", "private-lane", "off" or "local", for #675 to read when it routes.
    pub sharing: String,
    /// The one-time hint about What really happened has been shown.
    pub witness_hint_shown: bool,
    /// A provider was chosen and answered through these screens (in the installer or at first
    /// boot): the first boot does not ask again.
    pub provider_chosen: bool,
}

pub fn saved_path() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/root"));
    home.join(".config/yantrik/first-run.json")
}

pub fn load_saved(path: &Path) -> Saved {
    std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn update_saved(path: &Path, f: impl FnOnce(&mut Saved)) {
    let mut s = load_saved(path);
    f(&mut s);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string_pretty(&s) {
        if let Err(e) = std::fs::write(path, text + "\n") {
            tracing::warn!(error = %e, "first run: could not save the choice");
        }
    }
}

// ── The wiring ─────────────────────────────────────────────────────────────────────────────

fn to_ui(c: &Card) -> FirstRunProvider {
    FirstRunProvider {
        id: c.id.clone().into(),
        name: c.name.clone().into(),
        line: c.line.clone().into(),
        needs: c.needs.clone().into(),
        group: c.group.clone().into(),
        key_placeholder: c.key_placeholder.clone().into(),
        browser_sign_in: c.browser_sign_in,
        key_help: c.key_help,
    }
}

fn model_of<T: Clone + 'static>(v: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(v))
}

/// Save `entry` as the one primary provider, as Settings and the old onboarding do.
fn save_primary(entry: ProviderStoreEntry) -> Result<(), String> {
    let mut store = ProviderStore::load();
    store.entries.retain(|e| !e.is_primary && e.id != entry.id);
    store.entries.push(entry);
    store.save()
}

/// A connection is proven by the smallest real answer: a one-token call with the model the
/// person configured, or, when they named none, the catalogue's default if the provider serves
/// it. Returns the model it answered with (for Details only), or the plain reason.
fn prove_answer(entry: &ProviderStoreEntry) -> Result<(String, i32), String> {
    use crate::wire::provider_models::{check_key, pick_model, KeyCheck};
    let started = std::time::Instant::now();
    let model = if !entry.model.is_empty() {
        entry.model.clone()
    } else {
        let listed = crate::wire::settings::test_provider_connection(&entry.base_url, entry.api_key.as_deref(), &entry.auth_type);
        if !listed.success {
            return Err(listed.message);
        }
        pick_model(&listed.models, &[default_model_for(&entry.provider_type)]).unwrap_or("").to_string()
    };
    if model.is_empty() {
        return Err("it answered, but offers no model to talk to".into());
    }
    match check_key(&entry.base_url, entry.api_key.as_deref().unwrap_or(""), &model) {
        KeyCheck::Works => Ok((model, started.elapsed().as_millis() as i32)),
        KeyCheck::Refused => Err("the key was refused".into()),
        KeyCheck::Unconfirmed(why) => Err(why),
    }
}

struct State {
    rows: Vec<MindRows>,
    local: Option<LocalModel>,
}

pub fn wire(ui: &App, _ctx: &AppContext) {
    let proofs: Arc<dyn Proofs> = Arc::new(PendingProofs);
    let lane: Arc<dyn Lane> = Arc::new(PendingLane);
    let saved = saved_path();

    // The text size the person chose, on every later start too.
    let s = load_saved(&saved).text_scale;
    if s >= 1.0 {
        ui.global::<TextScale>().set_factor(s);
    }

    let local = local_model_in(Path::new(LOCAL_MODEL_DIR)).or_else(|| lane.downloadable());
    let state = Arc::new(Mutex::new(State {
        rows: proofs.minds().iter().map(|(id, n)| MindRows::new(id, n)).collect(),
        local: local.clone(),
    }));

    ui.set_fr_featured(model_of(featured_cards(local.as_ref(), lane.as_ref()).iter().map(to_ui).collect()));
    ui.set_fr_every_provider(model_of(every_card(lane.as_ref()).iter().map(to_ui).collect()));
    ui.set_fr_shared_items(model_of(SHARED_ITEMS.iter().map(|s| SharedString::from(*s)).collect()));
    let lane_listed = !crate::ai_accounts::onboarding::private_lane_choices().is_empty();
    ui.set_fr_private_lane_words(match (&local, lane_listed) {
        (Some(_), _) => private_lane_words(local.as_ref()),
        (None, true) => "Uses a model on this computer or your network. Answers about you may be slower.".into(),
        (None, false) => String::new(),
    }.into());
    ui.set_fr_minds(model_of(lock(&state).rows.iter().map(MindRows::to_ui).collect()));
    // Chosen once: a provider already saved (the installer carries the live session's choice
    // into this account's providers.yaml) skips the two AI screens.
    if let Some(p) = ProviderStore::load().primary().filter(|_| load_saved(&saved).provider_chosen) {
        let name = card_name_for(&p.provider_type, &p.name);
        ui.set_fr_installer_chose(true);
        ui.set_fr_chosen_name(name.clone().into());
        ui.set_fr_installer_choice(name.into());
    }

    // Back from the provider screens the installer drew: its "Thinking" row says what was chosen
    // (and connected), and the install carries it over (wire/installer.rs).
    ui.on_fr_installer_done({
        let weak = ui.as_weak();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let chosen = ui.get_fr_connect_status() == "connected";
            let name = match ProviderStore::load().primary() {
                Some(p) if chosen => card_name_for(&p.provider_type, &p.name),
                _ => String::new(),
            };
            ui.set_fr_installer_choice(name.into());
        }
    });

    ui.on_fr_text_scale_changed({
        let saved = saved.clone();
        move |f| update_saved(&saved, |s| s.text_scale = f)
    });

    ui.on_fr_read_aloud(|text| speak(text.to_string()));

    ui.on_fr_choose_provider({
        let weak = ui.as_weak();
        let lane = lane.clone();
        let state = state.clone();
        move |id| {
            let Some(ui) = weak.upgrade() else { return };
            let local = lock(&state).local.clone();
            let Some(c) = find_card(&id, local.as_ref(), lane.as_ref()) else { return };
            ui.set_fr_chosen_id(c.id.clone().into());
            ui.set_fr_chosen_name(c.name.clone().into());
            ui.set_fr_chosen_key_placeholder(c.key_placeholder.clone().into());
            ui.set_fr_chosen_browser_sign_in(c.browser_sign_in);
            ui.set_fr_chosen_key_help(c.key_help);
            ui.set_fr_key_input("".into());
            ui.set_fr_connect_status("".into());
            ui.set_fr_connect_words("".into());
            let mode = if c.id == THIS_COMPUTER {
                "this-computer"
            } else if c.browser_sign_in {
                "browser"
            } else if c.key_placeholder.is_empty() {
                // A runtime the person hosts: no key, so connect straight away.
                connect(&ui, ProviderStoreEntry {
                    id: format!("{}-onboarding", c.id),
                    name: crate::wire::provider_catalogue::provider_preset(&c.id).0.into(),
                    provider_type: c.id.clone(),
                    base_url: crate::wire::provider_catalogue::provider_preset(&c.id).1.into(),
                    api_key: None,
                    auth_type: auth_type_for(&c.id).into(),
                    is_primary: true,
                    is_fallback: false,
                    model: String::new(),
                });
                "local"
            } else {
                "key"
            };
            ui.set_fr_connect_mode(mode.into());
        }
    });

    ui.on_fr_show_key_help(|id| {
        if let Some(url) = key_page(&id) {
            // A page where a key is made: the browser apart from the desktop's own.
            if let Err(e) = crate::open_url::open_apart(url) {
                tracing::warn!(error = %e, "first run: could not open the key page");
            }
        }
    });

    ui.on_fr_sign_in_browser({
        let weak = ui.as_weak();
        let lane = lane.clone();
        move |id| {
            let Some(ui) = weak.upgrade() else { return };
            if let Err(why) = lane.sign_in(&id) {
                ui.set_fr_connect_status("attention".into());
                ui.set_fr_connect_words(format!("Needs attention: {why}").into());
                ui.set_fr_connect_mode("key".into());
            }
        }
    });

    ui.on_fr_submit_key({
        let weak = ui.as_weak();
        move |id, key| {
            let Some(ui) = weak.upgrade() else { return };
            let (name, url) = crate::wire::provider_catalogue::provider_preset(&id);
            if url.is_empty() || key.trim().is_empty() {
                return;
            }
            connect(&ui, ProviderStoreEntry {
                id: format!("{id}-onboarding"),
                name: name.into(),
                provider_type: id.to_string(),
                base_url: url.into(),
                api_key: Some(key.trim().to_string()),
                auth_type: auth_type_for(&id).into(),
                is_primary: true,
                is_fallback: false,
                model: String::new(),
            });
        }
    });

    ui.on_fr_submit_custom({
        let weak = ui.as_weak();
        move |url, model, key| {
            let Some(ui) = weak.upgrade() else { return };
            let key = key.trim().to_string();
            connect(&ui, ProviderStoreEntry {
                id: "custom-onboarding".into(),
                name: "Custom".into(),
                provider_type: "custom".into(),
                base_url: url.trim().to_string(),
                auth_type: if key.is_empty() { "none".into() } else { "bearer".into() },
                api_key: if key.is_empty() { None } else { Some(key) },
                is_primary: true,
                is_fallback: false,
                // The person typed it: this one is theirs.
                model: model.trim().to_string(),
            });
        }
    });

    ui.on_fr_use_this_computer({
        let weak = ui.as_weak();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            // The image's own account for the bundled model, when it wrote one; else a new one.
            if let Some(mut e) = ProviderStore::load().entries.into_iter().find(|e| e.base_url.trim_end_matches('/') == LOCAL_ENDPOINT) {
                e.is_primary = true;
                connect(&ui, e);
                return;
            }
            connect(&ui, ProviderStoreEntry {
                id: "llamacpp-onboarding".into(),
                name: "On this computer".into(),
                provider_type: "llamacpp".into(),
                base_url: LOCAL_ENDPOINT.into(),
                api_key: None,
                auth_type: "none".into(),
                is_primary: true,
                is_fallback: false,
                model: String::new(),
            });
        }
    });

    ui.on_fr_sharing_chosen({
        let weak = ui.as_weak();
        let saved = saved.clone();
        move |mode| {
            update_saved(&saved, |s| s.sharing = mode.to_string());
            let Some(sharing) = Sharing::parse(&mode) else { return };
            if let Err(e) = apply_sharing(sharing) {
                tracing::warn!(error = %e, "first run: the sharing choice was not saved");
                if let Some(ui) = weak.upgrade() {
                    ui.set_fr_connect_words(format!("Needs attention: the sharing choice was not saved ({e})").into());
                }
            }
        }
    });

    ui.on_fr_start_checks({
        let weak = ui.as_weak();
        let state = state.clone();
        let proofs = proofs.clone();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            if ui.get_fr_checks_running() {
                return;
            }
            start_checks(&ui, state.clone(), proofs.clone());
        }
    });

    ui.on_fr_add_mind({
        let weak = ui.as_weak();
        move || {
            // Settings → Harnesses, where every mind installs with the model and memory chosen
            // here; the first run is done once a mind can be added there.
            let Some(ui) = weak.upgrade() else { return };
            crate::onboarding::write_marker();
            ui.set_settings_category(HARNESSES_SECTION);
            ui.set_current_screen(SETTINGS_SCREEN);
            ui.invoke_navigate(SETTINGS_SCREEN);
        }
    });

    ui.on_fr_finished({
        let weak = ui.as_weak();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            crate::onboarding::write_marker();
            // The first success happens in the chat beside the desktop.
            ui.set_fr_chat(1);
            ui.set_lens_open(true);
            tracing::info!("first run: done, chat opened for the first success");
        }
    });

    ui.on_fr_chat_done({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_fr_chat(0);
            }
        }
    });
}

fn card_name_for(provider_type: &str, saved_name: &str) -> String {
    match ProviderDescriptor::by_id(provider_type) {
        Some(p) => card_name(p),
        None => saved_name.to_string(),
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Save the account and prove it answers, off the UI thread. "Connected to Claude." only after a
/// real answer; anything else is the plain reason.
fn connect(ui: &App, entry: ProviderStoreEntry) {
    if entry.base_url.is_empty() {
        ui.set_fr_connect_status("attention".into());
        ui.set_fr_connect_words("Needs attention: there is no address for it".into());
        return;
    }
    if let Err(e) = save_primary(entry.clone()) {
        ui.set_fr_connect_status("attention".into());
        ui.set_fr_connect_words(format!("Needs attention: it could not be saved ({e})").into());
        return;
    }
    let name = ui.get_fr_chosen_name().to_string();
    ui.set_fr_connect_status("checking".into());
    ui.set_fr_connect_words(format!("Checking (started {})", clock(chrono::Local::now())).into());
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let result = prove_answer(&entry);
        if let Ok((model, _)) = &result {
            // The model it answered with is the one pick for every mind (#675).
            let id = crate::ai_accounts::models::gateway_id(&account_id(&entry), model);
            if let Err(e) = crate::ai_accounts::onboarding::choose_model(&id) {
                tracing::info!(error = %e, "first run: the pick waits for the account's model list");
            }
            update_saved(&saved_path(), |s| s.provider_chosen = true);
        }
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = weak.upgrade() else { return };
            match result {
                Ok(_) => {
                    ui.set_fr_connect_status("connected".into());
                    ui.set_fr_connect_words(format!("Connected to {name}.").into());
                }
                Err(why) => {
                    ui.set_fr_connect_status("attention".into());
                    ui.set_fr_connect_words(format!("Needs attention: {why}").into());
                }
            }
        });
    });
}

fn push_rows(ui: &App, state: &Mutex<State>) {
    let st = lock(state);
    ui.set_fr_minds(model_of(st.rows.iter().map(MindRows::to_ui).collect()));
    let done = st.rows.iter().all(MindRows::done);
    ui.set_fr_checks_done(done);
    ui.set_fr_checks_running(!done);
}

/// Run the three checks for every mind, one row at a time, each row saying when it started.
fn start_checks(ui: &App, state: Arc<Mutex<State>>, proofs: Arc<dyn Proofs>) {
    {
        let mut st = lock(&state);
        st.rows = proofs.minds().iter().map(|(id, n)| MindRows::new(id, n)).collect();
    }
    ui.set_fr_checks_running(true);
    ui.set_fr_checks_done(false);
    push_rows(ui, &state);
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let minds: Vec<String> = lock(&state).rows.iter().map(|r| r.id.clone()).collect();
        for mind in minds {
            for kind in CheckKind::ALL {
                set_row(&weak, &state, &mind, kind, CheckState::Checking(clock(chrono::Local::now())));
                let proof = match kind {
                    CheckKind::Answer => match ProviderStore::load().primary() {
                        None => Proof::Attention {
                            reason: "no account is connected".into(),
                            details: "providers.yaml has no primary provider".into(),
                        },
                        Some(entry) => match prove_answer(entry) {
                            Ok((model, ms)) => Proof::Passed { details: format!("{} · {model} · answered in {ms} ms", entry.base_url) },
                            Err(why) => Proof::Attention { reason: why.clone(), details: format!("{} · {why}", entry.base_url) },
                        },
                    },
                    CheckKind::Remember => proofs.remember(&mind),
                    CheckKind::Desktop => proofs.desktop(&mind),
                };
                set_row(&weak, &state, &mind, kind, CheckState::Done(proof));
            }
        }
    });
}

fn set_row(weak: &slint::Weak<App>, state: &Arc<Mutex<State>>, mind: &str, kind: CheckKind, s: CheckState) {
    if let Some(r) = lock(state).rows.iter_mut().find(|r| r.id == mind) {
        r.set(kind, s);
    }
    let weak = weak.clone();
    let state = state.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = weak.upgrade() {
            push_rows(&ui, &state);
        }
    });
}

/// Read this to me: the screen's words through the same voices the voice runtime uses (Piper,
/// else the system's), on a thread of its own so the screen never waits on it.
fn speak(text: String) {
    std::thread::spawn(move || {
        let piper = yantrik_ml::PiperTTS::from_dir(Path::new("/opt/yantrik/models/tts"));
        let result = match piper {
            Ok(p) => p.speak(&text, None),
            Err(_) => yantrik_ml::TTSEngine::new().and_then(|t| t.speak(&text, None)),
        };
        if let Err(e) = result {
            tracing::warn!(error = %e, "first run: could not read the screen aloud");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Signs;
    impl Lane for Signs {
        fn browser_sign_in(&self, p: &str) -> bool {
            p == "openrouter"
        }
        fn sign_in(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn downloadable(&self) -> Option<LocalModel> {
            Some(LocalModel { bytes: 2_500_000_000, present: false })
        }
    }

    #[test]
    fn the_full_provider_list_stays_behind_the_cards() {
        let every = every_card(&PendingLane);
        let known = KNOWN_PROVIDERS.iter().filter(|p| !p.id.is_empty()).count();
        assert_eq!(every.len(), known, "every provider the catalogue knows, expert tier included");
        for id in ["anthropic", "openai", "ollama", "llamacpp", "nvidia-nim", "cloudflare", "litellm"] {
            assert!(every.iter().any(|c| c.id == id), "{id}");
        }
    }

    #[test]
    fn named_cards_say_one_plain_line_and_an_honest_need() {
        let cards = featured_cards(None, &PendingLane);
        let claude = &cards[0];
        assert_eq!(claude.name, "Claude");
        assert_eq!(claude.line, "online, careful with words");
        assert_eq!(claude.needs, "needs an Anthropic account");
        assert!(cards.iter().all(|c| !c.needs.is_empty() && !c.line.is_empty()));
        assert_eq!(cards.iter().find(|c| c.id == "openai").unwrap().needs, "needs an OpenAI account");
        assert!(every_card(&PendingLane).iter().find(|c| c.id == "ollama").unwrap().needs.contains("running"));
    }

    #[test]
    fn this_computer_is_offered_only_when_a_model_is_here_or_downloadable_with_its_size() {
        assert!(!featured_cards(None, &PendingLane).iter().any(|c| c.id == THIS_COMPUTER));
        let here = LocalModel { bytes: 2_740_000_000, present: true };
        let c = featured_cards(Some(&here), &PendingLane).pop().unwrap();
        assert_eq!((c.name.as_str(), c.line.as_str(), c.needs.as_str()), ("On this computer", "private, slower", "already here (2.7 GB)"));
        let fetch = Signs.downloadable().unwrap();
        assert_eq!(this_computer_card(&fetch).needs, "downloads 2.5 GB");
    }

    #[test]
    fn a_model_on_disk_is_found_by_its_size_and_an_empty_dir_offers_none() {
        let dir = std::env::temp_dir().join(format!("fr-local-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(local_model_in(&dir), None);
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        assert_eq!(local_model_in(&dir), None);
        std::fs::write(dir.join("yantrik-4b.gguf"), vec![0u8; 4096]).unwrap();
        assert_eq!(local_model_in(&dir), Some(LocalModel { bytes: 4096, present: true }));
        assert_eq!(local_model_in(&dir.join("missing")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn browser_sign_in_is_offered_only_where_the_backend_has_it() {
        assert!(every_card(&PendingLane).iter().all(|c| !c.browser_sign_in), "the stub offers none");
        let with = every_card(&Signs);
        assert!(with.iter().find(|c| c.id == "openrouter").unwrap().browser_sign_in);
        assert!(!with.iter().find(|c| c.id == "anthropic").unwrap().browser_sign_in);
        assert!(PendingLane.sign_in("openrouter").is_err());
    }

    #[test]
    fn show_me_where_is_offered_only_for_a_known_key_page() {
        assert!(key_page("anthropic").unwrap().starts_with("https://"));
        assert_eq!(key_page("custom"), None);
        let every = every_card(&PendingLane);
        assert!(every.iter().find(|c| c.id == "anthropic").unwrap().key_help);
        assert!(!every.iter().find(|c| c.id == "ollama").unwrap().key_help);
    }

    #[test]
    fn a_row_goes_not_checked_then_checking_with_its_start_then_passed_or_the_reason() {
        let mut m = MindRows::new("companion", "Yantrik Companion");
        assert_eq!(m.checks[0].1.words(), "Not checked yet");
        assert_eq!(m.words(), "Not checked yet");
        m.set(CheckKind::Answer, CheckState::Checking("2:16".into()));
        assert_eq!(m.checks[0].1.words(), "Checking (started 2:16)");
        assert_eq!(m.checks[0].1.state(), "checking");
        m.set(CheckKind::Answer, CheckState::Done(Proof::Passed { details: "x".into() }));
        assert_eq!(m.checks[0].1.words(), "Passed");
        m.set(CheckKind::Remember, CheckState::Done(Proof::Attention { reason: "the note did not come back".into(), details: "".into() }));
        assert_eq!(m.checks[1].1.words(), "Needs attention: the note did not come back");
        assert!(!m.done());
        m.set(CheckKind::Desktop, CheckState::Done(Proof::Passed { details: "".into() }));
        assert!(m.done() && !m.ok());
        assert_eq!(m.words(), "Needs attention: the note did not come back");
        m.set(CheckKind::Remember, CheckState::Done(Proof::Passed { details: "".into() }));
        assert_eq!(m.words(), "Ready");
    }

    #[test]
    fn no_row_words_are_a_percentage_or_a_spinner() {
        let states = [
            CheckState::NotChecked,
            CheckState::Checking("9:05".into()),
            CheckState::Done(Proof::Passed { details: String::new() }),
            CheckState::Done(Proof::Attention { reason: "r".into(), details: String::new() }),
        ];
        for s in states {
            let w = s.words();
            assert!(!w.contains('%') && !w.contains("...") && !w.contains('…'), "{w}");
        }
    }

    #[test]
    fn the_clock_reads_like_a_person_says_it() {
        use chrono::TimeZone;
        let t = chrono::Local.with_ymd_and_hms(2026, 10, 11, 14, 16, 42).unwrap();
        assert_eq!(clock(t), "2:16");
        let t = chrono::Local.with_ymd_and_hms(2026, 10, 11, 9, 5, 0).unwrap();
        assert_eq!(clock(t), "9:05");
    }

    #[test]
    fn the_pending_proofs_never_claim_a_pass() {
        let p = PendingProofs;
        assert!(!p.minds().is_empty());
        for (id, _) in p.minds() {
            assert!(matches!(p.remember(&id), Proof::Attention { .. }));
            assert!(matches!(p.desktop(&id), Proof::Attention { .. }));
        }
    }

    #[test]
    fn the_private_lane_keeps_personal_things_on_this_computer_first() {
        let lan = ("lan-ollama/llama3".to_string(), "http://192.168.1.5:11434/v1".to_string());
        let here = ("on-this-computer-qwen-3-5-4b/qwen3.5-4b".to_string(), "http://127.0.0.1:8341/v1".to_string());
        assert_eq!(lane_model(&[lan.clone(), here.clone()]).as_deref(), Some(here.0.as_str()));
        assert_eq!(lane_model(&[lan.clone()]).as_deref(), Some(lan.0.as_str()));
        assert_eq!(lane_model(&[]), None, "no lane: refused with the reason, nothing shared");
        assert_eq!(Sharing::parse("share"), Some(Sharing::Share));
        assert_eq!(Sharing::parse("private-lane"), Some(Sharing::PrivateLane));
        assert_eq!(Sharing::parse("off"), Some(Sharing::Off));
        assert_eq!(Sharing::parse("local"), None, "a model on this computer has nothing to share");
    }

    #[test]
    fn the_account_id_is_the_one_ai_accounts_gives_the_saved_provider() {
        let e = ProviderStoreEntry {
            id: "anthropic-onboarding".into(),
            name: "Anthropic".into(),
            provider_type: "anthropic".into(),
            base_url: "https://api.anthropic.com/v1".into(),
            api_key: Some("k".into()),
            auth_type: "x-api-key".into(),
            is_primary: true,
            is_fallback: false,
            model: String::new(),
        };
        assert_eq!(account_id(&e), "anthropic");
        let ids: Vec<String> = crate::ai_accounts::account::accounts(&[e], &[], &Default::default(), &Default::default())
            .into_iter()
            .map(|a| a.id)
            .collect();
        assert_eq!(ids, ["anthropic"]);
    }

    #[test]
    fn the_sharing_sentence_is_the_one_ai_accounts_records() {
        let slint = include_str!("../../../yantrik-ui-slint/ui/onboarding_first_run.slint");
        let line = crate::ai_accounts::onboarding::share_line("Claude");
        let (head, tail) = line.split_once("Claude").unwrap();
        assert!(slint.contains(&format!("\"{head}\"")), "{head}");
        assert!(slint.contains(&format!("\"{tail}\"")), "{tail}");
    }

    #[test]
    fn the_private_lane_offer_states_its_size_or_is_absent() {
        assert_eq!(private_lane_words(None), "");
        let w = private_lane_words(Some(&LocalModel { bytes: 2_500_000_000, present: false }));
        assert!(w.contains("2.5 GB") && w.contains("slower"), "{w}");
    }

    #[test]
    fn what_is_shared_is_listed_plainly() {
        assert!(SHARED_ITEMS.contains(&"Your name") && SHARED_ITEMS.contains(&"Your town"));
        assert!(SHARED_ITEMS.contains(&"The app you're in"));
    }

    #[test]
    fn the_text_size_and_sharing_choice_survive_a_restart() {
        let dir = std::env::temp_dir().join(format!("fr-saved-{}", std::process::id()));
        let path = dir.join("first-run.json");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(load_saved(&path), Saved::default());
        update_saved(&path, |s| s.text_scale = 1.25);
        update_saved(&path, |s| s.sharing = "private-lane".into());
        let s = load_saved(&path);
        assert_eq!((s.text_scale, s.sharing.as_str()), (1.25, "private-lane"));
        assert!(!std::fs::read_to_string(&path).unwrap().contains("key"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Only the experience doc's plain words, on every screen of the first run.
    #[test]
    fn the_screens_use_the_plain_vocabulary() {
        let slint = include_str!("../../../yantrik-ui-slint/ui/onboarding_first_run.slint");
        let shown: String = slint
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .filter_map(|l| l.split_once("text: \"").or_else(|| l.split_once("label: \"")).map(|(_, r)| r))
            .collect::<Vec<_>>()
            .join("\n");
        for word in ["Lens", "harness", "Witness", "thread", "private lane", "attach", "Decline", "Error", "LLM", "API"] {
            assert!(!shown.contains(word), "a screen says \"{word}\"");
        }
        for want in ["Welcome to Yantrik.", "Make text bigger", "Read this to me", "Get started", "Who should do the thinking?",
                     "Connect account", "Paste a key", "Show me where to find it", "Advanced", "What's shared?",
                     "Keep personal things on this computer instead", "Getting your minds ready", "Your minds", "Add another mind"] {
            assert!(slint.contains(want), "{want}");
        }
    }
}
