//! Yantrik Calendar — standalone app binary.
//!
//! Communicates with `calendar-service` via JSON-RPC IPC.
//! Falls back to local event storage when service is unavailable.

use std::cell::RefCell;
use std::rc::Rc;

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use yantrik_app_runtime::prelude::*;
use yantrik_ipc_contracts::calendar::{
    method, CreateEventParams, DeleteEventParams, EventsParams,
};

mod views;
use views::ViewMode;

slint::include_modules!();

/// How long an event runs when nothing says otherwise.
const DEFAULT_EVENT_MINUTES: i32 = 60;

/// Fill the agent rail from the day on screen.
///
/// Events come off the model the app already renders; memory comes from the companion when it
/// is reachable. Nothing is added to fill the column -- a day with nothing on it and no shell
/// behind it gets an empty rail, and the calendar is wider for it.
fn refresh_agent_rail(ui: &CalendarApp) {
    let mut context: Vec<AgentContextItem> = Vec::new();
    for e in ui.get_events_today().iter() {
        context.push(AgentContextItem {
            id: format!("event:{}", e.id).into(),
            label: e.title.clone(),
            detail: e.time_text.clone(),
            source: "calendar".into(),
        });
    }
    ui.set_agent_context(ModelRc::new(VecModel::from(context)));

    let online = companion::is_online();
    let mut next: Vec<AgentSuggestion> = Vec::new();
    if online {
        next.push(AgentSuggestion {
            id: "explain".into(),
            label: "What does this day look like?".into(),
            detail: "shape of the day, and what to prepare".into(),
            icon: "spark".into(),
            running: ui.get_ai_is_working(),
            // An answer to read; nothing is moved, so nothing is proposed.
            proposes: false,
        });
    }
    if ui.get_selected_day() > 0 {
        next.push(AgentSuggestion {
            id: "today".into(),
            label: "Back to today".into(),
            detail: SharedString::new(),
            icon: "search".into(),
            running: false,
            proposes: false,
        });
    }
    ui.set_agent_suggestions(ModelRc::new(VecModel::from(next)));

    ui.set_agent_unavailable(if online {
        SharedString::new()
    } else {
        "Not connected. Start the Yantrik shell for suggestions.".into()
    });
}

fn main() {
    init_tracing("yantrik-calendar");

    // One window per app: a second launch defers to the running one (the shell focuses it).
    let Some(_instance) = instance::claim("calendar") else { return };

    let app = CalendarApp::new().unwrap();

    // Same dark/accent choice as the shell, read from the shell's settings file.
    let theme = theme::load();
    app.global::<ThemeMode>().set_dark(theme.dark);
    app.global::<AccentPreset>().set_index(theme.accent_index);

    wire(&app);

    // The "now" line on the week and day grids, kept at now.
    //
    // Those grids are one pixel to the minute, so the line has to be told the minute as well as
    // the hour or it stands up to an hour away from where the person is; and a window left open
    // past midnight would go on drawing yesterday's line. A minute is as often as a line that
    // moves a pixel a minute can usefully be redrawn.
    let clock = slint::Timer::default();
    {
        let weak = app.as_weak();
        clock.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_secs(60),
            move || {
                if let Some(ui) = weak.upgrade() {
                    set_clock(&ui);
                }
            },
        );
    }

    app.run().unwrap();
}

/// Where "now" is, and which clock it is on.
///
/// The timezone strip in the new-event form says the offset and no more, because the offset is
/// all `chrono::Local` carries -- there is no zone database in this dependency set. It was an
/// empty string nothing ever wrote, so the form never said which clock a time typed into it
/// belonged to.
fn set_clock(ui: &CalendarApp) {
    let now = chrono::Local::now();
    ui.set_current_hour(now.hour() as i32);
    ui.set_current_minute(now.minute() as i32);
    ui.set_cal_timezone_display(views::timezone_label(now.offset().local_minus_utc()).into());
}

// ── Calendar state ───────────────────────────────────────────────────

#[derive(Clone)]
struct CalState {
    year: i32,
    month: u32,
    events: Vec<CalEvent>,
    /// The date range `events` was read for, or `None` when the last read failed and the next
    /// redraw should ask again rather than show an empty calendar for the life of the process.
    range: Option<(chrono::NaiveDate, chrono::NaiveDate)>,
    /// How long the event the form is about should run.
    ///
    /// The form has no duration field -- it asks for a title, a date, a time and notes -- so a
    /// template's answer to "how long" waits here until Save. `DEFAULT_EVENT_MINUTES` otherwise.
    new_event_duration_min: i32,
}

