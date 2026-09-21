//! Shared token streaming — the chat panel's pump, used by on_send_message and on_lens_submit.
//!
//! Each answer gets a bubble and a 16ms timer that appends whatever arrives to that bubble until
//! the stream ends.
//!
//! # Why a list of streams and not one
//!
//! There used to be one slot holding one timer, and starting a second answer replaced it. Since
//! the pump's receiver lives in the timer's closure, replacing the timer dropped the receiver of
//! the answer still arriving — and the sender on the other side is the harness host's channel for
//! that turn. The host saw the send fail, dropped the turn, and refused every chunk after it:
//! "turn 18 is not one this harness was given".
//!
//! A mind working on a long task and a person typing something else meanwhile is not an edge
//! case. It is exactly what an approval is: Hermes asked to run a command, the person answered
//! `/approve` — and that answer killed the task that was waiting for it. Everything the agent said
//! for the next twenty minutes had nowhere to go, and the conversation looked dead while the work
//! carried on.
//!
//! So every stream keeps its own pump and its own bubble, and a stream ends only when its own
//! answer ends. Finished pumps are pruned when the next one starts.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use slint::{Model, ModelRc, SharedString, Timer, TimerMode, VecModel};

use crate::bridge::CompanionBridge;
use crate::markdown;
use crate::{App, ContentBlock, MessageData};

/// One running pump: whether it has finished, and whatever has to stay alive for it to run.
struct Stream {
    done: Rc<Cell<bool>>,
    /// The timer driving this pump. Held as `Any` only so the bookkeeping below can be tested
    /// without a Slint event loop to own a real `Timer`.
    _driver: Box<dyn std::any::Any>,
}

/// The answers arriving on screen right now.
///
/// Held by the callback that starts streams, so the timers outlive the call that created them.
#[derive(Clone, Default)]
pub struct Streams(Rc<RefCell<Vec<Stream>>>);

impl Streams {
    pub fn new() -> Self {
        Self::default()
    }

    /// How many answers are still arriving.
    pub fn active(&self) -> usize {
        self.0.borrow().iter().filter(|s| !s.done.get()).count()
    }

    fn remember(&self, done: Rc<Cell<bool>>, timer: Timer) {
        self.keep(done, Box::new(timer));
    }

    fn keep(&self, done: Rc<Cell<bool>>, driver: Box<dyn std::any::Any>) {
        let mut streams = self.0.borrow_mut();
        // A finished pump has nothing left to do; dropping its timer here is the cleanup that
        // used to happen by replacing the single slot. A pump still running is left alone, which
        // is the whole point.
        streams.retain(|s| !s.done.get());
        streams.push(Stream { done, _driver: driver });
    }
}

/// Start streaming tokens from the companion into the message model.
pub fn start_ai_stream(
    ui_weak: slint::Weak<App>,
    bridge: &Arc<CompanionBridge>,
    text: &str,
    streams: &Streams,
) {
    stream_into(ui_weak, bridge.send_message(text.to_string()), text, streams);
}

/// The same thing, from whatever is answering.
///
/// Split out because the body had the builtin companion welded into it: it called
/// `bridge.send_message` itself, so every message anyone typed went to the builtin no matter
/// which mind the picker said was driving. The harness host, the socket, the picker and the
/// status-bar chip were all built and all correct, and the conversation walked straight past
/// them. Selecting a mind changed a label.
pub fn stream_into(
    ui_weak: slint::Weak<App>,
    token_rx: crossbeam_channel::Receiver<String>,
    text: &str,
    streams: &Streams,
) {
    let row = match open_bubbles(&ui_weak, Some(text)) {
        Some(row) => row,
        None => return,
    };
    pump(ui_weak, token_rx, row, streams);
}

/// Start a proactive AI stream — only the assistant's response is shown (no user bubble).
/// Used for morning brief and other proactive messages where the AI speaks first.
pub fn start_proactive_stream(
    ui_weak: slint::Weak<App>,
    bridge: &Arc<CompanionBridge>,
    hidden_prompt: &str,
    streams: &Streams,
) {
    let Some(row) = open_bubbles(&ui_weak, None) else {
        return;
    };
    pump(ui_weak, bridge.send_message(hidden_prompt.to_string()), row, streams);
}

/// Add the bubbles an answer streams into. Returns the row of the assistant's bubble.
///
/// The row, not "the last row": with two answers arriving at once, the last row belongs to
/// whichever started most recently, and the other one would write its words into it. Rows are
/// only ever appended, so an index stays pointing at the same message.
fn open_bubbles(ui_weak: &slint::Weak<App>, asked: Option<&str>) -> Option<usize> {
    let ui = ui_weak.upgrade()?;
    let messages = ui.get_messages();
    let model = messages.as_any().downcast_ref::<VecModel<MessageData>>()?;
    if let Some(text) = asked {
        model.push(MessageData {
            role: "user".into(),
            content: SharedString::from(text),
            is_streaming: false,
            blocks: ModelRc::default(),
        });
    }
    model.push(MessageData {
        role: "assistant".into(),
        content: "".into(),
        is_streaming: true,
        blocks: ModelRc::default(),
    });
    ui.set_is_generating(true);
    ui.set_is_thinking(true);
    ui.set_companion_status("thinking".into());
    // Readable from the companion worker thread, which cannot touch a Slint property and must
    // not put an unprompted message into a transcript that is mid-answer.
    crate::wire::notifications::note_answer_started();
    if asked.is_some() {
        // Shown, not merely recorded.
        ui.set_lens_chat_mode(true);
    }
    Some(model.row_count() - 1)
}

