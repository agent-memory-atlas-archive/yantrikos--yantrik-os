//! The Agents screen (34), and every agent popped out into a window of its own.
//!
//! Both draw from the one store in `crate::agents`, through the `AgentsState` global in
//! `agents.slint`. The screen's instance of that global lives on the shell's window; each popped-out
//! `AgentWindow` has its own, filled by the same code from the same store — so the two views of an
//! agent cannot disagree, and closing a window closes a view, never the agent.
//!
//! A timer redraws a quarter of a second at a time: the list and the counts while the screen is up
//! (the "running · 2m" moves on its own), and each open agent window. A session is rebuilt only when
//! the store has changed or the person opened or folded something.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use slint::{ComponentHandle, Model, ModelRc, Timer, TimerMode, VecModel};

use crate::agents::model::{bytes, now, Agent, CallState, Card, Details, Item, Mark, OutputKind, Provenance, State, Tab};
use crate::agents::{self, launch, AgentId, Store};
use crate::app_context::AppContext;
use crate::{
    AccentPreset, AgentDetailsData, AgentHeaderData, AgentItemData, AgentMindData, AgentRowData, AgentRunData,
    AgentTabData, AgentWindow, AgentsState, App, ThemeMode, ThemeOverrides, ToolCallData,
};

/// The screen id `app.slint` draws the Agents screen at.
pub const SCREEN: i32 = 34;

const TICK: Duration = Duration::from_millis(250);

/// How many of an agent's turns a session shows. The rest are in its file.
const SHOWN_TURNS: usize = 30;

/// How many lines of a running call's text output show under its line.
const LIVE_LINES: usize = 8;

/// How much of a call's output an opened card shows. All of it is one click further, in the Editor.
const OPEN_BYTES: usize = 64 * 1024;

/// How much of one block of the mind's text is drawn.
const TEXT_BYTES: usize = 32 * 1024;

/// What one surface shows: the screen, or one popped-out window.
struct Surface {
    agent: Option<AgentId>,
    /// Cards and thinking the person opened, by key.
    expanded: HashSet<String>,
    items: Rc<VecModel<AgentItemData>>,
    keys: Vec<String>,
    /// The store revision and local change drawn last.
    drawn: Option<(u64, u64)>,
    local: u64,
}

impl Surface {
    fn new() -> Self {
        Surface {
            agent: None,
            expanded: HashSet::new(),
            items: Rc::new(VecModel::default()),
            keys: Vec::new(),
            drawn: None,
            local: 0,
        }
    }

    fn toggle(&mut self, key: &str, open: bool) {
        if open {
            self.expanded.insert(key.to_string());
        } else {
            self.expanded.remove(key);
        }
        self.local += 1;
    }
}

/// An agent in its own window.
struct Popped {
    window: AgentWindow,
    surface: Surface,
    /// Set by the window's ×. The window is hidden then, and let go on the next tick — never from
    /// inside its own close handler.
    closed: Rc<Cell<bool>>,
    title: String,
}

struct Screen {
    tab: Tab,
    selected: Option<AgentId>,
    /// The rows as drawn, so they can be held still while the pointer is over them.
    order: Vec<AgentId>,
    main: Surface,
    windows: BTreeMap<AgentId, Popped>,
}

type Shared = Rc<RefCell<Screen>>;