#[derive(Clone, Debug)]
struct CalEvent {
    id: String,
    title: String,
    start: String,
    end: String,
    #[allow(dead_code)]
    notes: String,
    is_all_day: bool,
    /// Position in `PALETTE`, not a colour: the same event has to be coloured the same in the
    /// agenda list and on the week grid, and `views` -- which has no Slint in it -- carries this
    /// through the derivation.
    color_index: usize,
}

/// The colours an event is drawn in, by its position in what the store returned.
const PALETTE: [(u8, u8, u8); 5] = [
    (0x4E, 0x79, 0xA7),
    (0xF2, 0x8E, 0x2C),
    (0xE1, 0x57, 0x59),
    (0x76, 0xB7, 0xB2),
    (0x59, 0xA1, 0x4F),
];

fn palette(index: usize) -> slint::Color {
    let (r, g, b) = PALETTE[index % PALETTE.len()];
    slint::Color::from_rgb_u8(r, g, b)
}

// ── Service wrappers ─────────────────────────────────────────────────

/// Every event overlapping `from..=to`.
///
/// The range is a parameter because the week view needs one. The app asked for exactly the month
/// on screen, and a week straddling a month boundary is the common case at both ends of every
/// month: the first week of October would have been drawn with nothing on 28, 29 or 30 September
/// and said nothing about it. `views::visible_range` decides what to ask for.
fn fetch_events_in_range(
    from: chrono::NaiveDate,
    to: chrono::NaiveDate,
) -> Result<Vec<CalEvent>, String> {
    let client = service::client("calendar")?;
    let params = EventsParams {
        start_date: format!("{}T00:00:00", from.format("%Y-%m-%d")),
        end_date: format!("{}T23:59:59", to.format("%Y-%m-%d")),
    };
    let result = client
        .call(method::EVENTS, serde_json::to_value(params).map_err(|e| e.to_string())?)
        .map_err(|e| e.message)?;
    let svc_events: Vec<yantrik_ipc_contracts::calendar::CalendarEvent> =
        serde_json::from_value(result).map_err(|e| e.to_string())?;

    Ok(svc_events.iter().enumerate().map(|(i, e)| CalEvent {
        id: e.id.clone(),
        title: e.title.clone(),
        start: e.start.clone(),
        end: e.end.clone(),
        notes: e.description.clone(),
        is_all_day: e.is_all_day,
        color_index: i,
    }).collect())
}

fn create_event_via_service(title: &str, start: &str, end: &str, notes: &str) -> Result<String, String> {
    let client = service::client("calendar")?;
    let params = CreateEventParams {
        title: title.to_string(),
        start: start.to_string(),
        end: end.to_string(),
        description: notes.to_string(),
        location: None,
        color: String::new(),
        // The form has no field for either, and this is not the place to add one. They are
        // carried so the mind's calendar tools — which do accept both — can reach the same store.
        is_all_day: false,
        attendees: Vec::new(),
    };
    let result = client
        .call(method::CREATE_EVENT, serde_json::to_value(params).map_err(|e| e.to_string())?)
        .map_err(|e| e.message)?;
    // The stored event, with the id the store gave it — the proof it landed, not a hope.
    let event: yantrik_ipc_contracts::calendar::CalendarEvent =
        serde_json::from_value(result).map_err(|e| e.to_string())?;
    Ok(event.id)
}

fn delete_event_via_service(event_id: &str) -> Result<(), String> {
    let client = service::client("calendar")?;
    let params = DeleteEventParams { id: event_id.to_string() };
    client
        .call(method::DELETE_EVENT, serde_json::to_value(params).map_err(|e| e.to_string())?)
        .map_err(|e| e.message)?;
    Ok(())
}

// ── Date helpers ─────────────────────────────────────────────────────

/// Column index of a date in the month grid: 0=Sunday .. 6=Saturday.
///
/// This MUST match the header row in calendar.slint, which is Sun-first. The previous
/// hand-rolled Zeller returned a Monday-first index and the grid used it as the number of
/// leading blanks, so every month was drawn one column to the left of the truth.
fn day_of_week_for_date(year: i32, month: u32, day: u32) -> u32 {
    chrono::NaiveDate::from_ymd_opt(year, month, day)
        .map(|d| d.weekday().num_days_from_sunday())
        .unwrap_or(0)
}

fn today() -> (i32, u32, u32) {
    let now = chrono::Local::now();
    (now.year(), now.month(), now.day())
}

use chrono::{Datelike, Timelike};

