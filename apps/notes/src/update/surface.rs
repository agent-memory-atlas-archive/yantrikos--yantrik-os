//! The surface's calls (`app.act`), each doing what the same action in the window does, and
//! answered once its work has landed: with what the note now is on disk, or with a refusal that
//! is said in the window's notice too (contract point 4: failure is said twice).
//!
//! A call that only starts work records what it waits for in `Model::asked`; [`finish`] runs
//! after every command and answers it as soon as nothing of it is still running or waiting.

use serde_json::{json, Value};
use yantrik_app::effects::Effects;
use yantrik_app::Refused;

use super::{editing, relist, run, saving, window};
use crate::command::{Act, Step, WindowAction};
use crate::describe::{listed, read_notes, written};
use crate::model::{Asked, Expect, Model, Pending};
use crate::store::{expand_home, read, validate, LIMIT};
use crate::view::render::{CHROME, CONTENT, FIELDS, STATUS};
use crate::Notes;

const NOTHING_OPEN: &str = "No note is open, so there is nothing to report.";

pub fn act(m: &mut Model, fx: &mut Effects<Notes>, act: Act) {
    m.answer = Value::Null;
    match act {
        Act::Refuse(sentence) => refuse(m, fx, sentence),
        Act::NewNote { title, body } => {
            if library_open(m, fx) {
                ask(m, fx, Expect::Written);
                save_then(m, fx, Step::NewNote { title, body });
            }
        }
        Act::OpenNote { wanted } => open_note(m, fx, &wanted),
        Act::SetTitle(title) => {
            let Some(text) = open_text(m, fx) else { return };
            commit(m, fx, retitled(&text, &title.replace(['\r', '\n'], " ")));
        }
        Act::Append(addition) => {
            let Some(text) = open_text(m, fx) else { return };
            commit(m, fx, text + &addition);
        }
        Act::SetContent(text) => {
            if open_text(m, fx).is_some() {
                commit(m, fx, text);
            }
        }
        Act::Search(query) => {
            m.pane.query = query.clone();
            fx.changed(CHROME);
            relist(m, fx);
            let (matches, titles) = listed(m);
            m.answer = json!({ "query": query, "folder": m.pane.folder, "matches": matches, "titles": titles });
        }
        Act::ReadNotes { query, skip } => m.answer = read_notes(&m.library, &query, skip),
        Act::SetFolder(folder) => {
            m.pane.folder = folder.clone();
            fx.changed(CHROME);
            relist(m, fx);
            let (listed, titles) = listed(m);
            m.answer = json!({ "folder": folder, "listed": listed, "titles": titles });
        }
        Act::Notebook(notebook) => {
            if open_text(m, fx).is_some() {
                let tags = m.pane.tags.clone();
                fields(m, fx, notebook, tags);
            }
        }
        Act::Tags(tags) => {
            if open_text(m, fx).is_some() {
                let notebook = m.pane.notebook.clone();
                fields(m, fx, notebook, tags);
            }
        }
        Act::Save => {
            if open_text(m, fx).is_some() {
                ask(m, fx, Expect::Saved);
                saving::save(m, fx, saving::How::Asked);
            }
        }
        Act::Trash => trash(m, fx),
        Act::Restore => restore(m, fx),
        Act::Copy => {
            let Some(from) = m.current().map(|c| c.id().to_string()) else {
                return refuse(m, fx, "No note is open, so there is nothing to copy.".into());
            };
            ask(m, fx, Expect::Copied { from });
            window::act(m, fx, WindowAction::Copy);
        }
        Act::Reload => {
            ask(m, fx, Expect::Reloaded);
            window::act(m, fx, WindowAction::Reload);
        }
        Act::Preview => {
            window::act(m, fx, WindowAction::Preview);
            m.answer = json!({
                "preview": m.pane.preview,
                "blocks": m.pane.blocks.len(),
                "title": m.current().map(|c| c.title()),
            });
        }
        Act::Focus => {
            window::act(m, fx, WindowAction::Focus);
            m.answer = json!({ "focus_mode": m.pane.focus_mode, "open_note": m.current().map(|c| c.title()) });
        }
        Act::Undo => step_history(m, fx, false),
        Act::Redo => step_history(m, fx, true),
        Act::Import { path } => {
            let from = expand_home(path.trim());
            if !from.is_absolute() {
                return refuse(m, fx, format!("`import` needs an absolute path; `{path}` is not one."));
            }
            if library_open(m, fx) {
                ask(m, fx, Expect::Imported { from });
                save_then(m, fx, Step::Import { path });
            }
        }
        Act::Export { path } => {
            let to = expand_home(path.trim());
            if !to.is_absolute() {
                return refuse(m, fx, format!("`export` needs an absolute path; `{path}` is not one."));
            }
            let Some(text) = open_text(m, fx) else { return };
            if library_open(m, fx) {
                ask(m, fx, Expect::Exported { to, text });
                save_then(m, fx, Step::Export { path });
            }
        }
    }
}

