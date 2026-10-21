//! The surface's calls (`app.act`), each doing what the same action in the window does, and
//! answered once its work has landed: with what the note now is on disk, or with a refusal that
//! is said in the window's notice too (contract point 4: failure is said twice).
//!
//! A call that only starts work records what it waits for in `Model::asked`; [`finish`] runs
//! after every command and answers it as soon as nothing of it is still running or waiting.

use serde_json::{json, Value};
use yantrik_app::effects::Effects;
use yantrik_app::Refused;

use super::{editing, outside, recovery, relist, run, said, saving, window};
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
            // Saved first as the window's Refresh is, except that a save that fails refuses the
            // call: nothing was reloaded. A read-only library reloads with no save first.
            let reload = Step::Window(WindowAction::Reload);
            if window::saves_first(m, &WindowAction::Reload) {
                save_then(m, fx, reload);
            } else {
                run(m, fx, reload);
            }
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
        Act::RestoreDraft => recovered(m, fx, WindowAction::RestoreDraft),
        Act::DiscardDraft => recovered(m, fx, WindowAction::DiscardDraft),
        Act::KeepMineAsCopy => resolve(m, fx, WindowAction::KeepMine, false),
        Act::TakeTheirs => resolve(m, fx, WindowAction::TakeTheirs, true),
        Act::Compare => resolve(m, fx, WindowAction::Compare, true),
        Act::Export { path } => {
            let to = expand_home(path.trim());
            if !to.is_absolute() {
                return refuse(m, fx, format!("`export` needs an absolute path; `{path}` is not one."));
            }
            if open_text(m, fx).is_none() {
                return;
            }
            let text = m.shown().map(|c| c.text.clone()).unwrap_or_default();
            if library_open(m, fx) {
                ask(m, fx, Expect::Exported { to, text });
                // A read-only library would refuse the save first; the export writes the text as
                // it is in the window, outside the vault, which is how it gets out.
                if m.vault.read_only().is_some() {
                    run(m, fx, Step::Export { path });
                } else {
                    save_then(m, fx, Step::Export { path });
                }
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
        None => answer(m, asked.expect, asked.landed, asked.kept),
    };
    match outcome {
        Ok(answer) => {
            m.answer = answer;
            fx.settle(Ok(()));
        }
        Err(sentence) => refuse(m, fx, sentence),
    }
}

/// The answer for what the call waited on, read from the model and the disk now. `kept` is the
/// note an outside change's resolution kept the window's text as.
fn answer(m: &Model, expect: Expect, landed: bool, kept: Option<String>) -> Result<Value, String> {
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
                // Nothing, unless text typed while the job ran became a note of its own.
                "open_note": m.current().map(|c| c.id().to_string()),
            }))
        }
        Expect::Restored { id } => {
            if m.library.iter().any(|n| n.id == id && n.trash) {
                return Err(why("The note is still in the Trash; nothing was restored.".into()));
            }
            note()
        }
        Expect::Recovered { id } => {
            if m.recovery.offer_for(&id).is_some() || m.current().is_none_or(|c| c.id() != id) {
                return Err(why("The unsaved text is still waiting; nothing was restored.".into()));
            }
            note()
        }
        Expect::Discarded { id } => {
            if m.recovery.offer_for(&id).is_some() {
                return Err(why("The unsaved text is still waiting; nothing was discarded.".into()));
            }
            Ok(json!({ "discarded": id, "still_offered": m.recovery.offers.len(), "notice": m.notice }))
        }
        Expect::Copied { from } => {
            let mut answer = note()?;
            if answer["filename"] == json!(from) {
                return Err(why("Nothing was copied; the same note is still open.".into()));
            }
            answer["copied_from"] = json!(from);
            Ok(answer)
        }
        Expect::TookTheirs { id } => {
            let took = m.current().is_some_and(|c| c.id() == id && !c.doc.blocked());
            if !took {
                return Err(why("Nothing was replaced; the window still holds its own text.".into()));
            }
            let mut answer = note()?;
            answer["kept_as"] = json!(kept);
            Ok(answer)
        }
        Expect::Compared => {
            if !landed {
                return Err(why("The version on disk could not be read, so nothing was compared.".into()));
            }
            compared(m).ok_or_else(|| NOTHING_OPEN.to_string())
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
pub(super) fn refuse(m: &mut Model, fx: &mut Effects<Notes>, sentence: String) {
    m.notice = sentence.clone();
    fx.changed(STATUS);
    fx.settle(Err(Refused::new("refused", sentence)));
}

fn ask(m: &mut Model, fx: &Effects<Notes>, expect: Expect) {
    m.asked = Some(Asked { invocation: fx.invocation(), expect, refused: None, landed: false, kept: None });
}

/// A choice about an outside change, through the same command as the bar's button. Refused, in
/// the same words the window would use, when the open note has nothing to resolve.
fn resolve(m: &mut Model, fx: &mut Effects<Notes>, action: WindowAction, theirs: bool) {
    if let Some(sentence) = outside::unresolvable(m.current(), theirs) {
        return refuse(m, fx, sentence.into());
    }
    let Some(id) = m.current().map(|c| c.id().to_string()) else { return };
    let expect = match action {
        WindowAction::TakeTheirs => Expect::TookTheirs { id },
        WindowAction::Compare => Expect::Compared,
        _ => Expect::Copied { from: id },
    };
    ask(m, fx, expect);
    window::act(m, fx, action);
}

/// The recovery bar's choices, through the same commands as its buttons: refused, in the window's
/// words, when nothing is offered. A restore is answered once the text is back and saved (or its
/// conflict is offered), a discard with what is left.
fn recovered(m: &mut Model, fx: &mut Effects<Notes>, action: WindowAction) {
    let Some(offer) = recovery::shown(m).cloned() else {
        return refuse(m, fx, said::NOTHING_OFFERED.into());
    };
    let expect = match action {
        WindowAction::RestoreDraft => Expect::Recovered { id: offer.id },
        _ => Expect::Discarded { id: offer.id },
    };
    ask(m, fx, expect);
    window::act(m, fx, action);
}

/// How much of each side a comparison answers with: two of them fit a safe read's answer.
const COMPARED_CHARS: usize = 1500;

/// The comparison as a mind reads it: both texts (each cut at [`COMPARED_CHARS`], and said when it
/// is) with their sidecars, whether they are the same, and the first line where the texts part.
fn compared(m: &Model) -> Option<Value> {
    let c = m.current()?;
    let mine = c.content();
    let theirs = m.pane.theirs.as_ref();
    let cut = |text: &str| text.chars().take(COMPARED_CHARS).collect::<String>();
    let first_difference = theirs.and_then(|theirs| {
        let (mut a, mut b) = (mine.text.lines(), theirs.text.lines());
        // The first line number where one side has a line the other does not; none when equal.
        (1..)
            .find_map(|line| match (a.next(), b.next()) {
                (None, None) => Some(None),
                (x, y) if x != y => Some(Some(line)),
                _ => None,
            })
            .flatten()
    });
    Some(json!({
        "title": c.title(),
        "mine": cut(&mine.text),
        "mine_cut_short": mine.text.chars().count() > COMPARED_CHARS,
        "mine_metadata": mine.meta,
        "theirs": theirs.map(|t| cut(&t.text)),
        "theirs_cut_short": theirs.is_some_and(|t| t.text.chars().count() > COMPARED_CHARS),
        "theirs_metadata": theirs.map(|t| t.meta.as_str()),
        "file_exists": theirs.is_some(),
        "identical": theirs == Some(mine),
        "first_different_line": first_difference,
    }))
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