fn build_month_grid(year: i32, month: u32, events: &[CalEvent], today_day: Option<u32>) -> Vec<CalendarDay> {
    let first_dow = day_of_week_for_date(year, month, 1);
    let last = views::last_day_of_month(year, month);
    let mut cells = Vec::with_capacity(42);

    // Empty cells before month start
    for _ in 0..first_dow {
        cells.push(CalendarDay {
            day_number: 0,
            is_today: false,
            is_selected: false,
            is_current_month: false,
            has_events: false,
            event_count: 0,
        });
    }

    for d in 1..=last as i32 {
        let day_str = format!("{:04}-{:02}-{:02}", year, month, d);
        let ev_count = events.iter().filter(|e| e.start.starts_with(&day_str)).count() as i32;
        cells.push(CalendarDay {
            day_number: d,
            is_today: today_day == Some(d as u32),
            is_selected: false,
            is_current_month: true,
            has_events: ev_count > 0,
            event_count: ev_count,
        });
    }

    // Pad to 42 cells (6 weeks)
    while cells.len() < 42 {
        cells.push(CalendarDay {
            day_number: 0,
            is_today: false,
            is_selected: false,
            is_current_month: false,
            has_events: false,
            event_count: 0,
        });
    }
    cells
}

/// The agenda in the sidebar: what is on one day, in the order the store returned it.
///
/// The row's `id` is its position in this list, because that is what `delete-event` hands back
/// and what `on_delete_event` indexes with. It was the literal 0 on every row, so the trash icon
/// on any row of a day deleted the first one.
fn events_for_day(events: &[CalEvent], year: i32, month: u32, day: i32) -> Vec<CalendarEvent> {
    let prefix = format!("{:04}-{:02}-{:02}", year, month, day);
    events.iter().filter(|e| e.start.starts_with(&prefix)).enumerate().map(|(row, e)| {
        let time_text = if e.is_all_day {
            "All day".to_string()
        } else {
            // Hours and minutes. The seconds are in the store because the store keeps ISO
            // timestamps, and nobody reading their own day needs "14:00:00 - 15:00:00".
            let clock = |iso: &str| {
                iso.split('T').nth(1).unwrap_or("").split(':').take(2).collect::<Vec<_>>().join(":")
            };
            format!("{} – {}", clock(&e.start), clock(&e.end))
        };
        CalendarEvent {
            id: row as i32,
            title: e.title.clone().into(),
            date_text: prefix.clone().into(),
            time_text: time_text.into(),
            color: palette(e.color_index),
            is_all_day: e.is_all_day,
        }
    }).collect()
}

/// The events in hand, in the shape the pure view code works in.
fn source_events(events: &[CalEvent]) -> Vec<views::SourceEvent> {
    events
        .iter()
        .map(|e| views::SourceEvent {
            title: e.title.clone(),
            start: e.start.clone(),
            end: e.end.clone(),
            is_all_day: e.is_all_day,
            color_index: e.color_index,
        })
        .collect()
}

/// Derived blocks, turned into the rows the two time grids draw.
fn time_events(blocks: &[views::TimeEvent]) -> Vec<CalendarTimeEvent> {
    blocks
        .iter()
        .map(|b| CalendarTimeEvent {
            title: b.title.clone().into(),
            start_hour: b.start_hour,
            start_min: b.start_min,
            duration_min: b.duration_min,
            day_index: b.day_index,
            color: palette(b.color_index),
        })
        .collect()
}

/// How many of the events in hand fall in the month on screen.
///
/// Not `events.len()`: the week view widens what is fetched past the month's edges, and counting
/// everything fetched would report events the month grid is not drawing.
fn events_in_month(events: &[CalEvent], year: i32, month: u32) -> usize {
    let prefix = format!("{:04}-{:02}", year, month);
    events.iter().filter(|e| e.start.starts_with(&prefix)).count()
}

// ── Wire all callbacks ───────────────────────────────────────────────

// ── The control surface ──────────────────────────────────────────────
//
// "What is on my calendar today" should never be answered by photographing a month grid and
// asking a vision model to read the numbers. See `yantrik_app_runtime::control`.

/// One block of a time grid as a caller reads it: what, when, and for how long.
fn block_json(block: &CalendarTimeEvent) -> serde_json::Value {
    serde_json::json!({
        "title": block.title.to_string(),
        "at": format!("{:02}:{:02}", block.start_hour, block.start_min),
        "minutes": block.duration_min,
    })
}

fn count_phrase(n: usize, one: &str, many: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {many}") }
}