pub fn wire(ui: &App, _ctx: &AppContext) {
    let state: Shared = Rc::new(RefCell::new(Screen {
        tab: Tab::Active,
        selected: None,
        order: Vec::new(),
        main: Surface::new(),
        windows: BTreeMap::new(),
    }));
    let g = ui.global::<AgentsState>();
    g.set_items(ModelRc::from(state.borrow().main.items.clone()));

    let weak = ui.as_weak();
    let on = |f: fn(&App, &Shared, String)| {
        let (weak, state) = (weak.clone(), state.clone());
        move |arg: slint::SharedString| {
            if let Some(ui) = weak.upgrade() {
                f(&ui, &state, arg.to_string());
            }
        }
    };

    g.on_select_tab(on(|ui, state, key| {
        {
            let mut st = state.borrow_mut();
            st.tab = Tab::from_key(&key);
            st.order.clear();
            st.selected = None;
        }
        ui.global::<AgentsState>().set_tab(key.into());
        refresh(ui, state, true);
    }));
    g.on_select(on(|ui, state, id| {
        state.borrow_mut().selected = Some(AgentId(id));
        refresh(ui, state, true);
    }));
    g.on_pop_out(on(|ui, state, id| pop_out(ui, state, AgentId(id))));
    g.on_stop(on(|ui, state, id| {
        notice(&ui.global::<AgentsState>(), launch::stop(&AgentId(id)));
        refresh(ui, state, true);
    }));
    g.on_close(on(|ui, state, id| close(ui, state, AgentId(id), false)));
    g.on_close_confirmed(on(|ui, state, id| close(ui, state, AgentId(id), true)));
    g.on_close_cancelled({
        let weak = weak.clone();
        move || {
            if let Some(ui) = weak.upgrade() {
                ui.global::<AgentsState>().set_confirm_close("".into());
            }
        }
    });
    g.on_toggle({
        let (weak, state) = (weak.clone(), state.clone());
        move |key, open| {
            let Some(ui) = weak.upgrade() else { return };
            state.borrow_mut().main.toggle(&key, open);
            refresh(&ui, &state, false);
        }
    });
    g.on_open_all(on(|ui, state, key| {
        let agent = state.borrow().main.agent.clone();
        if let Some(agent) = agent {
            notice(&ui.global::<AgentsState>(), open_all(&agent, &key));
        }
    }));
    g.on_pick_mind(on(|ui, _state, mind| pick_mind(&ui.global::<AgentsState>(), &mind)));
    g.on_start({
        let (weak, state) = (weak.clone(), state.clone());
        move |mind, prompt| {
            let Some(ui) = weak.upgrade() else { return };
            let g = ui.global::<AgentsState>();
            match launch::start(&mind, &prompt) {
                Ok(agent) => {
                    g.set_new_open(false);
                    g.set_new_error("".into());
                    {
                        let mut st = state.borrow_mut();
                        if st.tab != Tab::All {
                            st.tab = Tab::Active;
                            g.set_tab(Tab::Active.key().into());
                        }
                        st.order.clear();
                        st.selected = Some(agent);
                    }
                    refresh(&ui, &state, true);
                }
                Err(why) => g.set_new_error(why.into()),
            }
        }
    });
    g.on_send({
        let (weak, state) = (weak.clone(), state.clone());
        move |agent, text| {
            let Some(ui) = weak.upgrade() else { return };
            notice(&ui.global::<AgentsState>(), launch::send(&AgentId(agent.to_string()), &text));
            refresh(&ui, &state, true);
        }
    });
    g.on_dismiss_notice({
        let weak = weak.clone();
        move || {
            if let Some(ui) = weak.upgrade() {
                ui.global::<AgentsState>().set_notice("".into());
            }
        }
    });

    let timer = Timer::default();
    {
        let (weak, state) = (weak.clone(), state.clone());
        timer.start(TimerMode::Repeated, TICK, move || {
            agents::store().save_if_due();
            sync_with_host(&Seen::now());
            let Some(ui) = weak.upgrade() else { return };
            if ui.get_current_screen() == SCREEN {
                refresh(&ui, &state, false);
            }
            refresh_windows(&ui, &state);
        });
    }
    // The timer lives as long as the shell, the idiom every wire module uses.
    std::mem::forget(timer);
}

/// Say how a press went, when it did not go as asked.
fn notice(g: &AgentsState, outcome: Result<(), String>) {
    match outcome {
        Ok(()) => g.set_notice("".into()),
        Err(why) => g.set_notice(why.into()),
    }
}

/// What the harness host says right now: the minds attached, and the agents live on them.
struct Seen {
    /// Attached minds that can hold agents — every one but the built-in, which answers in the
    /// Lens: (id, name, what it says it runs on).
    minds: Vec<(String, String, String)>,
    agents: Vec<yantrik_harness::AgentEntry>,
}

impl Seen {
    fn now() -> Seen {
        let Some(host) = crate::wire::harness::host() else {
            return Seen { minds: Vec::new(), agents: Vec::new() };
        };
        Seen {
            minds: host
                .list()
                .into_iter()
                .filter(|e| !e.builtin)
                .map(|e| (e.id, e.name, e.detail.unwrap_or_default()))
                .collect(),
            agents: host.agents(),
        }
    }

    fn attached(&self) -> Vec<String> {
        self.minds.iter().map(|(id, _, _)| id.clone()).collect()
    }

    fn live(&self) -> Vec<AgentId> {
        self.agents.iter().map(|a| a.id.clone()).collect()
    }
}

/// Keep the list honest about the host: a conversation the host is running shows up here even
/// when this screen did not start it, and an agent caught working when its harness went is marked
/// so. Nothing else is taken from the host — the session itself comes in through `feed`.
fn sync_with_host(seen: &Seen) {
    if crate::wire::harness::host().is_none() {
        return;
    }
    let attached = seen.attached();
    let (unknown, gone) = agents::store().read(|s| {
        let unknown: Vec<agents::AgentMeta> = seen
            .agents
            .iter()
            .filter(|e| s.agent(&e.id).is_none())
            .map(|e| {
                let mut meta = agents::AgentMeta::new(e.id.clone(), e.harness_name.clone());
                meta.conversations = e.conversations;
                meta.started = e.started.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
                meta
            })
            .collect();
        let gone: Vec<AgentId> = s
            .agents()
            .iter()
            .filter(|a| a.state.working() && a.meta.id.harness() != crate::wire::harness::BUILTIN_ID)
            .filter(|a| !attached.iter().any(|h| h == a.meta.id.harness()))
            .map(|a| a.meta.id.clone())
            .collect();
        (unknown, gone)
    });
    for meta in unknown {
        agents::store().upsert_agent(meta);
    }
    for agent in gone {
        agents::store().set_state(&agent, State::HarnessGone);
    }
}

