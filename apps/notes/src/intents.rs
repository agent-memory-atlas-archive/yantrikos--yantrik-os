//! What a mind may ask Notes by name: the twenty-one actions worded, argued and graded as they
//! always were, and the three choices about an outside change (step 8d), each the command its
//! window action runs.
//!
//! A description is written at the definition site of every action, and [`act`] and [`arg`]
//! refuse to build one that is a placeholder: every description used to be `"Notes: {name}"`
//! and every argument optional, which is why a model asked for a note titled Groceries made a
//! note with no title. `yos describe` prints each description and each argument's type, required
//! flag and description, so what is written here is verbatim what a model reads.
//!
//! Every call is answered once its work has landed (`Settle::Explicit`): "it worked" is what the
//! disk says afterwards, never the handler's opinion of itself.

use std::time::Duration;

use serde_json::Value;
use yantrik_app::control::{Action, Param};
use yantrik_app::intents::{Args, Intent, Intents, Settle};
use yantrik_app::Refused;

use crate::command::{Act, Command};
use crate::model::Model;
use crate::Notes;

/// How long a call may wait for its saves and its file work. A save is one small file write; the
/// old window waited at most 1.6 s for two of them inside a 3 s round trip.
const ANSWER_WITHIN: Duration = Duration::from_secs(3);

/// One action, with the one sentence a reader who cannot see the screen needs. The guard stops
/// the app (and the tests, which build this same table) on a placeholder.
fn act(name: &'static str, sentence: &'static str) -> Action {
    assert!(
        sentence.len() >= 20 && !sentence.starts_with("Notes:"),
        "notes action `{name}` was given a placeholder description: {sentence:?}"
    );
    Action::new(name, sentence)
}

/// One argument, with the format it takes and what leaving it out means. Required unless
/// `.optional()` is added.
fn arg(name: &'static str, sentence: &'static str) -> Param {
    assert!(
        sentence.len() >= 15,
        "notes argument `{name}` was given no usable description: {sentence:?}"
    );
    Param::text(name).describe(sentence)
}

/// A required text that has to say something. The dispatch refuses a missing one before this
/// runs; this is the other half, a present and blank one, refused by name with the window's
/// notice saying it too. The text itself is kept as given, not trimmed.
fn needed(a: &Args<'_>, action: &str, name: &str, hint: &str) -> Result<String, Command> {
    match a.raw().get(name).and_then(Value::as_str) {
        Some(v) if !v.trim().is_empty() => Ok(v.to_string()),
        Some(_) => Err(refuse(format!("`{action}` was given an empty `{name}`. {hint}"))),
        None => Err(refuse(format!("`{action}` needs `{name}`. {hint}"))),
    }
}

fn refuse(sentence: String) -> Command {
    Command::Surface(Act::Refuse(sentence))
}

/// An optional text: absent and empty mean the same thing to every caller here.
fn given(a: &Args<'_>, name: &str) -> String {
    a.raw().get(name).and_then(Value::as_str).unwrap_or_default().to_string()
}

/// A call, parsed: its command, or the refusal of a blank argument.
fn call(a: &Args<'_>, parse: impl FnOnce(&Args<'_>) -> Result<Act, Command>) -> Result<Command, Refused> {
    Ok(parse(a).map(Command::Surface).unwrap_or_else(|refusal| refusal))
}