/// Answers the call waiting in `Model::asked`, once nothing of it runs or waits any more. Only
/// under that call's own invocation: it is the one the answer is owed to.
pub fn finish(m: &mut Model, fx: &mut Effects<Notes>) {
    let idle = m.busy.is_none() && m.pending.is_none();
    if !idle || m.asked.as_ref().is_none_or(|a| a.invocation != fx.invocation()) {
        return;
    }
    let Some(asked) = m.asked.take() else { return };
    let outcome = match asked.refused {
        Some(sentence) => Err(sentence),
        None => answer(m, asked.expect, asked.landed),
    };
    match outcome {
        Ok(answer) => {
            m.answer = answer;
            fx.settle(Ok(()));
        }
        Err(sentence) => refuse(m, fx, sentence),
    }
}

/// The answer for what the call waited on, read from the model and the disk now.
fn answer(m: &Model, expect: Expect, landed: bool) -> Result<Value, String> {
    // What went wrong, as the notice says it, or else what is known not to have happened.
    let why = |otherwise: String| if m.notice.is_empty() { otherwise } else { m.notice.clone() };
    let note = || written(m).ok_or_else(|| NOTHING_OPEN.to_string());
    match expect {
        Expect::Written => note(),
        Expect::Saved => {
            let answer = note()?;
            if answer["matches_disk"] != true {
                let path = answer["path"].as_str().unwrap_or_default();
                return Err(why(format!("The note was not written to {path}; the draft is still open.")));
            }
            Ok(answer)
        }
        Expect::Trashed { id, title, path } => {
            if !m.library.iter().any(|n| n.id == id && n.trash) {
                return Err(why(format!("\"{title}\" is still in the library; nothing was trashed.")));
            }
            Ok(json!({
                "trashed": title,
                "filename": id,
                "file_removed": !path.exists(),
                "in_trash": true,
                "trash_count": m.library.iter().filter(|n| n.trash).count(),
                "notes": m.library.iter().filter(|n| !n.trash).count(),
                "open_note": Value::Null,
            }))
        }
        Expect::Restored { id } => {
            if m.library.iter().any(|n| n.id == id && n.trash) {
                return Err(why("The note is still in the Trash; nothing was restored.".into()));
            }
            note()
        }
        Expect::Copied { from } => {
            let mut answer = note()?;
            if answer["filename"] == json!(from) {
                return Err(why("Nothing was copied; the same note is still open.".into()));
            }
            answer["copied_from"] = json!(from);
            Ok(answer)
        }
        Expect::Reloaded => Ok(json!({
            "notes": m.library.iter().filter(|n| !n.trash).count(),
            "in_trash": m.library.iter().filter(|n| n.trash).count(),
            "open_note": m.current().map(|c| c.id().to_string()),
            "library_ready": m.ready,
            "notice": m.notice,
        })),
        Expect::Imported { from } => {
            if !landed {
                return Err(why(format!("Nothing was imported from {}.", from.display())));
            }
            let mut answer = note()?;
            answer["imported_from"] = json!(from.display().to_string());
            Ok(answer)
        }
        Expect::Exported { to, text } => {
            let bytes = read(&to, LIMIT).ok().flatten();
            if !landed || bytes.is_none() {
                return Err(why(format!("Nothing was written to {}.", to.display())));
            }
            Ok(json!({
                "path": to.display().to_string(),
                "bytes": bytes.as_deref().map(str::len),
                "matches_note": bytes.as_deref() == Some(text.as_str()),
                "title": m.current().map(|c| c.title()),
            }))
        }
    }
}

/// A refusal the person at the window sees too: the notice, and the call's error.
fn refuse(m: &mut Model, fx: &mut Effects<Notes>, sentence: String) {
    m.notice = sentence.clone();
    fx.changed(STATUS);
    fx.settle(Err(Refused::new("refused", sentence)));
}

fn ask(m: &mut Model, fx: &Effects<Notes>, expect: Expect) {
    m.asked = Some(Asked { invocation: fx.invocation(), expect, refused: None, landed: false });
}

/// The calls that leave the open note save it first, and need the library open to do so.
fn library_open(m: &mut Model, fx: &mut Effects<Notes>) -> bool {
    if !m.ready {
        refuse(m, fx, "The library is unavailable; resolve the storage error and run `reload`.".into());
    }
    m.ready
}

