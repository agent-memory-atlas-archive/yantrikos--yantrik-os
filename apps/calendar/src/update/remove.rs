//! Taking an event off the calendar: the trash icon on a row, `delete_event`, and
//! `delete_own_event`.
//!
//! The one delete path. All three end in [`spawn_delete`], so none can report an outcome the
//! others would not, and there is only one place where the service's answer is read. Whatever
//! happened, the window is redrawn from the store: a delete that failed may still have been
//! preceded by somebody else's successful one.

use serde_json::json;
use yantrik_app::Refused;

use super::{apply_after_write, Fx};
use crate::backend::calls;
use crate::command::{Command, Door, Kept, Removed, Work};
use crate::model::{Model, NOTHING, PART_FORM, PART_NOTICE};
use crate::notice;
use crate::ownership;
use crate::work;

pub fn update(m: &mut Model, command: Command, fx: &mut Fx) -> Result<(), Refused> {
    match command {
        Command::DeleteRow(row) => {
            // The row index is a position in the list the agenda was drawn from, which is this
            // one. Every row of a day used to carry the id 0, so the trash icon on any of them
            // deleted the first event on that day (617dac9).
            fx.changed(NOTHING);
            let Some(event) = m.on_selected_day().get(row as usize).map(|e| (*e).clone()) else {
                return Ok(());
            };
            if m.removing.contains(&event.id) {
                return Ok(());
            }
            m.removing.push(event.id.clone());
            let named = format!("“{}”", event.title);
            let (api, sequence, here) = (m.machine.api.clone(), m.reads.sequence.clone(), m.visible_range());
            fx.spawn_or(Command::WorkFailed(Work::Remove(Some(event.id.clone()))), move |_| {
                let result = work::delete_through(api.as_ref(), &event.id);
                let listing = work::reload(api.as_ref(), &sequence, here);
                Command::Removed(Box::new(Removed { id: event.id, named, result, listing }))
            });
        }
        Command::Delete { ask, own, caller } => {
            m.answer = serde_json::Value::Null;
            fx.changed(NOTHING);
            // Who this caller was verified to be, for the own-creation rule. Only the own door
            // asks: `delete_event` reaches any event, and the person sees a card for it.
            let me = if own { m.machine.who.identify(&caller) } else { None };
            let (api, sequence, here) = (m.machine.api.clone(), m.reads.sequence.clone(), m.visible_range());
            fx.spawn_or(Command::WorkFailed(Work::Remove(None)), move |_| {
                let (id, named) = match work::named_event(api.as_ref(), &ask) {
                    Ok(found) => found,
                    Err(e) => return Command::Declined(e),
                };
                if own {
                    // The record the service kept at creation, read back from the event's own
                    // file — so the check survives this app restarting between the create and
                    // this delete, which the arena's reset can (#201).
                    let event = match calls::get(api.as_ref(), &id) {
                        Ok(event) => event,
                        Err(e) => return Command::Declined(e),
                    };
                    if !ownership::may_delete_unasked(event.creator.as_deref(), me.as_deref()) {
                        let why = own_delete_refused(&named, event.creator.as_deref(), me.as_deref());
                        return Command::Kept(Box::new(Kept {
                            refused: notice::Refused::Delete,
                            title: event.title,
                            who: me,
                            id,
                            why,
                        }));
                    }
                }
                let result = work::delete_through(api.as_ref(), &id);
                let listing = work::reload(api.as_ref(), &sequence, here);
                Command::Removed(Box::new(Removed { id, named, result, listing }))
            });
        }
        Command::Removed(removed) => {
            let Removed { id, named, result, listing } = *removed;
            m.removing.retain(|r| r != &id);
            match result {
                Ok(title) => {
                    m.notice.say("");
                    m.answer = json!({ "deleted": title, "id": id });
                }
                // On screen as well as in the answer. A delete that did not happen leaves a row
                // where it was, and a row that stayed put has to say which of the two things it
                // means: "it is still there", and why.
                Err(e) => {
                    m.notice.say(format!("Could not delete {named}: {e}"));
                    fx.settle(Err(Refused::new("not_deleted", e)));
                }
            }
            fx.changed(PART_NOTICE);
            apply_after_write(m, fx, listing);
        }
        other => unreachable!("not a delete command: {other:?}"),
    }
    Ok(())
}

/// The sentence `delete_own_event` refuses with: the rule, and the door that asks a person.
fn own_delete_refused(named: &str, creator: Option<&str>, me: Option<&str>) -> String {
    let who = me.unwrap_or("nobody this machine could identify");
    let why = match creator {
        Some(made_by) => format!(
            "{named} was created by {made_by} and this call is {who}: only the caller that \
             created an event may delete it without a person being asked"
        ),
        None => format!(
            "{named} has no creator on record — it is older than the record, or a person made it \
             in the window — and this call is {who}: only the caller that created an event may \
             delete it without a person being asked"
        ),
    };
    format!("{why}. Any event comes off through `delete_event`, which asks first")
}

/// A job that ended without a result: a panic, or the pool at its bound. Before, a panic took the
/// window down; now the person is told, and what waited on it is let go.
pub fn work_failed(m: &mut Model, work: Work, fx: &mut Fx) {
    let sentence = match work {
        Work::Save(door) => {
            if door == Door::Form {
                m.form.saving = false;
                fx.changed(PART_FORM);
            }
            "Saving the event did not finish; try again."
        }
        Work::Remove(id) => {
            if let Some(id) = id {
                m.removing.retain(|r| r != &id);
            }
            "The delete did not finish; try again."
        }
        Work::Change => "The change did not finish; try again.",
        Work::Open => "Opening that event did not finish; try again.",
        // A read for a caller, who is told by the framework; nothing on screen waited on it.
        Work::Between => {
            fx.changed(NOTHING);
            return;
        }
    };
    m.notice.say(sentence);
    fx.changed(PART_NOTICE);
}