/// Everything on screen, re-derived from the events the app is holding.
///
/// One function rather than a redraw written out again in each callback, because the week and the
/// day were left out of every one of them: `on_switch_view` set `view-mode` and nothing else, so
/// pressing Week moved to a grid holding whatever it had been given last, which was nothing at
/// all. Anything that changes the month, the selected day, the view, or what is stored ends here.
fn render(ui: &CalendarApp, state: &Rc<RefCell<CalState>>) {
    let s = state.borrow();
    let (this_year, this_month, this_day) = today();
    let today_day =
        if s.year == this_year && s.month == this_month { Some(this_day) } else { None };
    let day = ui.get_selected_day();

    ui.set_month_title(format!("{} {}", views::month_name(s.month), s.year).into());
    ui.set_days(ModelRc::new(VecModel::from(build_month_grid(
        s.year,
        s.month,
        &s.events,
        today_day,
    ))));
    ui.set_events_today(ModelRc::new(VecModel::from(events_for_day(
        &s.events, s.year, s.month, day,
    ))));

    // The same events, placed on the hour grids. `views` decides where each one goes; this side
    // only turns the answer into Slint rows.
    let source = source_events(&s.events);
    let selected = views::selected_date(s.year, s.month, day);

    let week = views::week_view(&source, selected);
    let labels: Vec<SharedString> =
        week.labels.iter().map(|l| SharedString::from(l.as_str())).collect();
    ui.set_week_day_labels(ModelRc::new(VecModel::from(labels)));
    ui.set_week_events(ModelRc::new(VecModel::from(time_events(&week.events))));

    let day_view = views::day_view(&source, selected);
    ui.set_day_events(ModelRc::new(VecModel::from(time_events(&day_view.events))));
    ui.set_day_view_title(day_view.title.into());

    drop(s);
    set_clock(ui);
    refresh_agent_rail(ui);
}

/// Redraw, reading the store again first when the range on screen has moved.
///
/// A day clicked inside the month already in hand needs no round trip; a month stepped, or a week
/// view opened on a week that runs past the month's edge, does.
fn refresh(ui: &CalendarApp, state: &Rc<RefCell<CalState>>) {
    let wanted = {
        let s = state.borrow();
        views::visible_range(
            s.year,
            s.month,
            ViewMode::from_index(ui.get_view_mode()),
            ui.get_selected_day(),
        )
    };
    let held = state.borrow().range;
    if held != Some(wanted) {
        match fetch_events_in_range(wanted.0, wanted.1) {
            Ok(events) => {
                let mut s = state.borrow_mut();
                s.events = events;
                s.range = Some(wanted);
            }
            Err(e) => {
                // The range is left unrecorded on purpose: the next redraw asks again, instead
                // of a calendar that failed one read once staying empty until it is restarted.
                tracing::warn!(error = %e, "could not read the calendar");
                let mut s = state.borrow_mut();
                s.events.clear();
                s.range = None;
            }
        }
    }
    render(ui, state);
}

/// Redraw, reading the store again whatever the range. For after something has been written.
fn reload(ui: &CalendarApp, state: &Rc<RefCell<CalState>>) {
    state.borrow_mut().range = None;
    refresh(ui, state);
}

/// Put an event on the calendar and show it, or say why not.
///
/// The single path behind the form's Save button and the `add_event` action, so neither can
/// report an outcome it did not get. The action used to answer `{"added": ...}` the moment it
/// had handed the title to the window, and the window's own save dropped the service's error on
/// the floor — which is how a calendar that was storing nothing told every caller it had.
fn store_event(
    ui: &CalendarApp,
    state: &Rc<RefCell<CalState>>,
    title: &str,
    date: &str,
    time: &str,
    notes: &str,
) -> Result<String, String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("an event needs a title".into());
    }
    // How long it runs is the state's, not the form's: the form has no duration field, and a
    // template pressed a moment ago has already said 15 or 30 or 120. The arithmetic that turns
    // a date, a clock time and a length into two ISO timestamps is in `views` so it can be
    // tested -- it is where 23:30 plus an hour used to become "24:30", which is not a time.
    let duration = state.borrow().new_event_duration_min;
    let (start, end) = views::start_and_end(date, time, duration)
        .ok_or_else(|| format!("`{date} {time}` is not a date and a time"))?;

    let id = create_event_via_service(title, &start, &end, notes)?;
    state.borrow_mut().new_event_duration_min = DEFAULT_EVENT_MINUTES;
    reload(ui, state);
    Ok(id)
}

