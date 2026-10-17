//! The security review of #727 (5f3f849e), one test per finding, with the reviewer's inputs.

use serde_json::json;
use yantrik_app_runtime::control::AgentTokenScope;

use crate::session::Session;
use crate::surface;
use crate::tests::{act, Scratch};

/// The person opens a file outside the home, which no agent may read. The temp dir is outside
/// `$HOME` in every test run, so the file is hidden from agents by `home_paths`.
fn person_opens_a_secret(dir: &Scratch) -> Session {
    let secret = dir.path("secret.csv");
    std::fs::write(&secret, "topsecret,42\n").unwrap();
    let mut s = Session::new();
    act(&mut s, "open", json!({ "path": secret })).unwrap();
    s
}

#[test]
fn an_agent_reads_nothing_of_a_hidden_sheet_through_any_action() {
    let dir = Scratch::new("hidden");
    let mut s = person_opens_a_secret(&dir);
    let copy = dir.path("x.csv");
    {
        let _agent = AgentTokenScope::enter(Some("tok-sheets-review".into()));
        for (name, args) in [
            ("select", json!({ "cell": "A1" })),
            ("set_cell", json!({ "cell": "C1", "text": "=A1&B1" })),
            ("set_cell", json!({ "cell": "C1", "text": "=VLOOKUP(\"topsecret\",A1:B1,2)" })),
            ("undo", json!({})),
            ("redo", json!({})),
            ("get_range", json!({ "range": "A1:C1" })),
            ("save", json!({ "path": "~/Documents/x.csv" })),
            ("save", json!({ "path": copy })),
            ("save_over", json!({ "path": copy })),
            ("save_over", json!({})),
        ] {
            let e = act(&mut s, name, args).unwrap_err();
            assert!(!e.contains("topsecret"), "{name}: {e}");
            assert!(e.contains("not read or saved for an agent"), "{name}: {e}");
        }
        assert!(!copy.exists(), "nothing of the hidden sheet was written");
        assert_eq!(s.doc.sheet.raw(crate::address::Cell::new(0, 2)), "", "set_cell did not run");

        let view = serde_json::to_string(&yantrik_app_runtime::control::describe_json(
            "spreadsheet",
            &surface::describe(&s),
            &[],
        ))
        .unwrap();
        assert!(!view.contains("topsecret"), "{view}");
    }
    // The person still sees and edits their own sheet.
    let got = act(&mut s, "select", json!({ "cell": "A1" })).unwrap();
    assert_eq!(got["typed"], "topsecret");
}

#[test]
fn a_hidden_sheet_quoted_by_the_status_line_is_not_read_out() {
    let dir = Scratch::new("hidden-status");
    let mut s = person_opens_a_secret(&dir);
    // The person's own formula quotes the cell's text in its error, on the status line.
    act(&mut s, "set_cell", json!({ "cell": "C1", "text": "=A1+1" })).unwrap();
    assert!(s.status().0.contains("topsecret"));
    let _agent = AgentTokenScope::enter(Some("tok-sheets-review".into()));
    let state = yantrik_app_runtime::control::describe_json("spreadsheet", &surface::describe(&s), &[]);
    let text = state.to_string();
    assert!(!text.contains("topsecret"), "{text}");
}