fn add(
    table: &mut Intents<Notes>,
    action: Action,
    parse: impl Fn(&Args<'_>) -> Result<Act, Command> + 'static,
) {
    table.add(
        Intent::new(action, move |a| call(a, &parse))
            .settles(Settle::Explicit { within: ANSWER_WITHIN })
            .reply(|m: &Model| m.answer.clone()),
    );
}

pub fn table(table: &mut Intents<Notes>) {
    // A call while a file operation runs is refused, as it always was: the answer it would give
    // is about a note that is about to change.
    table.state_rule(|m: &Model| match m.busy {
        Some(_) => Err(Refused::new(
            "busy",
            "Notes is in the middle of a file operation; read `describe` again in a moment.",
        )),
        None => Ok(()),
    });

    add(
        table,
        act(
            "new_note",
            "Create a note with this title, put `text` in it if given, save it, and open it. The \
             title is written as the note's first line, `# <title>`, which is what the library \
             lists it by; whatever note was open is saved first.",
        )
        .arg(arg(
            "title",
            "The new note's title. It is written as the first line, `# <title>`, and is what \
             `open_note` and `search` find it by.",
        ))
        .arg(
            arg(
                "text",
                "The body, written under the title on its own lines. Leave it out for a note \
                 that is only a heading; `append` can add to it later.",
            )
            .optional(),
        ),
        |a| {
            let title = needed(
                a,
                "new_note",
                "title",
                "It becomes the note's `# heading` line — for example title=\"Groceries\".",
            )?;
            Ok(Act::NewNote { title, body: given(a, "text") })
        },
    );

    add(
        table,
        act(
            "open_note",
            "Open an existing note by its exact title or its filename and show it in the editor. \
             The note that was open is saved first; a note in the Trash opens read-only.",
        )
        .arg(arg(
            "title",
            "The note's exact title, or the filename `describe` lists it under (`note-….md`). \
             Trashed notes are matched too.",
        )),
        |a| {
            let wanted = needed(a, "open_note", "title", "Use a title or filename from `describe.notes`.")?;
            Ok(Act::OpenNote { wanted })
        },
    );

    add(
        table,
        act(
            "set_title",
            "Rename the note that is open by rewriting its first `# heading` line, adding one at \
             the top if it has none, and save it. Nothing else in the note changes.",
        )
        .arg(arg("title", "The new title, on one line; any line breaks in it become spaces.")),
        |a| Ok(Act::SetTitle(needed(a, "set_title", "title", "It replaces the `# heading` line.")?)),
    );

    add(
        table,
        act(
            "append",
            "Add text to the end of the note that is open and save it. Nothing already in the \
             note is removed; use `set_content` to replace it instead.",
        )
        .arg(arg(
            "text",
            "Exactly what to add, appended to the last character already there — begin it with \
             a newline to start it on its own line.",
        )),
        |a| {
            Ok(Act::Append(needed(
                a,
                "append",
                "text",
                "There is nothing to append without it, and the note was left untouched.",
            )?))
        },
    );

    add(
        table,
        // Sensitive: it overwrites the whole note, unsaved paragraphs included. What it replaces
        // is on disk only if it had been saved, and `undo` only reaches it while this window
        // stays on this note.
        act(
            "set_content",
            "Replace everything in the note that is open with this text and save it. What was \
             there is gone from the window; `undo` takes it back while the note stays open.",
        )
        .risk("sensitive")
        .arg(arg(
            "text",
            "The note's entire new text, Markdown included — keep the `# <title>` first line if \
             the note should keep its title. An empty string empties the note.",
        )),
        |a| match a.raw().get("text").and_then(Value::as_str) {
            Some(text) => Ok(Act::SetContent(text.to_string())),
            None => Err(refuse("`set_content` needs `text`: the note's entire new text.".into())),
        },
    );

    add(
        table,
        act(
            "search",
            "Filter the library list to the notes whose text or metadata contains this, without \
             opening anything; `read_notes` returns their text. `set_folder` picks the section.",
        )
        .arg(arg(
            "query",
            "Matched case-insensitively against each note's text and its notebook and tags; an \
             empty string clears the filter.",
        )),
        |a| Ok(Act::Search(given(a, "query"))),
    );

    add(
        table,
        act(
            "read_notes",
            "Read the text of many notes in one call, newest first: every note whose text, \
             notebook or tags contain `query`, or all of them, without opening any. One page at \
             a time; the answer names the `skip` for the next page.",
        )
        .arg(
            arg(
                "query",
                "Matched case-insensitively like `search`; leave it out to read every note \
                 outside the Trash.",
            )
            .optional(),
        )
        .arg(
            arg(
                "skip",
                "How many matching notes to pass over, as the last answer's `how_to_see_the_rest` \
                 says; leave it out for the first page.",
            )
            .optional(),
        )
        // A read that opens nothing and moves no view. Left at the default grade, a mind's work
        // log cut its answer at 900 characters, so the model saw the envelope and none of the
        // notes, and asked eight more times.
        .risk("safe"),
        |a| {
            let skip = a
                .raw()
                .get("skip")
                .and_then(|v| v.as_u64().or_else(|| v.as_str()?.trim().parse().ok()))
                .unwrap_or(0) as usize;
            Ok(Act::ReadNotes { query: given(a, "query"), skip })
        },
    );

    add(
        table,
        act(
            "set_folder",
            "Switch the library list to one section of the library. It changes what `search` and \
             `describe.matches` list, and opens nothing.",
        )
        .arg(arg(
            "folder",
            "One of `all`, `recent` (touched in the last seven days), `favorites` (pinned), \
             `trash`, or `book:<notebook name>`.",
        )),
        |a| {
            Ok(Act::SetFolder(needed(
                a,
                "set_folder",
                "folder",
                "One of `all`, `recent`, `favorites`, `trash`, `book:<name>`.",
            )?))
        },
    );

    add(
        table,
        act(
            "notebook",
            "File the note that is open under a notebook, which is the section `set_folder \
             book:<name>` lists, and save it. It replaces whatever notebook it was in.",
        )
        .arg(arg(
            "text",
            "The notebook's name, up to 80 characters; an empty string files the note under none.",
        )),
        |a| Ok(Act::Notebook(given(a, "text"))),
    );

    add(
        table,
        act(
            "tags",
            "Replace the tags on the note that is open and save it. Tags are searched by \
             `search` along with the note's text.",
        )
        .arg(arg(
            "text",
            "Comma-separated tags, e.g. `work, ideas`, up to 512 characters; an empty string \
             clears them.",
        )),
        |a| Ok(Act::Tags(given(a, "text"))),
    );

    add(
        table,
        act(
            "save",
            "Write the note that is open to its file now instead of waiting for the 750 ms \
             autosave, and check that the bytes on disk are the text in the window.",
        ),
        |_| Ok(Act::Save),
    );

    add(
        table,
        // Sensitive: the note leaves the library and its file is removed from the notes
        // directory. It is not `dangerous` because `restore` puts back exactly this note from
        // the Trash record written before the file is unlinked.
        act(
            "trash",
            "Move the note that is open to the Trash: it leaves the library, its file is removed \
             from the notes directory, and `restore` puts it back.",
        )
        .risk("sensitive"),
        |_| Ok(Act::Trash),
    );

    add(
        table,
        act(
            "restore",
            "Put the trashed note that is open back into the library, writing its file into the \
             notes directory again. It refuses rather than overwrite a note of the same filename.",
        ),
        |_| Ok(Act::Restore),
    );

    add(
        table,
        act(
            "copy",
            "Duplicate the note that is open as a new note with its own filename, save that, and \
             open it. Unsaved changes go with the copy — it is the way out of \"this note changed \
             on disk\" — and the original file is left as it is.",
        ),
        |_| Ok(Act::Copy),
    );

    add(
        table,
        act(
            "reload",
            "Re-read every note from the notes directory, picking up files changed outside this \
             window. The open note is saved first and stays open if its file is still there.",
        ),
        |_| Ok(Act::Reload),
    );

    add(
        table,
        act(
            "preview",
            "Turn the rendered Markdown preview of the open note on or off — it is a toggle, and \
             the answer says which it now is. The note itself is not changed.",
        ),
        |_| Ok(Act::Preview),
    );

    add(
        table,
        act(
            "focus",
            "Turn focus mode, which hides the library list and the side panels, on or off — it \
             is a toggle, and the answer says which it now is.",
        ),
        |_| Ok(Act::Focus),
    );

    add(
        table,
        act(
            "undo",
            "Take back the last change made to the note that is open in this window, and save \
             the result. The history is cleared whenever another note is opened.",
        ),
        |_| Ok(Act::Undo),
    );

    add(
        table,
        act(
            "redo",
            "Put back the change that `undo` took off the note that is open, and save the \
             result. Editing the note discards what redo was holding.",
        ),
        |_| Ok(Act::Redo),
    );

    add(
        table,
        // Sensitive: it reads a file from anywhere this user can read, outside the notes
        // directory, and copies its contents into the library.
        act(
            "import",
            "Read a UTF-8 text or Markdown file from anywhere on disk and add its contents to \
             the library as a new note, then open it. The file itself is left where it is.",
        )
        .risk("sensitive")
        .arg(arg(
            "path",
            "Absolute path to the file to read, or one starting `~/`. Up to 256 KiB of text with \
             no control characters.",
        )),
        |a| Ok(Act::Import { path: needed(a, "import", "path", "An absolute path to a text file.")? }),
    );

    add(
        table,
        // Sensitive: it writes a file outside the notes directory, at a path the caller chooses.
        // It cannot overwrite — the store publishes the new file with a link, which fails if
        // something is already there — so it is not `dangerous`.
        act(
            "export",
            "Write the open note's text to a file outside the library, at this path. It refuses \
             rather than overwrite anything already there, and the note stays open.",
        )
        .risk("sensitive")
        .arg(arg(
            "path",
            "Absolute path of the file to create, or one starting `~/`. Its folder must exist \
             and nothing may already be at that path.",
        )),
        |a| Ok(Act::Export { path: needed(a, "export", "path", "An absolute path to write to.")? }),
    );

    // The outside bar's choices, for a mind as for the person: the same commands its buttons run.
    // *Not now* is the bar's alone: it only puts the bar away, and a mind's saves are refused
    // until a choice is made either way.
    add(
        table,
        // Sensitive: it writes a new note into the library and moves the window to it. Nothing is
        // lost or overwritten: the file the other program wrote is left as it is.
        act(
            "keep_mine_as_copy",
            "When the open note's file was changed outside Notes while it had unsaved edits, or was \
             moved or deleted (`describe.outside` is `conflict` or `deleted`), save the text in the \
             window as a new note and open it. The file on disk is left exactly as it is.",
        )
        .risk("sensitive"),
        |_| Ok(Act::KeepMineAsCopy),
    );

    add(
        table,
        // Sensitive: the text in the window is replaced. It is not `dangerous` because that text is
        // saved as a note of its own first, which `open_note` reaches.
        act(
            "take_theirs",
            "When the open note's file was changed outside Notes while it had unsaved edits \
             (`describe.outside` is `conflict`), replace the text in the window with the file's. The \
             window's text is first saved as a separate note, and the undo history is cleared.",
        )
        .risk("sensitive"),
        |_| Ok(Act::TakeTheirs),
    );

    add(
        table,
        // Safe: it reads the file and shows the two versions side by side, read-only. Nothing is
        // written, saved or replaced.
        act(
            "compare",
            "When the open note's file was changed outside Notes while it had unsaved edits, show the \
             window's text beside the file's, read-only, and answer with both and the first line \
             where they differ. Nothing is changed or saved.",
        )
        .risk("safe"),
        |_| Ok(Act::Compare),
    );
}