fn pick_mind(g: &AgentsState, mind: &str) {
    g.set_new_mind(mind.into());
    g.set_new_error("".into());
    let seen = Seen::now();
    let name = seen.minds.iter().find(|(id, _, _)| id == mind).map(|(_, name, _)| name.clone()).unwrap_or_else(|| mind.to_string());
    let theirs: Vec<&yantrik_harness::AgentEntry> = seen.agents.iter().filter(|a| a.harness == mind).collect();
    let note = if theirs.iter().any(|a| a.conversations) {
        format!("{name} holds a conversation per agent: this one starts fresh, apart from the Lens.")
    } else if !theirs.is_empty() {
        format!(
            "{name} holds one conversation at a time, so this continues it — the same \
             conversation the Lens has with it."
        )
    } else {
        String::new()
    };
    g.set_new_note(note.into());
}

/// Redraw the screen: tabs, list, and the selected agent's session and details.
fn refresh(ui: &App, state: &Shared, force: bool) {
    let g = ui.global::<AgentsState>();
    let seen = Seen::now();
    let hovering = g.get_list_hovered();
    let mut st = state.borrow_mut();
    let st = &mut *st;
    agents::store().read(|s| {
        let tabs: Vec<AgentTabData> = Tab::EVERY
            .iter()
            .zip(s.counts())
            .map(|(tab, count)| AgentTabData { id: tab.key().into(), label: tab.label().into(), count: count as i32 })
            .collect();
        if let Some(model) = crate::models::changed(g.get_tabs(), tabs) {
            g.set_tabs(model);
        }

        let hold = (hovering && !st.order.is_empty()).then_some(st.order.as_slice());
        let order = s.list(st.tab, hold);
        let rows: Vec<AgentRowData> = order.iter().filter_map(|id| s.agent(id)).map(row_of).collect();
        st.order = order;
        if let Some(model) = crate::models::changed(g.get_rows(), rows) {
            g.set_rows(model);
        }

        if st.selected.as_ref().is_none_or(|id| s.agent(id).is_none()) {
            st.selected = st.order.first().cloned();
        }
        let selected = st.selected.clone().map(|id| id.0).unwrap_or_default();
        if g.get_selected() != selected.as_str() {
            g.set_selected(selected.into());
        }
        let selected = st.selected.clone();
        draw(&g, &mut st.main, s, selected.as_ref(), &seen, force);
    });

    if g.get_new_open() {
        let rows: Vec<AgentMindData> = seen
            .minds
            .iter()
            .map(|(id, name, detail)| AgentMindData { id: id.into(), name: name.into(), detail: detail.into() })
            .collect();
        if let Some(model) = crate::models::changed(g.get_minds(), rows) {
            g.set_minds(model);
        }
        // Start where the Lens is, when the Lens is talking to a mind that can hold an agent.
        if g.get_new_mind().is_empty() {
            if let Some(active) = crate::wire::harness::host().map(|h| h.active_id()) {
                if seen.minds.iter().any(|(id, _, _)| *id == active) {
                    pick_mind(&g, &active);
                }
            }
        }
    }
}

/// Redraw every popped-out window, and let go of the ones whose × was pressed.
fn refresh_windows(ui: &App, state: &Shared) {
    let mut st = state.borrow_mut();
    st.windows.retain(|_, p| !p.closed.get());
    if st.windows.is_empty() {
        return;
    }
    let seen = Seen::now();
    let windows = &mut st.windows;
    agents::store().read(|s| {
        for (id, popped) in windows.iter_mut() {
            sync_theme(ui, &popped.window);
            draw(&popped.window.global::<AgentsState>(), &mut popped.surface, s, Some(id), &seen, false);
        }
    });
}

/// Fill one surface with one agent: its header and details every time (they carry the clock), its
/// session when something in it changed.
fn draw(g: &AgentsState, surface: &mut Surface, s: &Store, agent: Option<&AgentId>, seen: &Seen, force: bool) {
    let Some(a) = agent.and_then(|id| s.agent(id)) else {
        if g.get_has_agent() {
            g.set_has_agent(false);
            g.set_header(AgentHeaderData::default());
            g.set_details(AgentDetailsData::default());
        }
        if surface.agent.take().is_some() || surface.items.row_count() > 0 {
            publish_items(g, surface, Vec::new(), true);
        }
        return;
    };
    if !g.get_has_agent() {
        g.set_has_agent(true);
    }
    let header = header_of(a, seen);
    if g.get_header() != header {
        g.set_header(header);
    }
    let details = details_of(a, s.details(&a.meta.id).unwrap_or_default());
    if g.get_details() != details {
        g.set_details(details);
    }

    let fresh = surface.agent.as_ref() != Some(&a.meta.id);
    if fresh {
        surface.agent = Some(a.meta.id.clone());
        surface.expanded.clear();
        surface.drawn = None;
    }
    let stamp = (s.revision(), surface.local);
    if !force && !fresh && surface.drawn == Some(stamp) {
        return;
    }
    surface.drawn = Some(stamp);
    publish_items(g, surface, items_of(a, &surface.expanded), fresh);
}

