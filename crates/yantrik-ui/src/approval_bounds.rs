//! What a request may be, so that the approval card can show all of it — and the refusal when it
//! cannot.
//!
//! # Why this exists
//!
//! Third security review of #639: the card pins every argument above its buttons, which is what
//! keeps a mind from hiding `&& rm -rf ~/x` behind a cut line — and it made the arguments' size the
//! card's size. A value is cut to sixty characters before its control characters are shown as
//! escapes, and each escape is up to nine characters (`<U+E0041>`); a key is never cut at all. So a
//! caller could send arguments that push Decline and Allow off the screen, and hold it: the shell
//! keeps itself in front while a card waits, and an expiry is not a denial.
//!
//! Two layers, both here:
//!
//! 1. [`refusal`] — at request time, before anything is raised or sent: a request the card could
//!    not show in full is not asked at all, and the caller is told which rule it broke. It changes
//!    what may be ASKED, never what a grant is: binding, single use, the TTLs and spending are the
//!    approvals store's and are untouched.
//! 2. [`clip_row`] and [`clip_rows`] — at drawing time, after escaping, keys included, to the same
//!    limits, so the card's worst case is a fixed size whatever reaches it.

use crate::approval_wording::visible;
use crate::approvals;

/// The longest argument name a card shows. Published parameter names on this desktop are a word
/// or two (`pid`, `channel`, `args_json`).
pub const KEY_CHARS: usize = 40;

/// The longest one argument row may be once escaped: `key: value`, a forty-character key and a
/// value the card shows in full.
pub const ROW_CHARS: usize = 120;

/// What all the rows together may come to once escaped. Eight sixty-character values under short
/// names (the most `args_rows` shows of ordinary arguments, ~580 characters) fit under it; at the
/// card's 14px on its ~380px width it is about twelve lines (~220px), which with every other
/// pinned line at its worst — three discrepancies, the open-ended warning, the session row — keeps
/// the card inside the top-right corner's ~684px and its buttons inside the Lens panel.
pub const TOTAL_CHARS: usize = 640;

/// Why a request cannot be put in front of a person in full, or `None` when it can.
///
/// - (a) an argument name longer than [`KEY_CHARS`], or with a character [`visible`] would escape;
/// - (b) an argument the app does not publish, when the app publishes its parameters — the
///   action's own dispatch would refuse the call anyway (`check_arguments`);
/// - (c) the rows, escaped, longer than [`TOTAL_CHARS`] together;
/// - (d) on a destructive card (graded dangerous, or it cannot be undone — the reading the red
///   button is drawn from), a value that the card would cut: its escaped form is longer than the
///   value bound `args_rows` cuts at. A cut is acceptable on an ordinary card, under its "(N
///   characters in full)" marker; on a card whose action cannot be taken back, the cut-off part
///   is exactly where `; rm -rf ~` goes.
pub fn refusal(args: &serde_json::Value, params: Option<&[String]>, destructive: bool) -> Option<String> {
    if let Some(map) = args.as_object() {
        let mut keys: Vec<&String> = map.keys().collect();
        keys.sort();
        for key in &keys {
            if key.chars().count() > KEY_CHARS {
                return Some(format!("an argument name is {} characters long, and a card shows names of at most {KEY_CHARS}", key.chars().count()));
            }
            if visible(key) != **key {
                return Some(format!("the argument name `{}` carries characters a card can only show as escapes", visible(key)));
            }
            if let Some(published) = params {
                if !published.iter().any(|p| p == *key) {
                    return Some(format!("`{key}` is not an argument the app publishes for this action"));
                }
            }
        }
        if destructive {
            for key in &keys {
                let value = &map[*key];
                let shown = match value {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                let escaped = visible(&shown).chars().count();
                if escaped > approvals::ARG_VALUE_CHARS {
                    return Some(format!(
                        "`{key}` is {escaped} characters as the card would draw it, and on a card for an action \
                         that cannot be taken back a value is shown whole or not asked about (at most {})",
                        approvals::ARG_VALUE_CHARS
                    ));
                }
            }
        }
    }
    let total: usize = approvals::args_rows(args).iter().map(|row| visible(row).chars().count()).sum();
    if total > TOTAL_CHARS {
        return Some(format!("the arguments come to {total} characters as the card would draw them, past the {TOTAL_CHARS} a card shows"));
    }
    None
}

/// The sentence a refused request is answered with.
pub fn refused(rule: &str) -> String {
    format!("this request is too long to put in front of a person in full, so nothing was asked: {rule}")
}

/// One escaped row cut to [`ROW_CHARS`], the cut named. Defence in depth: [`refusal`] already
/// turned away anything longer, so on a request that was asked this changes nothing.
pub fn clip_row(escaped: &str) -> String {
    clip(escaped, ROW_CHARS)
}

/// The rows joined as the card's "Exactly:" line, cut to [`TOTAL_CHARS`], the cut named.
pub fn clip_rows(joined: &str) -> String {
    clip(joined, TOTAL_CHARS)
}

fn clip(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max).collect();
    format!("{head}\u{2026} ({count} characters in full)")
}

#[cfg(test)]
#[path = "approval_bounds_tests.rs"]
mod approval_bounds_tests;
