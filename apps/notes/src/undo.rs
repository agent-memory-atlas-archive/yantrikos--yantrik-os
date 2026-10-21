//! A view's undo and redo: whole texts, newest last, bounded in count and in bytes. A view's own
//! history, cleared whenever the view shows another note, so undo never crosses notes.

/// The most steps kept, and the most text they may hold together.
const STEPS: usize = 64;
const BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Default)]
pub struct History {
    undo: Vec<String>,
    redo: Vec<String>,
}

impl History {
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }

    /// An edit replaced `previous`: it can be undone, and what redo held is gone.
    pub fn edited(&mut self, previous: String) {
        remember(&mut self.undo, previous);
        self.redo.clear();
    }

    /// One step back (or forward, for `redo`) from `now`: the text to show, with `now` kept on
    /// the other side. `None` when there is nothing to take.
    pub fn step(&mut self, redo: bool, now: String) -> Option<String> {
        let (from, to) = if redo { (&mut self.redo, &mut self.undo) } else { (&mut self.undo, &mut self.redo) };
        let text = from.pop()?;
        remember(to, now);
        Some(text)
    }
}

fn remember(history: &mut Vec<String>, text: String) {
    history.push(text);
    while history.len() > STEPS || history.iter().map(String::len).sum::<usize>() > BYTES {
        history.remove(0);
    }
}
