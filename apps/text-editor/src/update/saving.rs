//! Saving a tab through its document: one file operation at a time, in a job, with how much of a
//! failure reached the disk said honestly, another program's edit never written over, and what a
//! replace took the place of kept (files/save.rs). A save that lands moves the tab to the file it
//! wrote, and goes on with what waited for it: the close that asked, or the editor's end.

use std::path::PathBuf;

use yantrik_app::documents::{Durability, Fingerprint, OutsideState, SaveError, SaveOutcome, SaveState, Source};
use yantrik_app::effects::Effects;

use super::{closing, say, surface};
use crate::command::{AfterSave, Command};
use crate::files::kept;
use crate::files::save::{save, SaveFailed, SaveJob, Wrote};
use crate::model::{Dialog, Model, Work};
use crate::tab::TabId;
use crate::TextEditor;

/// Said when a save ended without an answer: it may have written anything, so nothing is called
/// saved.
const JOB_LOST: &str = "Save failed: the save did not finish. Your draft is still open.";

/// Writes tab `id` to `target` (its own file when `None`) in a job.
pub fn start(
    m: &mut Model,
    fx: &mut Effects<TextEditor>,
    id: TabId,
    target: Option<PathBuf>,
    overwrite: bool,
    then: AfterSave,
    agent: bool,
) {
    let Some(tab) = m.tab(id).cloned() else { return };
    let own = m.path(&tab).map(PathBuf::from);
    let Some(target) = target.or_else(|| own.clone()) else { return };
    let Some(doc) = m.doc_mut(tab.doc) else { return };
    let seq = doc.seq;
    let job = SaveJob {
        text: doc.content.clone(),
        target,
        own,
        on_disk: tab.on_disk.clone(),
        unsure: tab.unsure.clone(),
        inode: tab.inode,
        overwrite,
        agent,
    };
    // One save in flight, judged by the document when it lands, whatever the save was for.
    doc.save = SaveState::Saving { seq };
    m.busy = Some(Work::Save(id));
    m.dialog_error.clear();
    say(m, "Saving…");
    let written = Fingerprint::of_bytes(job.text.as_bytes());
    let lost = Command::SaveLanded {
        tab: id,
        seq,
        then: then.clone(),
        result: Err(Box::new(SaveFailed {
            error: SaveError::after_writing(Durability::Unknown, written, JOB_LOST),
            kept: None,
        })),
    };
    let disk = m.disk.clone();
    fx.spawn_or(lost, move |_| Command::SaveLanded { tab: id, seq, then, result: save(&job, disk.as_ref()) });
}

/// A save of tab `id` at `seq` landed.
pub fn landed(
    m: &mut Model,
    fx: &mut Effects<TextEditor>,
    id: TabId,
    seq: u64,
    then: AfterSave,
    result: Result<Wrote, Box<SaveFailed>>,
) {
    if m.busy == Some(Work::Save(id)) {
        m.busy = None;
    }
    let Some(tab) = m.tab(id).cloned() else {
        return surface::refuse_if_owed(m, fx, &then, "The tab was closed before its save landed.");
    };
    let kept = match &result {
        Ok(w) => w.kept.as_ref().map(|k| kept::said(k, &w.file.path)),
        Err(f) => f.kept.as_ref().zip(m.path(&tab)).map(|(k, p)| kept::said(k, p)),
    };
    match result {
        Ok(wrote) => {
            let Some(doc) = m.doc_mut(tab.doc) else { return };
            let outcome = doc.save_landed_witnessed(seq, Ok(wrote.written.clone()), wrote.witnessed);
            if !matches!(outcome, SaveOutcome::Saved { .. }) {
                return;
            }
            moved_to_file(m, fx, id, &wrote);
            say(m, kept.unwrap_or_else(|| "Saved".into()));
            m.dialog = Dialog::None;
            closing::saved(m, fx, id);
            surface::answers::saved(m, fx, &then, id);
        }
        Err(failed) => {
            let Some(doc) = m.doc_mut(tab.doc) else { return };
            if matches!(doc.save_landed(seq, Err(failed.error.clone())), SaveOutcome::Stale) {
                return;
            }
            if let (Some(t), Some(landed)) = (m.tab_mut(id), failed.error.landed.clone()) {
                t.unsure = Some(landed);
            }
            let sentence = match kept {
                Some(kept) => format!("{kept} {}", failed.error.sentence),
                None => failed.error.sentence.clone(),
            };
            m.dialog_error = sentence.clone();
            say(m, sentence.clone());
            closing::not_saved(m, fx, id, &sentence);
            surface::refuse_if_owed(m, fx, &then, &sentence);
        }
    }
}

/// The tab now is the file the save wrote: its source, its baseline, its watch, what an agent is
/// shown of it. The save checked the file held what the tab expected before it replaced it, so an
/// outside change seen before is answered.
fn moved_to_file(m: &mut Model, fx: &mut Effects<TextEditor>, id: TabId, wrote: &Wrote) {
    let Some(tab) = m.tab(id).cloned() else { return };
    let Some(doc) = m.doc_mut(tab.doc) else { return };
    let source = Source::File(wrote.file.path.clone());
    if doc.source != source {
        fx.unwatch(&doc.source.key());
        doc.source = source;
    }
    doc.outside = OutsideState::Clean;
    if let Some((key, path, seen)) = doc.watch_entry() {
        fx.watch(&key, path, seen);
    }
    if let Some(t) = m.tab_mut(id) {
        t.on_disk = Some(wrote.file.text.clone());
        t.recovered = false;
        t.hidden = wrote.file.hidden;
        t.inode = wrote.file.inode;
        t.unsure = None;
    }
}