/// Put a session's items in the model: in place when only the end changed, so the view keeps its
/// scroll and each card keeps its state; a new model when the agent or the shape changed.
fn publish_items(g: &AgentsState, surface: &mut Surface, items: Vec<AgentItemData>, fresh: bool) {
    let keys: Vec<String> = items.iter().map(|i| i.key.to_string()).collect();
    let extends = !fresh && keys.len() >= surface.keys.len() && surface.keys.iter().zip(&keys).all(|(a, b)| a == b);
    if extends {
        let model = &surface.items;
        for (i, item) in items.into_iter().enumerate() {
            if i < model.row_count() {
                if model.row_data(i).as_ref() != Some(&item) {
                    model.set_row_data(i, item);
                }
            } else {
                model.push(item);
            }
        }
    } else {
        surface.items = Rc::new(VecModel::from(items));
        g.set_items(ModelRc::from(surface.items.clone()));
    }
    surface.keys = keys;
}

// ── From the store to what the screen draws ────────────────────────

fn row_of(a: &Agent) -> AgentRowData {
    AgentRowData {
        id: a.meta.id.0.as_str().into(),
        mind: a.meta.mind.as_str().into(),
        title: a.meta.title.as_str().into(),
        state: a.state.key().into(),
        label: a.state.label().into(),
        since: since(a).into(),
        parent: a.meta.parent.as_ref().map(|p| p.0.clone()).unwrap_or_default().into(),
    }
}

/// "2m" while it works, "21:02" once it has stopped.
fn since(a: &Agent) -> String {
    if a.state.live() {
        duration(now().saturating_sub(a.since))
    } else {
        clock(a.since)
    }
}

fn duration(secs: u64) -> String {
    match secs {
        0..=4 => "just now".into(),
        5..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        _ => format!("{}h {}m", secs / 3600, (secs % 3600) / 60),
    }
}

/// Local time today, the date before today.
fn clock(unix: u64) -> String {
    use chrono::TimeZone;
    let Some(at) = chrono::Local.timestamp_opt(unix as i64, 0).single() else { return String::new() };
    if at.date_naive() == chrono::Local::now().date_naive() {
        at.format("%H:%M").to_string()
    } else {
        at.format("%b %-d, %H:%M").to_string()
    }
}

fn header_of(a: &Agent, seen: &Seen) -> AgentHeaderData {
    let harness = a.meta.id.harness();
    let attached = seen.minds.iter().any(|(id, _, _)| id == harness);
    let builtin = harness == crate::wire::harness::BUILTIN_ID;
    let reachable = launch::reachable(&a.meta.id, &seen.live(), &seen.attached());
    let turn_open = a.open_turn().is_some();
    let gone = a.state == State::HarnessGone;
    let send_hint = if gone {
        "its harness is gone".to_string()
    } else if !attached {
        format!("{} is not attached right now", a.meta.mind)
    } else if !reachable {
        "this conversation has ended — start a new agent to go on".to_string()
    } else if turn_open {
        format!("{} is working — wait, or Stop it", a.meta.mind)
    } else {
        String::new()
    };
    let note = if a.meta.conversations || builtin {
        String::new()
    } else {
        format!("{} holds one conversation at a time — the same one the Lens talks to.", a.meta.mind)
    };
    AgentHeaderData {
        id: a.meta.id.0.as_str().into(),
        mind: a.meta.mind.as_str().into(),
        title: a.meta.title.as_str().into(),
        state: a.state.key().into(),
        label: a.state.label().into(),
        since: since(a).into(),
        status: a.status.as_str().into(),
        note: note.into(),
        can_send: reachable && !turn_open && !gone,
        send_hint: send_hint.into(),
        can_stop: attached && !builtin && a.busy(),
    }
}

fn details_of(a: &Agent, d: Details) -> AgentDetailsData {
    let model = if !a.meta.model.is_empty() { a.meta.model.clone() } else { d.usage.model.clone() };
    let calls = if d.failed_calls > 0 { format!("{} ({} failed)", d.calls, d.failed_calls) } else { d.calls.to_string() };
    let failed_commands = d.commands.iter().filter(|(_, _, s)| *s == CallState::Failed).count();
    let commands = match (d.commands.len(), failed_commands) {
        (0, _) => "none run by the shell".to_string(),
        (n, 0) => n.to_string(),
        (n, f) => format!("{n} ({f} failed)"),
    };
    let command_lines: Vec<String> = d
        .commands
        .iter()
        .rev()
        .take(6)
        .rev()
        .map(|(line, exit, state)| {
            let how = match (state, exit) {
                (CallState::Running, _) => "running".to_string(),
                (_, Some(code)) => format!("exit {code}"),
                (s, None) => s.key().to_string(),
            };
            format!("{how:>8}  {}", one_line(line, 60))
        })
        .collect();
    let mut file_lines: Vec<String> = d.files.iter().take(6).cloned().collect();
    if d.files.len() > 6 {
        file_lines.push(format!("and {} more", d.files.len() - 6));
    }
    let tokens = if d.usage.reported {
        format!("{} in · {} out", thousands(d.usage.input_tokens), thousands(d.usage.output_tokens))
    } else {
        "not reported".to_string()
    };
    let cost = if d.usage.cost_usd > 0.0 { format!("${:.2}", d.usage.cost_usd) } else { String::new() };
    AgentDetailsData {
        mind: a.meta.mind.as_str().into(),
        model: model.into(),
        since: clock(a.meta.started).into(),
        turns: d.turns.to_string().into(),
        calls: calls.into(),
        commands: commands.into(),
        command_lines: command_lines.join("\n").into(),
        files: if d.files.is_empty() { "none named".into() } else { d.files.len().to_string().into() },
        file_lines: file_lines.join("\n").into(),
        approvals: format!("{} asked · {} answered", d.approvals_asked, d.approvals_answered).into(),
        tokens: tokens.into(),
        cost: cost.into(),
        refused: if d.refused > 0 { format!("{} events", d.refused).into() } else { "".into() },
        basis: "Commands, files and approvals count only what the shell itself ran or asked. Calls \
                include what the harness reported."
            .into(),
    }
}

