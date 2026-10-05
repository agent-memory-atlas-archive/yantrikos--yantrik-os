//! Tests for approval_bounds.rs: each rule a request is refused by before a card is raised, and
//! the cut after escaping that keeps the card a fixed size (third security review of #639).
use super::*;
use serde_json::json;

const PADDED_RM: &str = "echo tidying up the temporary build files now, one moment ; rm -rf ~";

#[test]
fn a_key_that_is_too_long_or_hides_characters_is_refused() {
    let long = "k".repeat(KEY_CHARS + 1);
    let why = refusal(&json!({ long: "x" }), None, false).expect("(a) a 41-character name is refused");
    assert!(why.contains("41 characters"), "{why}");
    assert!(refusal(&json!({ "k".repeat(KEY_CHARS): "x" }), None, false).is_none(), "40 is allowed");
    for hidden in ["pid\nUndo", "p\u{202E}id", "pi\u{200B}d", "a\\b"] {
        let why = refusal(&json!({ hidden: "1" }), None, false).unwrap_or_else(|| panic!("(a) {hidden:?} is refused"));
        assert!(why.contains("escapes"), "{why}");
    }
}

#[test]
fn an_argument_the_app_does_not_publish_is_refused() {
    let published = vec!["pid".to_string(), "force".to_string()];
    let why = refusal(&json!({ "pid": 1, "note": "x" }), Some(&published), false).expect("(b) refused");
    assert!(why.contains("`note`"), "{why}");
    assert!(refusal(&json!({ "pid": 1, "force": true }), Some(&published), false).is_none());
    // An app that publishes no parameters for the action takes none.
    assert!(refusal(&json!({ "pid": 1 }), Some(&[]), false).is_some());
    // Where the parameters are not to hand (the shell's own surface), the other rules still hold.
    assert!(refusal(&json!({ "pid": 1 }), None, false).is_none());
}

#[test]
fn rows_too_long_to_draw_are_refused() {
    // Eight values of control characters, each escaped nine-fold: past the budget.
    let args: serde_json::Map<String, serde_json::Value> =
        (0..8).map(|i| (format!("a{i}"), json!("\u{E0041}".repeat(60)))).collect();
    let why = refusal(&serde_json::Value::Object(args), None, false).expect("(c) refused");
    assert!(why.contains("characters as the card would draw them"), "{why}");
    // At the budget, allowed: eight rows of exactly TOTAL_CHARS / 8 escaped characters, with
    // forty-character names and control characters in every value.
    let per_row = TOTAL_CHARS / 8;
    let args: serde_json::Map<String, serde_json::Value> = (0..8)
        .map(|i| {
            let key = format!("{i}{}", "k".repeat(KEY_CHARS - 1));
            let value = "\u{1}".repeat(3) + &"v".repeat(per_row - KEY_CHARS - 2 - 3 * 8);
            (key, json!(value))
        })
        .collect();
    let rows = approvals::args_rows(&serde_json::Value::Object(args.clone()));
    assert!(rows.iter().all(|r| visible(r).chars().count() == per_row), "{rows:?}");
    assert!(refusal(&serde_json::Value::Object(args.clone()), None, false).is_none(), "exactly the budget is shown");
    // One character more is refused.
    let mut over = args;
    over.insert("0".repeat(1), json!("x"));
    assert!(refusal(&serde_json::Value::Object(over), None, false).is_some());
    // Eight ordinary sixty-character values under short names are asked about.
    let ordinary: serde_json::Map<String, serde_json::Value> =
        (0..8).map(|i| (format!("arg{i}"), json!("v".repeat(approvals::ARG_VALUE_CHARS)))).collect();
    assert!(refusal(&serde_json::Value::Object(ordinary), None, false).is_none());
}

/// (d), and the pre-existing hole it closes: `echo … ; rm -rf ~` padded past the sixty characters
/// a card shows of a value hid its end everywhere on the card.
#[test]
fn a_destructive_card_shows_a_value_whole_or_is_not_asked() {
    assert!(PADDED_RM.chars().count() > approvals::ARG_VALUE_CHARS);
    let why = refusal(&json!({ "command": PADDED_RM }), None, true).expect("(d) refused on a destructive card");
    assert!(why.contains("cannot be taken back"), "{why}");
    // Escapes count: twenty newlines are forty characters drawn.
    assert!(refusal(&json!({ "command": "\n".repeat(31) }), None, true).is_some());
    assert!(refusal(&json!({ "command": "rm -rf ~/x" }), None, true).is_none());
    // The same value on an ordinary card is asked about, and shown cut under its marker.
    assert!(refusal(&json!({ "command": PADDED_RM }), None, false).is_none());
    let rows = approvals::args_rows(&json!({ "command": PADDED_RM }));
    assert!(rows[0].contains("characters in full)") && !rows[0].contains("rm -rf ~"), "{rows:?}");
    let c = crate::approval_wording::consequences("agent_run", crate::approval_wording::Published::app("Run it."), "Run it.", "", &rows);
    assert!(c.exactly.contains("characters in full)"), "the marker reaches the card: {}", c.exactly);
    // And the refusal says why, in one sentence.
    assert!(refused("x").starts_with("this request is too long to put in front of a person in full, so nothing was asked: "));
}

/// Defence in depth: whatever reaches the card is cut after escaping, keys included, so its worst
/// case is a fixed size.
#[test]
fn what_is_drawn_is_cut_after_escaping() {
    let huge = format!("{}: {}", "k".repeat(500), "\u{E0041}".repeat(60));
    let row = clip_row(&visible(&huge));
    assert!(row.chars().count() <= ROW_CHARS + 40, "{}", row.chars().count());
    assert!(row.contains("characters in full)"));
    let rows: Vec<String> = (0..9).map(|_| huge.clone()).collect();
    let c = crate::approval_wording::consequences("x", crate::approval_wording::Published::app(""), "", "", &rows);
    assert!(c.exactly.chars().count() <= TOTAL_CHARS + 40, "{}", c.exactly.chars().count());
}