/// Poll one answer at 60fps and append it to its own bubble.
fn pump(
    ui_weak: slint::Weak<App>,
    token_rx: crossbeam_channel::Receiver<String>,
    row: usize,
    streams: &Streams,
) {
    let done_flag = Rc::new(Cell::new(false));
    let finished = done_flag.clone();
    let mine = streams.clone();
    let replace_next = Rc::new(RefCell::new(false));

    let timer = Timer::default();
    timer.start(TimerMode::Repeated, Duration::from_millis(16), move || {
        let mut done = false;
        while let Ok(token) = token_rx.try_recv() {
            if token == "__DONE__" {
                done = true;
                break;
            }
            // __REPLACE__: the next token replaces the whole message content (used when tool
            // calls are detected, to strip raw XML).
            if token == "__REPLACE__" {
                *replace_next.borrow_mut() = true;
                continue;
            }
            if let Some(ui) = ui_weak.upgrade() {
                let messages = ui.get_messages();
                let Some(model) = messages.as_any().downcast_ref::<VecModel<MessageData>>() else {
                    continue;
                };
                if let Some(mut bubble) = model.row_data(row) {
                    if *replace_next.borrow() {
                        bubble.content = SharedString::from(&token);
                        *replace_next.borrow_mut() = false;
                    } else {
                        let mut content = bubble.content.to_string();
                        content.push_str(&token);
                        bubble.content = SharedString::from(&content);
                    }
                    model.set_row_data(row, bubble);
                }
            }
        }
        if done {
            finished.set(true);
            crate::wire::notifications::note_answer_ended();
            if let Some(ui) = ui_weak.upgrade() {
                let messages = ui.get_messages();
                if let Some(model) = messages.as_any().downcast_ref::<VecModel<MessageData>>() {
                    if let Some(mut bubble) = model.row_data(row) {
                        bubble.is_streaming = false;
                        bubble.blocks = parse_content_blocks(&bubble.content);
                        model.set_row_data(row, bubble);
                    }
                }
                // Still thinking, if something else is. The spinner belongs to the panel, not to
                // one answer, and turning it off here would hide work that is still running.
                if mine.active() == 0 {
                    ui.set_is_generating(false);
                    ui.set_is_thinking(false);
                    ui.set_companion_status("idle".into());
                }
            }
        }
    });
    streams.remember(done_flag, timer);
}

/// Parse message content into styled blocks for rich rendering.
fn parse_content_blocks(content: &SharedString) -> ModelRc<ContentBlock> {
    let text = content.to_string();
    if text.trim().is_empty() {
        return ModelRc::default();
    }

    let parsed = markdown::parse_blocks(&text);
    let blocks: Vec<ContentBlock> = parsed
        .into_iter()
        .map(|b| ContentBlock {
            block_type: SharedString::from(b.block_type),
            text: SharedString::from(b.text.as_str()),
        })
        .collect();

    ModelRc::new(VecModel::from(blocks))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Starting an answer must not end one that is still arriving.
    ///
    /// The single slot did exactly that, and the cost was not a visual glitch: the pump's
    /// receiver died with it, the harness host saw its send fail and dropped the turn, and the
    /// mind's next twenty minutes of work had nowhere to go.
    #[test]
    fn a_new_answer_does_not_end_one_still_arriving() {
        let streams = Streams::new();
        let first = Rc::new(Cell::new(false));
        let second = Rc::new(Cell::new(false));
        streams.keep(first.clone(), Box::new(()));
        streams.keep(second.clone(), Box::new(()));
        assert_eq!(streams.active(), 2, "both answers are still arriving");

        first.set(true);
        let third = Rc::new(Cell::new(false));
        streams.keep(third, Box::new(()));
        assert_eq!(streams.active(), 2, "the finished one is gone; the live one is not");
        assert_eq!(streams.0.borrow().len(), 2, "a finished pump is dropped, not kept forever");
    }

    /// The spinner belongs to the panel: it stops when nothing is arriving, not when any one
    /// answer finishes.
    #[test]
    fn the_panel_is_still_thinking_while_anything_is_arriving() {
        let streams = Streams::new();
        let slow = Rc::new(Cell::new(false));
        let quick = Rc::new(Cell::new(false));
        streams.keep(slow.clone(), Box::new(()));
        streams.keep(quick.clone(), Box::new(()));
        quick.set(true);
        assert_eq!(streams.active(), 1);
        slow.set(true);
        assert_eq!(streams.active(), 0);
    }
}