fn thousands(n: u64) -> String {
    if n >= 10_000 {
        format!("{}k", n / 1000)
    } else if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

fn one_line(text: &str, max: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    format!("{}…", flat.chars().take(max - 1).collect::<String>())
}

/// A session as the screen draws it, newest last.
fn items_of(a: &Agent, expanded: &HashSet<String>) -> Vec<AgentItemData> {
    let mut out = Vec::new();
    let from = a.turns.len().saturating_sub(SHOWN_TURNS);
    if from > 0 {
        out.push(AgentItemData {
            kind: "note".into(),
            key: "earlier".into(),
            text: format!("{from} earlier turns are kept in its saved session, not shown here.").into(),
            ..Default::default()
        });
    }
    for turn in &a.turns[from..] {
        if !turn.prompt.is_empty() {
            out.push(AgentItemData {
                kind: "prompt".into(),
                key: format!("t{}", turn.n).into(),
                text: turn.prompt.as_str().into(),
                ..Default::default()
            });
        }
        for (j, item) in turn.items.iter().enumerate() {
            let key = format!("t{}.{}", turn.n, j);
            let open = expanded.contains(&key);
            match item {
                Item::Text(text) => {
                    let text = text.last(TEXT_BYTES);
                    let text = text.trim_matches('\n');
                    if text.trim().is_empty() {
                        continue;
                    }
                    out.push(AgentItemData { kind: "text".into(), key: key.into(), text: text.into(), ..Default::default() });
                }
                Item::Thinking(text) => {
                    let text = text.last(TEXT_BYTES);
                    let shown = if open { text.trim().to_string() } else { one_line(&text, 160) };
                    out.push(AgentItemData {
                        kind: "thinking".into(),
                        key: key.into(),
                        text: shown.into(),
                        expanded: open,
                        ..Default::default()
                    });
                }
                Item::Note(note) => {
                    out.push(AgentItemData { kind: "note".into(), key: key.into(), text: note.as_str().into(), ..Default::default() })
                }
                Item::Card(card) => out.push(card_of(card, key, open)),
            }
        }
    }
    out
}

/// One call, as the card draws it.
fn card_of(c: &Card, key: String, open: bool) -> AgentItemData {
    let call = c.as_call();
    let has_output = !c.output.bytes.is_empty();
    let live = c.running() && has_output;
    let lines = c.output.lines();
    let total = c.output.bytes.total();

    let mut badge = vec![c.provenance.key().to_string()];
    if let Some(code) = c.exit_code {
        badge.push(format!("exit {code}"));
    }

    let mut explain: Vec<String> = Vec::new();
    if !c.summary.is_empty() {
        explain.push(c.summary.clone());
    }
    match (c.state, c.mark) {
        (_, Some(Mark::EndWithoutStart)) => {
            explain.push("The harness said this call ended, and never said it started.".into())
        }
        (_, Some(Mark::OutputWithoutStart)) => {
            explain.push("Output arrived for a call the harness never said it started.".into())
        }
        (CallState::Untold, None) => {
            explain.push("Read from the mind's own text. It did not say how the call went.".into())
        }
        (CallState::Interrupted, None) => {
            explain.push("Still open when its turn ended; it never said how it went.".into())
        }
        _ => {}
    }
    if c.provenance == Provenance::Verified && c.is_command() && c.ended.is_some() {
        explain.push("Run by the shell itself; the exit code is the process's own.".into());
    }
    if let Some(mark) = c.mark {
        badge.push(mark.label().into());
    }

    // What ToolCallCard shows when opened is `call.output`; a single space while folded only tells
    // it there is something to open.
    let (output, card_output) = match c.output.kind {
        OutputKind::Text if open => (String::new(), c.output.plain(OPEN_BYTES)),
        OutputKind::Text if live => (c.output.tail_lines(LIVE_LINES), " ".to_string()),
        OutputKind::Text | OutputKind::Terminal if has_output && !open => (String::new(), " ".to_string()),
        _ => (String::new(), String::new()),
    };
    let more = match c.output.kind {
        OutputKind::Text if open && total > OPEN_BYTES as u64 => format!("the last {} of {}", bytes(OPEN_BYTES as u64), bytes(total)),
        OutputKind::Text if live && lines > LIVE_LINES as u64 => format!("the last {LIVE_LINES} of {lines} lines"),
        OutputKind::Terminal if (open || live) && lines > 0 => {
            format!("{lines} line{} in all", if lines == 1 { "" } else { "s" })
        }
        _ => String::new(),
    };
    let (runs, rows) = if c.output.kind == OutputKind::Terminal && (open || live) {
        let runs: Vec<AgentRunData> = c
            .output
            .runs()
            .into_iter()
            .map(|r| AgentRunData {
                text: r.text.into(),
                row: r.row as i32,
                col: r.col as i32,
                columns: r.width as i32,
                fg: slint::Color::from_rgb_u8(r.fg.0, r.fg.1, r.fg.2),
                bg: slint::Color::from_rgb_u8(r.bg.0, r.bg.1, r.bg.2),
                bold: r.bold,
            })
            .collect();
        let rows = runs.iter().map(|r| r.row + 1).max().unwrap_or(1);
        (ModelRc::new(VecModel::from(runs)), rows)
    } else {
        // The empty model compares equal to itself, so a card with nothing to draw is not
        // re-sent to the view on every redraw.
        (ModelRc::default(), 0)
    };

    AgentItemData {
        kind: "card".into(),
        key: key.into(),
        text: Default::default(),
        call: ToolCallData {
            name: call.name.as_str().into(),
            target: call.target.as_str().into(),
            summary: call.summary().into(),
            arguments: call.detail().into(),
            status: c.state.status().into(),
            output: card_output.into(),
        },
        badge: badge.join(" · ").into(),
        explain: explain.join("\n").into(),
        expanded: open,
        live,
        output_kind: match c.output.kind {
            OutputKind::None => "",
            OutputKind::Text => "text",
            OutputKind::Terminal => "terminal",
        }
        .into(),
        output: output.into(),
        more: more.into(),
        can_open_all: has_output && (c.output.kind == OutputKind::Terminal || total > OPEN_BYTES as u64),
        runs,
        rows,
    }
}

// ── Acts ──────────────────────────────────────────────────────────

/// Close an agent: asked first when it is still working; then stopped, its window closed, and
/// taken off the list with its saved session.
fn close(ui: &App, state: &Shared, agent: AgentId, confirmed: bool) {
    let g = ui.global::<AgentsState>();
    let busy = agents::store().read(|s| s.agent(&agent).is_some_and(Agent::busy));
    if busy && !confirmed {
        g.set_confirm_close(agent.0.as_str().into());
        return;
    }
    g.set_confirm_close("".into());
    if busy {
        if let Err(why) = launch::stop(&agent) {
            tracing::info!(agent = %agent, %why, "closing an agent that could not be stopped");
        }
    }
    agents::store().remove_agent(&agent);
    {
        let mut st = state.borrow_mut();
        if let Some(popped) = st.windows.remove(&agent) {
            let _ = popped.window.hide();
        }
        if st.selected.as_ref() == Some(&agent) {
            st.selected = None;
        }
    }
    refresh(ui, state, true);
}

/// The title an agent's window carries. labwc and the taskbar know a window by it.
fn window_title(mind: &str, title: &str) -> String {
    one_line(&format!("{}{mind} · {title}", agents::WINDOW_TITLE_PREFIX), 90)
}

/// Open an agent in a window of its own — or, when it already has one, bring that one forward.
fn pop_out(ui: &App, state: &Shared, agent: AgentId) {
    let known = agents::store().read(|s| s.agent(&agent).map(|a| (a.meta.mind.clone(), a.meta.title.clone())));
    let Some((mind, title)) = known else {
        notice(&ui.global::<AgentsState>(), Err("That agent is no longer in the list.".into()));
        return;
    };

    // One window per agent: a second Pop out raises the first.
    {
        let st = state.borrow();
        if let Some(popped) = st.windows.get(&agent) {
            popped.closed.set(false);
            let _ = popped.window.show();
            popped.window.window().set_minimized(false);
            let title = popped.title.clone();
            // wlrctl is a process; the UI thread does not wait on it.
            std::thread::spawn(move || {
                crate::windows::present(&title);
            });
            return;
        }
    }

    let window = match AgentWindow::new() {
        Ok(window) => window,
        Err(e) => {
            notice(&ui.global::<AgentsState>(), Err(format!("Could not open a window for this agent: {e}")));
            return;
        }
    };
    let title = window_title(&mind, &title);
    window.set_agent_title(title.as_str().into());
    sync_theme(ui, &window);
    let surface = Surface::new();
    {
        let g = window.global::<AgentsState>();
        g.set_popped(true);
        g.set_items(ModelRc::from(surface.items.clone()));
    }
    wire_window(&window, state, &agent);
    let closed = Rc::new(Cell::new(false));
    {
        let closed = closed.clone();
        window.window().on_close_requested(move || {
            closed.set(true);
            slint::CloseRequestResponse::HideWindow
        });
    }
    let seen = Seen::now();
    let mut st = state.borrow_mut();
    let popped = st.windows.entry(agent.clone()).or_insert(Popped { window, surface, closed, title });
    agents::store().read(|s| draw(&popped.window.global::<AgentsState>(), &mut popped.surface, s, Some(&agent), &seen, true));
    if let Err(e) = popped.window.show() {
        st.windows.remove(&agent);
        drop(st);
        notice(&ui.global::<AgentsState>(), Err(format!("Could not show a window for this agent: {e}")));
    }
}

/// The acts a popped-out window has: fold and open, Stop, say more, open all output.
fn wire_window(window: &AgentWindow, state: &Shared, agent: &AgentId) {
    let g = window.global::<AgentsState>();
    let weak = window.as_weak();
    g.on_toggle({
        let (state, agent) = (state.clone(), agent.clone());
        move |key, open| {
            let mut st = state.borrow_mut();
            let Some(popped) = st.windows.get_mut(&agent) else { return };
            popped.surface.toggle(&key, open);
            let seen = Seen::now();
            agents::store().read(|s| draw(&popped.window.global::<AgentsState>(), &mut popped.surface, s, Some(&agent), &seen, false));
        }
    });
    g.on_stop({
        let weak = weak.clone();
        move |agent| {
            if let Some(window) = weak.upgrade() {
                notice(&window.global::<AgentsState>(), launch::stop(&AgentId(agent.to_string())));
            }
        }
    });
    g.on_send({
        let weak = weak.clone();
        move |agent, text| {
            if let Some(window) = weak.upgrade() {
                notice(&window.global::<AgentsState>(), launch::send(&AgentId(agent.to_string()), &text));
            }
        }
    });
    g.on_open_all({
        let (weak, agent) = (weak.clone(), agent.clone());
        move |key| {
            if let Some(window) = weak.upgrade() {
                notice(&window.global::<AgentsState>(), open_all(&agent, &key));
            }
        }
    });
    g.on_dismiss_notice({
        let weak = weak.clone();
        move || {
            if let Some(window) = weak.upgrade() {
                window.global::<AgentsState>().set_notice("".into());
            }
        }
    });
}

/// A window has its own copy of every global, so it takes the shell's theme — dark or light, the
/// accent, any community theme — from the shell, and keeps taking it.
fn sync_theme(ui: &App, window: &AgentWindow) {
    macro_rules! copy {
        ($global:ty, $get:ident, $set:ident) => {{
            let want = ui.global::<$global>().$get();
            if window.global::<$global>().$get() != want {
                window.global::<$global>().$set(want);
            }
        }};
    }
    copy!(ThemeMode, get_dark, set_dark);
    copy!(AccentPreset, get_index, set_index);
    copy!(ThemeOverrides, get_enabled, set_enabled);
    copy!(ThemeOverrides, get_bg_deep_override, set_bg_deep_override);
    copy!(ThemeOverrides, get_bg_surface_override, set_bg_surface_override);
    copy!(ThemeOverrides, get_bg_card_override, set_bg_card_override);
    copy!(ThemeOverrides, get_bg_elevated_override, set_bg_elevated_override);
    copy!(ThemeOverrides, get_amber_override, set_amber_override);
    copy!(ThemeOverrides, get_cyan_override, set_cyan_override);
    copy!(ThemeOverrides, get_text_primary_override, set_text_primary_override);
    copy!(ThemeOverrides, get_text_secondary_override, set_text_secondary_override);
    copy!(ThemeOverrides, get_text_dim_override, set_text_dim_override);
    copy!(ThemeOverrides, get_accent_override, set_accent_override);
}

/// All of a call's output, in the Editor: written to a file beside the sessions and opened there.
fn open_all(agent: &AgentId, key: &str) -> Result<(), String> {
    let Some((turn, index)) = parse_key(key) else { return Err("That is not a call.".into()) };
    let found = agents::store().read(|s| {
        let a = s.agent(agent)?;
        let turn = a.turns.iter().find(|t| t.n == turn)?;
        match turn.items.get(index)? {
            Item::Card(c) => Some((c.call.clone(), c.as_call().summary(), c.exit_code, c.output.all())),
            _ => None,
        }
    });
    let Some((call, summary, exit, text)) = found else {
        return Err("That call is no longer in the session.".into());
    };
    let dir = agents::dir().join("output");
    std::fs::create_dir_all(&dir).map_err(|e| format!("Could not make {}: {e}", dir.display()))?;
    let name = format!("{}-{}.txt", agent.0, call)
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' })
        .collect::<String>();
    let path = dir.join(name);
    let exit = exit.map(|c| format!(" · exit {c}")).unwrap_or_default();
    std::fs::write(&path, format!("# {summary}{exit}\n\n{text}"))
        .map_err(|e| format!("Could not write {}: {e}", path.display()))?;
    let path = path.to_string_lossy().into_owned();
    crate::wire::dock::spawn_app_with_args("editor", "yantrik-text-editor", &[&path]);
    Ok(())
}

/// `t12.3` → turn 12, item 3.
fn parse_key(key: &str) -> Option<(u64, usize)> {
    let (turn, index) = key.strip_prefix('t')?.split_once('.')?;
    Some((turn.parse().ok()?, index.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn read(relative: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
    }

    /// A screen is in five places, and a screen missing from any one of them is a screen some door
    /// cannot reach: `show_screen` refuses it, `open_app` does not know it, the launcher has no tile,
    /// the taskbar vanishes on it, or `describe` never says it exists. Problem reports shipped
    /// without the taskbar; this checks every place at once.
    #[test]
    fn the_agents_screen_is_registered_everywhere_a_screen_must_be() {
        // app.slint draws it at this id, as AgentsScreen, and keeps the taskbar on it.
        let app = read("../yantrik-ui-slint/ui/app.slint");
        let branch = format!("if current-screen == {SCREEN} : WindowFrame");
        let at = app.find(&branch).expect("app.slint draws the Agents screen at SCREEN");
        assert!(app[at..].lines().take(40).any(|l| l.trim_start().starts_with("AgentsScreen {")), "screen {SCREEN} draws AgentsScreen");
        let taskbar = app.lines().find(|l| l.contains(": Rectangle") && l.contains("current-screen == 1 ||")).expect("the taskbar's condition");
        assert!(taskbar.contains(&format!("current-screen == {SCREEN}")), "the taskbar shows on the Agents screen: {taskbar}");
        // Its window and its global are exported, or the shell cannot pop an agent out or fill one.
        assert!(app.contains("export { AgentWindow, AgentsState"), "AgentWindow and AgentsState are exported from app.slint");

        // `show_screen agents`, and describe names it.
        assert_eq!(crate::control::screen_name(SCREEN), "agents");
        let control = read("src/control.rs");
        let control = control.split("#[cfg(test)]").next().unwrap();
        assert!(control.contains(".with(\"agents\", crate::agents::for_describe())"), "describe shell lists the agents");
        assert!(control.contains("problems, agents"), "show_screen's description offers agents");

        // `open_app agents`, the listing a caller reads, and what it is for.
        use crate::wire::dock::{availability, route, Availability, Launch};
        assert_eq!(route("agents"), Some(Launch::Screen(SCREEN)));
        assert_eq!(availability("agents", &[]), Availability::Ready);
        let listed = crate::wire::dock::openable();
        let entry = listed.iter().find(|a| a["name"] == "agents").expect("open_app lists agents");
        assert!(entry["for"].as_str().is_some_and(|f| f.contains("agent")), "{entry}");

        // A launcher tile, with an icon of its own.
        assert!(crate::apps::builtin_apps().iter().any(|e| e.app_id == "agents" && e.name == "Agents"));
        let icons = read("../yantrik-ui-kit/slint/icon.slint");
        assert!(icons.contains("id == \"agents\""), "Icons.app knows agents");
    }

    #[test]
    fn a_popped_out_window_is_known_as_an_agent_by_its_title() {
        let title = window_title("pi", "tidy the files in the terminal folder");
        assert!(title.starts_with(agents::WINDOW_TITLE_PREFIX), "{title}");
        // The window list would otherwise take "files" or "terminal" from the task and mark
        // Files or Terminal as open.
        assert_eq!(crate::windows::derive_app_id(&title), "agents");
    }

    #[test]
    fn a_card_folded_is_one_line_and_open_is_everything() {
        use crate::agents::model::{Stream, Card};
        let mut card = Card::new("j1", "agent_run", "", serde_json::json!({"command": "fdupes -r ~/Pictures"}), Provenance::Verified, 0);
        for n in 0..20 {
            card.output.push(Stream::Stdout, format!("line {n}\n").as_bytes());
        }
        // Running: the line, and the live tail under it.
        let running = card_of(&card, "t1.0".into(), false);
        assert_eq!(running.call.status, "running");
        assert!(running.live);
        assert_eq!(running.output.lines().count(), LIVE_LINES);
        assert_eq!(running.more, format!("the last {LIVE_LINES} of 20 lines"));
        // Ended: one line with the exit code on it, nothing under it until opened.
        card.state = CallState::Ok;
        card.exit_code = Some(0);
        card.ended = Some(1);
        let folded = card_of(&card, "t1.0".into(), false);
        assert!(!folded.live);
        assert_eq!(folded.badge, "verified · exit 0");
        assert_eq!(folded.call.summary, r#"agent_run command="fdupes -r ~/Pictures""#);
        assert_eq!(folded.output, "");
        let open = card_of(&card, "t1.0".into(), true);
        assert!(open.call.output.contains("line 0\n") && open.call.output.contains("line 19"));
        assert!(open.call.arguments.contains("fdupes"), "the arguments in full");
    }

    #[test]
    fn keys_name_a_turn_and_an_item() {
        assert_eq!(parse_key("t12.3"), Some((12, 3)));
        assert_eq!(parse_key("t12"), None);
        assert_eq!(parse_key("earlier"), None);
    }

    #[test]
    fn times_read_as_a_person_says_them() {
        assert_eq!(duration(2), "just now");
        assert_eq!(duration(42), "42s");
        assert_eq!(duration(134), "2m");
        assert_eq!(duration(3 * 3600 + 120), "3h 2m");
        assert_eq!(thousands(412), "412");
        assert_eq!(thousands(1_500), "1.5k");
        assert_eq!(thousands(41_000), "41k");
    }
}
