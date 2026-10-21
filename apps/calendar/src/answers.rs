//! The answer each intent is owed, settled in the command where its own work lands.
//!
//! An intent's reply is read from the model when it settles. If it settled only once nothing of
//! it was left running, a forced re-read under the same invocation could leave the reply waiting
//! while something else — the person's trash icon, another call — wrote the model, and the Mind
//! would be answered about an event it never named (review of #921, finding 1). So each intent
//! settles in the command that carries its own result: a write when the store's answer lands, a
//! read when its read lands. Nothing can land between the answer being written and being read.

use serde_json::{json, Value};
use yantrik_app::effects::Effects;

use crate::events;
use crate::model::Model;
use crate::views::{self, ViewMode};
use crate::Calendar;

/// What a reading intent answers with, once its read has landed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Asked {
    SelectDay,
    /// The date as it was asked for.
    ShowDate(String),
    ShowMonth,
    Today,
    SetView,
}

/// Remember what the invocation this command runs under is owed, if it came through the surface.
pub fn ask(m: &mut Model, fx: &Effects<Calendar>, asked: Asked) {
    if let Some(id) = fx.invocation() {
        m.asked.insert(id, asked);
    }
}

/// Settle the invocation this command runs under with `answer`, if one is owed. The reply reads
/// `m.answer` when this command commits, before anything else can change it.
pub fn settle(m: &mut Model, fx: &mut Effects<Calendar>, answer: Value) {
    if fx.invocation().is_some() {
        m.answer = answer;
        fx.settle(Ok(()));
    }
}

/// A read landed under an invocation: answer the reading intent that asked for it.
pub fn read_landed(m: &mut Model, fx: &mut Effects<Calendar>) {
    let Some(asked) = fx.invocation().and_then(|id| m.asked.remove(&id)) else { return };
    let answer = reading(m, asked);
    settle(m, fx, answer);
}

/// A read under an invocation ended without a result: the framework refuses the call, and what it
/// was owed is forgotten.
pub fn read_failed(m: &mut Model, fx: &Effects<Calendar>) {
    if let Some(id) = fx.invocation() {
        m.asked.remove(&id);
    }
}

/// What the window shows now, in the shape the intent always answered with.
fn reading(m: &Model, asked: Asked) -> Value {
    match asked {
        Asked::SelectDay => json!({ "day": m.selected_day, "events": m.titles_on_selected_day() }),
        Asked::ShowDate(date) => {
            // The week the date is in, on this window's week start: in the week view that is what
            // is now on screen, and the date is in it.
            let (week_start, week_end) = m.week_bounds();
            json!({
                "showing": m.month_title(),
                "date": date,
                "week_start": week_start.to_string(),
                "week_end": week_end.to_string(),
                "events": m.titles_on_selected_day(),
            })
        }
        Asked::ShowMonth => json!({ "showing": m.month_title() }),
        Asked::Today => json!({ "showing": m.month_title(), "day": m.selected_day }),
        Asked::SetView => shown(m),
    }
}

/// What `set_view` answers: what is on screen now, not the word that was asked for. This answered
/// `{"view": "week"}` while the week grid was empty of everything -- no events, no column headers,
/// a "now" line on midnight -- which reads as a view that was shown. It is the same fabrication
/// `add_event` was making in September, one layer up.
fn shown(m: &Model) -> Value {
    let source = events::source_events(&m.events);
    match m.view {
        ViewMode::Week => {
            let (start, end) = m.week_bounds();
            json!({
                "view": "week",
                "week_start": start.to_string(),
                "week_end": end.to_string(),
                "events": views::week_view(&source, m.selected_date(), m.week_start).events.len(),
            })
        }
        ViewMode::Day => {
            let day = views::day_view(&source, m.selected_date());
            json!({ "view": "day", "day": day.title, "events": day.events.len() })
        }
        ViewMode::Month => json!({
            "view": "month",
            "showing": m.month_title(),
            "events": events::events_in_month(&m.events, m.year, m.month),
        }),
    }
}
