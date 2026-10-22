//! What an agent can learn of a snippet's code, and what it can make Snippets read: the security
//! fixes of `fix/snippets-agent-exposure`. A snippet may be a password, a key or a token, so a
//! sentinel in its code is followed through what an agent reads.
//!
//! - `describe` carries no line of any snippet's code (the list's `preview` was its first line);
//! - an agent's `search` matches titles, tags and language, never code, so its counts cannot be
//!   used to read a secret back a character at a time;
//! - an agent's `import` names only a file in the person's home, outside its protected places
//!   (`home_paths`, #443, as Images' `open`).

use serde_json::{json, Value};
use yantrik_testing::Call;

use super::{open, Fixture};
use crate::Command;

const SENTINEL: &str = "SENTINEL-agent-7f3a";

/// An agent's call, through the real dispatch, answered once its work has landed.
fn as_agent(f: &Fixture, name: &str, args: Value) -> Result<Value, String> {
    let pending = f.app.act_as(Call::new(name, args).as_agent("mind"))?;
    f.quiet();
    pending.settle()
}

fn a_secret(f: &Fixture) -> i32 {
    f.keep("Deploy key", "Shell", &format!("export TOKEN={SENTINEL}\n# rotated {SENTINEL}"), "deploy, secrets")
}

#[test]
fn describe_carries_no_line_of_any_snippets_code() {
    let f = open();
    let id = a_secret(&f);
    f.act("open", json!({ "id": id.to_string() })).unwrap();
    let d = f.describe();
    assert!(!d.to_string().contains(SENTINEL), "{d}");
    let row = d["state"]["snippets"][0].clone();
    assert_eq!(row["title"], json!("Deploy key"), "what it is called stays");
    assert_eq!(row["tags"], json!(["deploy", "secrets"]));
    assert!(row.get("preview").is_none(), "{row}");
    // The window, the person's own, still shows the first line in the list.
    assert!(f.app.ui(|ui| slint::Model::row_data(&ui.get_snippets(), 0).unwrap().preview.to_string()).contains(SENTINEL));
}

#[test]
fn an_agents_search_never_matches_code() {
    let f = open();
    a_secret(&f);
    f.keep("Journal", "Shell", "journalctl -f", "logs");
    for probe in [SENTINEL, "TOKEN=S", "rotated", "export"] {
        let answer = as_agent(&f, "search", json!({ "query": probe })).unwrap();
        assert_eq!(answer["matching"], json!(0), "{probe}: {answer}");
        assert_eq!(f.state()["matching"], json!(0), "{probe}: and the list it leaves says the same");
    }
    // What a snippet is called still finds it.
    for probe in ["deploy key", "secrets", "shell"] {
        let answer = as_agent(&f, "search", json!({ "query": probe })).unwrap();
        assert!(answer["matching"].as_u64().unwrap() >= 1, "{probe}: {answer}");
    }
    // The person's own search matches the code, from `yos` and in the window.
    let answer = f.act("search", json!({ "query": SENTINEL })).unwrap();
    assert_eq!(answer["matching"], json!(1));
    f.app.run(Command::Search(format!("TOKEN={SENTINEL}")));
    assert_eq!(f.app.ui(|ui| slint::Model::row_count(&ui.get_snippets())), 1);
    assert!(!f.describe().to_string().contains("\"preview\""), "no preview, whoever searched");
}

#[test]
fn an_agents_import_reads_only_a_file_in_the_home() {
    let dir = std::env::temp_dir().join(format!("snippets-agent-import-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let outside = dir.join("credentials.env");
    std::fs::write(&outside, format!("API_KEY={SENTINEL}\n")).unwrap();
    assert!(!outside.starts_with(yantrik_foundation::place::home()), "the test's file is outside the home");
    let f = open();

    let e = as_agent(&f, "import", json!({ "path": outside.display().to_string() })).unwrap_err();
    assert!(e.starts_with("an agent may import only a file in the person's home"), "{e}");
    let protected = as_agent(&f, "import", json!({ "path": "~/.ssh/id_snippets_probe" })).unwrap_err();
    assert!(protected.starts_with("an agent may import only a file in the person's home"), "{protected}");
    assert_eq!(f.state()["total"], json!(0), "nothing was read in");
    assert_eq!(f.notice(), "", "a refusal to the agent, not a failure on the person's screen");
    assert!(!f.describe().to_string().contains(SENTINEL));

    // The person names any file they can read, as before.
    let answer = f.act("import", json!({ "path": outside.display().to_string() })).unwrap();
    assert_eq!(answer["imported"], json!(1));
    let _ = std::fs::remove_dir_all(&dir);
}
