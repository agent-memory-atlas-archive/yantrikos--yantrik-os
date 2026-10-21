//! The parity pin (step 8c): the action table and `describe` exactly as Notes published them
//! before the framework. The table below was extracted mechanically from the old
//! `apps/notes/src/main.rs` (`surface`, `act`, `arg`), not copied by hand from the new one, so a
//! word changed in `intents.rs` fails here.

use serde_json::{json, Value};
use yantrik_app::app::App as _;
use yantrik_app::intents::Intents;

use super::{act, settle, started, state, vault};
use crate::command::{Command, WindowAction};
use crate::Notes;

struct Golden {
    name: &'static str,
    risk: &'static str,
    description: &'static str,
    /// Name, required, description.
    params: &'static [(&'static str, bool, &'static str)],
}

const BEFORE: &[Golden] = &[
    Golden {
        name: "new_note",
        risk: "standard",
        description: r#"Create a note with this title, put `text` in it if given, save it, and open it. The title is written as the note's first line, `# <title>`, which is what the library lists it by; whatever note was open is saved first."#,
        params: &[
            ("title", true, r#"The new note's title. It is written as the first line, `# <title>`, and is what `open_note` and `search` find it by."#),
            ("text", false, r#"The body, written under the title on its own lines. Leave it out for a note that is only a heading; `append` can add to it later."#),
        ],
    },
    Golden {
        name: "open_note",
        risk: "standard",
        description: r#"Open an existing note by its exact title or its filename and show it in the editor. The note that was open is saved first; a note in the Trash opens read-only."#,
        params: &[
            ("title", true, r#"The note's exact title, or the filename `describe` lists it under (`note-….md`). Trashed notes are matched too."#),
        ],
    },
    Golden {
        name: "set_title",
        risk: "standard",
        description: r#"Rename the note that is open by rewriting its first `# heading` line, adding one at the top if it has none, and save it. Nothing else in the note changes."#,
        params: &[
            ("title", true, r#"The new title, on one line; any line breaks in it become spaces."#),
        ],
    },
    Golden {
        name: "append",
        risk: "standard",
        description: r#"Add text to the end of the note that is open and save it. Nothing already in the note is removed; use `set_content` to replace it instead."#,
        params: &[
            ("text", true, r#"Exactly what to add, appended to the last character already there — begin it with a newline to start it on its own line."#),
        ],
    },
    Golden {
        name: "set_content",
        risk: "sensitive",
        description: r#"Replace everything in the note that is open with this text and save it. What was there is gone from the window; `undo` takes it back while the note stays open."#,
        params: &[
            ("text", true, r#"The note's entire new text, Markdown included — keep the `# <title>` first line if the note should keep its title. An empty string empties the note."#),
        ],
    },
    Golden {
        name: "search",
        risk: "standard",
        description: r#"Filter the library list to the notes whose text or metadata contains this, without opening anything; `read_notes` returns their text. `set_folder` picks the section."#,
        params: &[
            ("query", true, r#"Matched case-insensitively against each note's text and its notebook and tags; an empty string clears the filter."#),
        ],
    },
    Golden {
        name: "read_notes",
        risk: "safe",
        description: r#"Read the text of many notes in one call, newest first: every note whose text, notebook or tags contain `query`, or all of them, without opening any. One page at a time; the answer names the `skip` for the next page."#,
        params: &[
            ("query", false, r#"Matched case-insensitively like `search`; leave it out to read every note outside the Trash."#),
            ("skip", false, r#"How many matching notes to pass over, as the last answer's `how_to_see_the_rest` says; leave it out for the first page."#),
        ],
    },
    Golden {
        name: "set_folder",
        risk: "standard",
        description: r#"Switch the library list to one section of the library. It changes what `search` and `describe.matches` list, and opens nothing."#,
        params: &[
            ("folder", true, r#"One of `all`, `recent` (touched in the last seven days), `favorites` (pinned), `trash`, or `book:<notebook name>`."#),
        ],
    },
    Golden {
        name: "notebook",
        risk: "standard",
        description: r#"File the note that is open under a notebook, which is the section `set_folder book:<name>` lists, and save it. It replaces whatever notebook it was in."#,
        params: &[
            ("text", true, r#"The notebook's name, up to 80 characters; an empty string files the note under none."#),
        ],
    },
    Golden {
        name: "tags",
        risk: "standard",
        description: r#"Replace the tags on the note that is open and save it. Tags are searched by `search` along with the note's text."#,
        params: &[
            ("text", true, r#"Comma-separated tags, e.g. `work, ideas`, up to 512 characters; an empty string clears them."#),
        ],
    },
    Golden {
        name: "save",
        risk: "standard",
        description: r#"Write the note that is open to its file now instead of waiting for the 750 ms autosave, and check that the bytes on disk are the text in the window."#,
        params: &[],
    },
    Golden {
        name: "trash",
        risk: "sensitive",
        description: r#"Move the note that is open to the Trash: it leaves the library, its file is removed from the notes directory, and `restore` puts it back."#,
        params: &[],
    },
    Golden {
        name: "restore",
        risk: "standard",
        description: r#"Put the trashed note that is open back into the library, writing its file into the notes directory again. It refuses rather than overwrite a note of the same filename."#,
        params: &[],
    },
    Golden {
        name: "copy",
        risk: "standard",
        description: r#"Duplicate the note that is open as a new note with its own filename, save that, and open it. Unsaved changes go with the copy — it is the way out of "this note changed on disk" — and the original file is left as it is."#,
        params: &[],
    },
    Golden {
        name: "reload",
        risk: "standard",
        description: r#"Re-read every note from the notes directory, picking up files changed outside this window. The open note is saved first and stays open if its file is still there."#,
        params: &[],
    },
    Golden {
        name: "preview",
        risk: "standard",
        description: r#"Turn the rendered Markdown preview of the open note on or off — it is a toggle, and the answer says which it now is. The note itself is not changed."#,
        params: &[],
    },
    Golden {
        name: "focus",
        risk: "standard",
        description: r#"Turn focus mode, which hides the library list and the side panels, on or off — it is a toggle, and the answer says which it now is."#,
        params: &[],
    },
    Golden {
        name: "undo",
        risk: "standard",
        description: r#"Take back the last change made to the note that is open in this window, and save the result. The history is cleared whenever another note is opened."#,
        params: &[],
    },
    Golden {
        name: "redo",
        risk: "standard",
        description: r#"Put back the change that `undo` took off the note that is open, and save the result. Editing the note discards what redo was holding."#,
        params: &[],
    },
    Golden {
        name: "import",
        risk: "sensitive",
        description: r#"Read a UTF-8 text or Markdown file from anywhere on disk and add its contents to the library as a new note, then open it. The file itself is left where it is."#,
        params: &[
            ("path", true, r#"Absolute path to the file to read, or one starting `~/`. Up to 256 KiB of text with no control characters."#),
        ],
    },
    Golden {
        name: "export",
        risk: "sensitive",
        description: r#"Write the open note's text to a file outside the library, at this path. It refuses rather than overwrite anything already there, and the note stays open."#,
        params: &[
            ("path", true, r#"Absolute path of the file to create, or one starting `~/`. Its folder must exist and nothing may already be at that path."#),
        ],
    },
];

#[test]
fn the_action_table_is_the_one_notes_always_published() {
    let mut table = Intents::<Notes>::new();
    Notes::intents(&mut table);
    let specs = table.specs();
    let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    let before: Vec<&str> = BEFORE.iter().map(|g| g.name).collect();
    assert_eq!(names, before, "the same actions, in the same order");
    for (spec, old) in specs.iter().zip(BEFORE) {
        assert_eq!(spec.description, old.description, "`{}`'s description", old.name);
        assert_eq!(spec.permission, old.risk, "`{}` is graded as it was", old.name);
        assert!(!spec.deferred, "`{}` answers when its work is done, as it did", old.name);
        assert!(!spec.stateless && spec.expected_seconds.is_none() && spec.explainer.is_none());
        let params: Vec<(&str, bool, &str)> =
            spec.params.iter().map(|p| (p.name.as_str(), p.required, p.description.as_str())).collect();
        assert_eq!(params, old.params.to_vec(), "`{}`'s arguments", old.name);
        for p in &spec.params {
            assert_eq!(p.kind, "string", "`{}.{}` is text, as it was", old.name, p.name);
            assert!(p.default.is_none() && p.values.is_empty() && p.items.is_none());
        }
    }
}

#[test]
fn describe_publishes_those_actions_and_the_frameworks_launch() {
    let app = started();
    let published: Vec<String> = app.describe()["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap().to_string())
        .collect();
    let mut expected: Vec<String> = BEFORE.iter().map(|g| g.name.to_string()).collect();
    // The framework's own hand-over between two launches; refused to agents, graded safe.
    expected.push("launch".into());
    assert_eq!(published, expected);
}

/// Every key `describe` gave, with its value for a library with nothing open.
#[test]
fn describe_has_the_same_keys_and_values() {
    let app = started();
    let s = state(&app);
    let keys: Vec<&str> = s.as_object().unwrap().keys().map(String::as_str).collect();
    let mut want = vec![
        "open_note", "title", "path", "unsaved", "lines", "characters", "trashed_note_open", "content",
        "metadata", "notes_directory", "library_ready", "busy", "notice", "status", "folder", "preview",
        "focus", "note_count", "trash_count", "search_query", "matches", "notes",
    ];
    want.sort_unstable();
    let mut keys = keys;
    keys.sort_unstable();
    assert_eq!(keys, want);
    assert_eq!(
        s,
        json!({
            "open_note": null, "title": null, "path": null, "unsaved": false, "lines": null,
            "characters": null, "trashed_note_open": false, "content": null, "metadata": null,
            "notes_directory": vault(&app).display().to_string(), "library_ready": true, "busy": false,
            "notice": "", "status": "All changes saved", "folder": "all", "preview": false, "focus": false,
            "note_count": 0, "trash_count": 0, "search_query": "", "matches": 0, "notes": [],
        })
    );
    assert_eq!(app.describe()["summary"], "Notes — nothing open, 0 notes");
}

/// The summary in every state it has a sentence for.
#[test]
fn the_summary_says_what_it_always_said() {
    let app = yantrik_testing::TestApp::<Notes>::start();
    assert_eq!(
        app.describe()["summary"],
        "Notes — the library has not opened · working",
        "before the first read lands"
    );
    settle(&app);
    act(&app, "new_note", json!({ "title": "Groceries", "text": "milk
eggs
bread" })).unwrap();
    assert_eq!(app.describe()["summary"], "Notes — \"Groceries\" open (5 lines, saved), 1 note");
    app.run(Command::Edit("# Groceries
".into()));
    assert_eq!(app.describe()["summary"], "Notes — \"Groceries\" open (1 line, unsaved), 1 note");
    super::autosave(&app);
    let s = state(&app);
    assert_eq!(s["title"], "Groceries");
    assert_eq!(s["unsaved"], false);
    assert_eq!(s["lines"], 1);
    assert_eq!(s["content"], "# Groceries
");
    assert_eq!(s["metadata"], "");
    assert_eq!(s["matches"], 1);
    let file = s["open_note"].as_str().unwrap().to_string();
    assert_eq!(s["path"], vault(&app).join(&file).display().to_string());
    assert_eq!(s["notes"], json!([{ "filename": file, "title": "Groceries", "trash": false, "pinned": false, "notebook": "" }]));

    app.run(Command::Window(WindowAction::Trash));
    settle(&app);
    assert_eq!(
        app.describe()["summary"],
        "Notes — nothing open, 0 notes, 1 in trash · Moved to Trash. You can restore it from the library."
    );
    app.run(Command::Window(WindowAction::Open { id: file, trash: true }));
    assert_eq!(app.describe()["summary"], "Notes — \"Groceries\" open from Trash (1 line, deleted), 0 notes, 1 in trash");
    let s: Value = state(&app);
    assert_eq!(s["trashed_note_open"], true);
    assert_eq!(s["status"], "Recoverable deletion");
}