fn publish_control(app: &CalendarApp, state: Rc<RefCell<CalState>>) {
    use yantrik_app_runtime::control::{Action, App, Param, View};

    let describe = {
        let weak = app.as_weak();
        let st = state.clone();
        move || {
            let Some(ui) = weak.upgrade() else {
                return View::new("Calendar — closing");
            };
            let month = ui.get_month_title().to_string();
            let day = ui.get_selected_day();

            let today_model = ui.get_events_today();
            let today: Vec<serde_json::Value> = (0..today_model.row_count())
                .filter_map(|i| today_model.row_data(i))
                .map(|e| {
                    serde_json::json!({
                        "title": e.title.to_string(),
                        "date": e.date_text.to_string(),
                        "time": e.time_text.to_string(),
                        "all_day": e.is_all_day,
                    })
                })
                .collect();

            // Which days of the month have anything on them. Six numbers instead of a picture of
            // a grid, and it is what a caller planning around the month actually needs.
            let grid = ui.get_days();
            let busy: Vec<serde_json::Value> = (0..grid.row_count())
                .filter_map(|i| grid.row_data(i))
                .filter(|d| d.is_current_month && d.has_events)
                .map(|d| serde_json::json!({ "day": d.day_number, "events": d.event_count }))
                .collect();

            let s = st.borrow();
            let view = ViewMode::from_index(ui.get_view_mode());
            let selected = views::selected_date(s.year, s.month, day);
            let (week_start, week_end) = views::week_bounds(selected);

            // What the view on screen is showing, read off the models it is drawn from.
            //
            // A mind asking what is on the calendar used to be told about the month whichever
            // view was up, because the month was the only view with anything in it. Week and Day
            // now answer as themselves: the week says its range and what each of its seven days
            // holds; the day says its date and its events in order.
            let week_blocks = ui.get_week_events();
            let day_blocks = ui.get_day_events();
            let labels = ui.get_week_day_labels();

            let summary = match view {
                ViewMode::Week => format!(
                    "Calendar — week of {week_start} to {week_end}, {} on the grid",
                    count_phrase(week_blocks.row_count(), "event", "events")
                ),
                ViewMode::Day => format!(
                    "Calendar — {}, {} on the grid",
                    ui.get_day_view_title(),
                    count_phrase(day_blocks.row_count(), "event", "events")
                ),
                ViewMode::Month if today.is_empty() => {
                    format!("Calendar — {month}, nothing on day {day}")
                }
                ViewMode::Month if today.len() == 1 => format!(
                    "Calendar — {month}, one thing on day {day}: {}",
                    today[0]["title"].as_str().unwrap_or_default()
                ),
                ViewMode::Month => {
                    format!("Calendar — {month}, {} things on day {day}", today.len())
                }
            };

            let mut out = View::new(summary)
                .with("month", month)
                .with("year", s.year)
                .with("month_number", s.month as i64)
                .with("selected_day", day)
                .with("view", view.as_str())
                .with("events_on_selected_day", serde_json::Value::Array(today))
                .with("days_with_events", serde_json::Value::Array(busy))
                .with("events_this_month", events_in_month(&s.events, s.year, s.month) as i64)
                // What the person is being told went wrong, if anything. A caller that just
                // failed to save should be able to read the reason rather than infer it.
                .with("notice", ui.get_notice().to_string());

            match view {
                ViewMode::Week => {
                    let per_day: Vec<serde_json::Value> = (0..labels.row_count())
                        .map(|column| {
                            let events: Vec<serde_json::Value> = week_blocks
                                .iter()
                                .filter(|b| b.day_index == column as i32)
                                .map(|b| block_json(&b))
                                .collect();
                            serde_json::json!({
                                "day": labels.row_data(column).unwrap_or_default().to_string(),
                                "events": events,
                            })
                        })
                        .collect();
                    out = out
                        .with("week_start", week_start.to_string())
                        .with("week_end", week_end.to_string())
                        .with("week", serde_json::Value::Array(per_day));
                }
                ViewMode::Day => {
                    let events: Vec<serde_json::Value> =
                        day_blocks.iter().map(|b| block_json(&b)).collect();
                    out = out
                        .with("day_shown", ui.get_day_view_title().to_string())
                        .with("events_on_day_grid", serde_json::Value::Array(events));
                }
                ViewMode::Month => {}
            }
            out
        }
    };

    let weak = app.as_weak();
    let ui_for = move || weak.upgrade().ok_or_else(|| "Calendar window is gone".to_string());

    let add_state = state.clone();
    let view_state = state.clone();
    let day_ui = ui_for.clone();
    let move_ui = ui_for.clone();
    let today_ui = ui_for.clone();
    let add_ui = ui_for.clone();
    let view_ui = ui_for;

    App::new("calendar")
        .describe(describe)
        .action(
            Action::new("select_day", "Show what is on one day of the month shown")
                .arg(Param::number("day").describe("Day of the month, 1-31")),
            move |args| {
                let ui = day_ui()?;
                let day = args["day"].as_i64().ok_or("`day` must be a number")? as i32;
                if !(1..=31).contains(&day) {
                    return Err(format!("{day} is not a day of the month"));
                }
                ui.invoke_day_clicked(day);
                let model = ui.get_events_today();
                let titles: Vec<String> = (0..model.row_count())
                    .filter_map(|i| model.row_data(i))
                    .map(|e| e.title.to_string())
                    .collect();
                Ok(serde_json::json!({ "day": day, "events": titles }))
            },
        )
        .action(
            Action::new("show_month", "Move to the next or previous month")
                .arg(Param::text("direction").describe("next | previous")),
            move |args| {
                let ui = move_ui()?;
                match args["direction"].as_str().unwrap_or_default().to_lowercase().as_str() {
                    "next" | "forward" => ui.invoke_next_month(),
                    "previous" | "prev" | "back" => ui.invoke_prev_month(),
                    other => return Err(format!("`direction` is next or previous, not `{other}`")),
                }
                Ok(serde_json::json!({ "showing": ui.get_month_title().to_string() }))
            },
        )
        .action(Action::new("go_to_today", "Jump back to the current month and day"), move |_| {
            let ui = today_ui()?;
            ui.invoke_today_pressed();
            Ok(serde_json::json!({
                "showing": ui.get_month_title().to_string(),
                "day": ui.get_selected_day(),
            }))
        })
        .action(
            Action::new("add_event", "Put something on the calendar")
                .arg(Param::text("title"))
                .arg(Param::text("date").describe("YYYY-MM-DD"))
                .arg(Param::text("time").describe("HH:MM, 24-hour"))
                .arg(Param::text("notes").optional()),
            move |args| {
                let ui = add_ui()?;
                let title = args["title"].as_str().unwrap_or_default().trim().to_string();
                let date = args["date"].as_str().unwrap_or_default().trim().to_string();
                let time = args["time"].as_str().unwrap_or_default().trim().to_string();
                if title.is_empty() {
                    return Err("`title` is empty".into());
                }
                // Checked here rather than let the service reject a malformed timestamp: the
                // error a caller can act on names the format it should have used.
                if date.len() != 10 || date.matches('-').count() != 2 {
                    return Err(format!("`date` should look like 2026-09-06, not `{date}`"));
                }
                if !time.contains(':') {
                    return Err(format!("`time` should look like 14:30, not `{time}`"));
                }
                let notes = args["notes"].as_str().unwrap_or_default().to_string();
                // Stored before answering, and the answer carries the id it was stored under,
                // so "added" cannot be a guess about what the window did next. A failure is put
                // on screen as well as returned: when a mind tries to put something on the
                // calendar and cannot, the person watching the window is owed the reason too.
                let id = match store_event(&ui, &add_state, &title, &date, &time, &notes) {
                    Ok(id) => id,
                    Err(e) => {
                        ui.set_notice(format!("Could not save “{title}”: {e}").into());
                        return Err(e);
                    }
                };
                ui.set_notice(SharedString::new());
                Ok(serde_json::json!({ "added": title, "on": format!("{date} {time}"), "id": id }))
            },
        )
        .action(
            Action::new("set_view", "Switch between the month, week and day views")
                .arg(Param::text("view").describe("month | week | day")),
            move |args| {
                let ui = view_ui()?;
                let mode = match args["view"].as_str().unwrap_or_default().to_lowercase().as_str() {
                    "month" => 0,
                    "week" => 1,
                    "day" => 2,
                    other => return Err(format!("unknown view `{other}`; use month, week or day")),
                };
                ui.invoke_switch_view(mode);
                // What is on screen now, not the word that was asked for. This answered
                // `{"view": "week"}` while the week grid was empty of everything -- no events, no
                // column headers, a "now" line on midnight -- which reads as a view that was
                // shown. It is the same fabrication `add_event` was making in September, one
                // layer up.
                Ok(match ViewMode::from_index(mode) {
                    ViewMode::Week => {
                        let s = view_state.borrow();
                        let (start, end) = views::week_bounds(views::selected_date(
                            s.year,
                            s.month,
                            ui.get_selected_day(),
                        ));
                        serde_json::json!({
                            "view": "week",
                            "week_start": start.to_string(),
                            "week_end": end.to_string(),
                            "events": ui.get_week_events().row_count(),
                        })
                    }
                    ViewMode::Day => serde_json::json!({
                        "view": "day",
                        "day": ui.get_day_view_title().to_string(),
                        "events": ui.get_day_events().row_count(),
                    }),
                    ViewMode::Month => {
                        let s = view_state.borrow();
                        serde_json::json!({
                            "view": "month",
                            "showing": ui.get_month_title().to_string(),
                            "events": events_in_month(&s.events, s.year, s.month),
                        })
                    }
                })
            },
        )
        .serve();
}