/// `step` once the open note is saved; if that save fails, the call is refused and nothing else
/// is done. An open note an outside edit blocks is refused at once, as a failed save is.
fn save_then(m: &mut Model, fx: &mut Effects<Notes>, step: Step) {
    if m.unsaved() {
        m.pending = Some(Pending { step, refuse_if_unsaved: true });
        saving::save(m, fx, saving::How::Asked);
    } else {
        run(m, fx, step);
    }
}

/// The open note's text, or a refusal that says what to do about it: one sentence each for
/// "no note is open" and "that note is in the Trash", for every editing call.
fn open_text(m: &mut Model, fx: &mut Effects<Notes>) -> Option<String> {
    let refusal = match m.current() {
        None => "No note is open. Open one with `open_note`, or make one with `new_note`.",
        Some(c) if c.trash() => "The open note is in the Trash and cannot be edited; `restore` puts it back first.",
        Some(c) => return Some(c.text().to_string()),
    };
    refuse(m, fx, refusal.into());
    None
}

/// New text in the open note, saved, and answered with what the note then holds.
fn commit(m: &mut Model, fx: &mut Effects<Notes>, text: String) {
    if let Err(e) = validate(&text) {
        return refuse(m, fx, e);
    }
    editing::edit(m, fx, text);
    fx.changed(CONTENT);
    ask(m, fx, Expect::Written);
    saving::save(m, fx, saving::How::Asked);
}

/// The notebook or tags fields set as the window's own would be, then saved.
fn fields(m: &mut Model, fx: &mut Effects<Notes>, notebook: String, tags: String) {
    editing::metadata(m, fx, notebook, tags);
    fx.changed(FIELDS);
    ask(m, fx, Expect::Written);
    saving::save(m, fx, saving::How::Asked);
}

/// The note's first `# heading` line rewritten, or one added at the top. Line breaks are joined
/// by `\n` again, as they always were.
fn retitled(text: &str, title: &str) -> String {
    let mut replaced = false;
    let lines: Vec<String> = text
        .lines()
        .map(|l| {
            if !replaced && l.starts_with("# ") {
                replaced = true;
                format!("# {title}")
            } else {
                l.into()
            }
        })
        .collect();
    let joined = lines.join("\n");
    if replaced {
        joined
    } else {
        format!("# {title}\n\n{joined}")
    }
}

fn open_note(m: &mut Model, fx: &mut Effects<Notes>, wanted: &str) {
    let Some((id, trash)) = m
        .library
        .iter()
        .find(|n| n.id == wanted || n.title() == wanted)
        .map(|n| (n.id.clone(), n.trash))
    else {
        let titles: Vec<String> =
            m.library.iter().filter(|n| !n.trash).take(8).map(|n| format!("\"{}\"", n.title())).collect();
        let held = if titles.is_empty() { "nothing yet".to_string() } else { titles.join(", ") };
        return refuse(m, fx, format!("No note is called \"{wanted}\". The library holds: {held}"));
    };
    if library_open(m, fx) {
        ask(m, fx, Expect::Written);
        save_then(m, fx, Step::Window(WindowAction::Open { id, trash }));
    }
}

fn trash(m: &mut Model, fx: &mut Effects<Notes>) {
    let refusal = match m.current() {
        None => Some("No note is open, so there is nothing to trash."),
        Some(c) if c.trash() => Some("That note is already in the Trash."),
        Some(_) => None,
    };
    if let Some(sentence) = refusal {
        return refuse(m, fx, sentence.into());
    }
    let Some(expect) = m.current().map(|c| Expect::Trashed {
        id: c.id().to_string(),
        title: c.title(),
        path: m.vault.dir().join(c.id()),
    }) else {
        return;
    };
    ask(m, fx, expect);
    window::act(m, fx, WindowAction::Trash);
}

fn restore(m: &mut Model, fx: &mut Effects<Notes>) {
    let id = match m.current() {
        None => return refuse(m, fx, "No note is open, so there is nothing to restore.".into()),
        Some(c) if !c.trash() => {
            return refuse(
                m,
                fx,
                "That note is in the library already; only a trashed note can be restored. Open it \
                 from `set_folder trash` first."
                    .into(),
            )
        }
        Some(c) => c.id().to_string(),
    };
    ask(m, fx, Expect::Restored { id });
    window::act(m, fx, WindowAction::Restore);
}

fn step_history(m: &mut Model, fx: &mut Effects<Notes>, redo: bool) {
    let Some(before) = open_text(m, fx) else { return };
    editing::undo(m, fx, redo);
    if m.current().is_some_and(|c| c.text() == before) {
        let sentence = if redo {
            "There is nothing to redo on this note."
        } else {
            "There is nothing left to undo on this note in this window."
        };
        return refuse(m, fx, sentence.into());
    }
    ask(m, fx, Expect::Written);
    saving::save(m, fx, saving::How::Asked);
}