fn wire(app: &CalendarApp) {
    let (ty, tm, td) = today();
    let state = Rc::new(RefCell::new(CalState {
        year: ty,
        month: tm,
        events: Vec::new(),
        range: None,
        new_event_duration_min: DEFAULT_EVENT_MINUTES,
    }));

    // Initial load. The selected day goes on first because the range the week view needs is
    // decided by it.
    app.set_selected_day(td as i32);
    refresh(app, &state);

    // Published once the first read is done, so the first `app.describe` reports real events.
    publish_control(app, state.clone());

    // ── Prev month ──
    {
        let weak = app.as_weak();
        let st = state.clone();
        app.on_prev_month(move || {
            let Some(ui) = weak.upgrade() else { return };
            {
                let mut s = st.borrow_mut();
                if s.month == 1 {
                    s.month = 12;
                    s.year -= 1;
                } else {
                    s.month -= 1;
                }
            }
            // Nothing is picked in the month just arrived at; the week and day views read that
            // as the first of it. See `views::selected_date`.
            ui.set_selected_day(0);
            refresh(&ui, &st);
        });
    }

    // ── Next month ──
    {
        let weak = app.as_weak();
        let st = state.clone();
        app.on_next_month(move || {
            let Some(ui) = weak.upgrade() else { return };
            {
                let mut s = st.borrow_mut();
                if s.month == 12 {
                    s.month = 1;
                    s.year += 1;
                } else {
                    s.month += 1;
                }
            }
            ui.set_selected_day(0);
            refresh(&ui, &st);
        });
    }

    // ── Day clicked ──
    {
        let weak = app.as_weak();
        let st = state.clone();
        app.on_day_clicked(move |day| {
            let Some(ui) = weak.upgrade() else { return };
            ui.set_selected_day(day);
            // Not just the agenda: picking a day moves the day view onto it and the week view
            // onto the week around it, and a day in the first or last week of the month needs
            // days the month fetch did not ask for.
            refresh(&ui, &st);
        });
    }

    // ── Add event (open form) ──
    {
        let weak = app.as_weak();
        let st = state.clone();
        app.on_add_event(move || {
            let Some(ui) = weak.upgrade() else { return };
            let date = {
                let mut s = st.borrow_mut();
                // A blank form is an hour long; a template says otherwise and is honoured once.
                s.new_event_duration_min = DEFAULT_EVENT_MINUTES;
                views::selected_date(s.year, s.month, ui.get_selected_day())
            };
            ui.set_event_date(date.format("%Y-%m-%d").to_string().into());
            ui.set_event_time("09:00".into());
            ui.set_event_title(SharedString::default());
            ui.set_event_notes(SharedString::default());
            ui.set_show_event_form(true);
        });
    }

    // ── A template pressed ──
    //
    // The four templates belong to the screen; pressing one hands over the two things the app
    // needs, the title to start from and how long the thing runs. That is the whole of what
    // "use a template" can honestly mean while the form has a title, a date, a time and notes
    // and nothing else: the title is filled in, and the minutes wait in the state until Save.
    // The handler used to log "Use template (standalone mode)" and return.
    {
        let weak = app.as_weak();
        let st = state.clone();
        app.on_cal_use_template(move |title, duration_min| {
            let Some(ui) = weak.upgrade() else { return };
            let date = {
                let mut s = st.borrow_mut();
                s.new_event_duration_min =
                    if duration_min > 0 { duration_min } else { DEFAULT_EVENT_MINUTES };
                views::selected_date(s.year, s.month, ui.get_selected_day())
            };
            ui.set_event_date(date.format("%Y-%m-%d").to_string().into());
            ui.set_event_time("09:00".into());
            ui.set_event_title(title);
            ui.set_event_notes(SharedString::default());
            ui.set_notice(SharedString::new());
            ui.set_show_event_form(true);
        });
    }

    // ── Save event ──
    {
        let weak = app.as_weak();
        let st = state.clone();
        app.on_save_event(move |title, date, time, notes| {
            let Some(ui) = weak.upgrade() else { return };
            match store_event(&ui, &st, &title, &date, &time, &notes) {
                Ok(_) => {
                    ui.set_notice(SharedString::new());
                    ui.set_show_event_form(false);
                }
                // The form stays open holding what was typed. Closing it on a failed save threw
                // the event away twice: once from the store, once from the screen.
                Err(e) => ui.set_notice(format!("Could not save “{}”: {e}", title.trim()).into()),
            }
        });
    }

    // ── Delete event ──
    {
        let weak = app.as_weak();
        let st = state.clone();
        app.on_delete_event(move |idx| {
            let Some(ui) = weak.upgrade() else { return };
            let s = st.borrow();
            let day = ui.get_selected_day();
            let prefix = format!("{:04}-{:02}-{:02}", s.year, s.month, day);
            let day_events: Vec<&CalEvent> = s.events.iter()
                .filter(|e| e.start.starts_with(&prefix)).collect();
            let idx = idx as usize;
            if idx >= day_events.len() { return; }
            let event_id = day_events[idx].id.clone();
            let event_title = day_events[idx].title.clone();
            drop(s);

            match delete_event_via_service(&event_id) {
                Ok(()) => {
                    ui.set_notice(SharedString::new());
                    reload(&ui, &st);
                }
                // A row that stayed on screen after a delete used to mean either "it is still
                // there" or "the store never heard"; now it means the first, and says the second.
                Err(e) => ui.set_notice(format!("Could not delete “{event_title}”: {e}").into()),
            }
        });
    }

    // ── Cancel event form ──
    {
        let weak = app.as_weak();
        app.on_cancel_event_form(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_show_event_form(false);
            }
        });
    }

    // ── Today pressed ──
    {
        let weak = app.as_weak();
        let st = state.clone();
        app.on_today_pressed(move || {
            let Some(ui) = weak.upgrade() else { return };
            let (ny, nm, nd) = today();
            {
                let mut s = st.borrow_mut();
                s.year = ny;
                s.month = nm;
            }
            ui.set_selected_day(nd as i32);
            refresh(&ui, &st);
        });
    }

    // ── Switch view ──
    {
        let weak = app.as_weak();
        let st = state.clone();
        app.on_switch_view(move |mode| {
            let Some(ui) = weak.upgrade() else { return };
            ui.set_view_mode(mode);
            // This used to be the whole handler. Week and Day are drawn from the same events the
            // month is, but nothing derived them and nothing fetched the days a week needs when
            // it runs past the month's edge, so both views were an empty drawing.
            refresh(&ui, &st);
        });
    }

    // ── The agent layer ──
    //
    // Calendar had no companion connection at all: its AI button logged a line and returned,
    // which is the state fifteen of the sixteen apps were in. The rail and the card follow the
    // same rule Notes does -- every row is something this app or the companion actually holds,
    // and says where it came from.
    {
        let weak = app.as_weak();
        app.on_ai_explain_pressed(move || {
            let Some(ui) = weak.upgrade() else { return };
            let day = if ui.get_selected_day() > 0 {
                format!("{} {}", ui.get_month_title(), ui.get_selected_day())
            } else {
                ui.get_month_title().to_string()
            };

            // What the day actually holds, read off the model rather than described to the
            // model second-hand.
            let events: Vec<String> = ui
                .get_events_today()
                .iter()
                .map(|e| format!("- {} ({})", e.title, e.time_text))
                .collect();

            ui.set_ai_is_working(true);
            ui.set_proposal_working(true);
            ui.set_proposal(AgentProposal {
                title: "Your day".into(),
                source: format!("from {day}").into(),
                ..Default::default()
            });

            let prompt = if events.is_empty() {
                format!(
                    "My calendar for {day} is empty. In two sentences, say so plainly and \
                     suggest one useful thing to do with an open day. Do not invent \
                     appointments."
                )
            } else {
                format!(
                    "Here is my calendar for {day}:\n{}\n\nIn at most four short lines, tell \
                     me what the shape of this day is and what to prepare. Use only what is \
                     listed; invent nothing.",
                    events.join("\n")
                )
            };

            let back = ui.as_weak();
            std::thread::spawn(move || {
                let outcome = companion::ask(&prompt);
                let _ = back.upgrade_in_event_loop(move |ui| {
                    ui.set_ai_is_working(false);
                    ui.set_proposal_working(false);
                    match outcome {
                        Ok(text) => ui.set_proposal(AgentProposal {
                            title: "Your day".into(),
                            body: text.into(),
                            source: format!("from {day}").into(),
                            // Reading, not changing. The card gives this one button.
                            impact: SharedString::new(),
                            destructive: false,
                            verb: "Close".into(),
                        }),
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
                // One code path: the rail presses the same button the toolbar does.
                "explain" => ui.invoke_ai_explain_pressed(),
                "today" => ui.invoke_today_pressed(),
                other => tracing::warn!(id = other, "unknown rail suggestion"),
            }
        });
    }
    // A row in the rail is an event on the day already on screen, so there is nowhere for a
    // click to go. It stays empty rather than being given something to do for the sake of it.
    app.on_agent_context_activated(|_| {});
    {
        let weak = app.as_weak();
        app.on_ai_dismiss(move || {
            if let Some(ui) = weak.upgrade() {
                // The header's AI button is a toggle and the second press has to put away what
                // the first press put up. The answer is in the proposal card; this handler was
                // empty, so pressing AI again closed nothing and left the card on the grid.
                ui.set_proposal_working(false);
                ui.set_proposal(AgentProposal::default());
            }
        });
    }
}
