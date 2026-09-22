//! The folder the open document lives in, watched, so that moving the document does not strand it.
//!
//! Seen on 22 Sep 2026: with a document open in yDoc, Files moved it into a new folder. The disk
//! agreed and the window did not — it went on saying `saved · <the old path>`, and everything
//! downstream of that line was then wrong. Save had no file to write into, Save As refused the
//! new path because something was already there, and Open silently dropped the unsaved edit. The
//! path in the window has to be able to change without a person retyping it, and inotify on the
//! folder is how the window finds out that it has.
//!
//! Nothing here decides anything. The decisions are `document::follow_rename` and
//! `document::moved_to`, which are pure enough to test with a temporary directory and no
//! filesystem event in sight; this module is the wire between them and the kernel.

use notify::{
    event::{EventKind, ModifyKind, RenameMode},
    Event, RecursiveMode, Watcher,
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{mpsc::Sender, Arc, Mutex},
};

/// What the folder said about the open document.
#[derive(Debug)]
pub enum Move {
    /// The rename named both of its ends, so this is where the document is now. An in-place
    /// rename, or a move into a folder that happens to be watched too.
    To(PathBuf),
    /// The document left the folder and the event did not say where to — which is the ordinary
    /// case, because the folder it went into is not the folder being watched. Somebody has to go
    /// and look: `document::moved_to`.
    Away,
}

/// The watch on one folder, re-pointed as the document moves between them.
pub struct Watch {
    /// `None` when the kernel would not give us a watcher. Not fatal: without it the document
    /// simply does not follow a move, which is where this app was before.
    watcher: Option<notify::RecommendedWatcher>,
    /// The folder being watched, so re-pointing at the same one is a no-op.
    folder: Option<PathBuf>,
    /// Where the document is, as the watcher's own thread sees it. The UI thread owns the
    /// document; this is the single field the watcher needs and the only one it is given.
    file: Arc<Mutex<Option<PathBuf>>>,
}

impl Watch {
    /// Start watching nothing.
    ///
    /// `wake` runs on the watcher's thread every time something is put on `events`, and its whole
    /// job is to get the UI thread to come and read them — the document cannot be touched from
    /// here. Same shape as the Files workbench's worker events in `yantrik-ui`.
    pub fn new(events: Sender<Move>, wake: impl Fn() + Send + 'static) -> Self {
        let file: Arc<Mutex<Option<PathBuf>>> = Arc::default();
        let mirror = file.clone();
        let handler = move |result: Result<Event, notify::Error>| {
            let Ok(event) = result else { return };
            let Some(current) = mirror.lock().ok().and_then(|held| (*held).clone()) else { return };
            let Some(moved) = read_event(&event, &current) else { return };
            if events.send(moved).is_ok() {
                wake();
            }
        };
        match notify::recommended_watcher(handler) {
            Ok(watcher) => Self { watcher: Some(watcher), folder: None, file },
            Err(e) => {
                tracing::warn!(error = %e, "yDoc cannot watch the folder its document is in");
                Self { watcher: None, folder: None, file }
            }
        }
    }

    /// Watch the folder `file` lives in, and stop watching whatever was being watched before.
    ///
    /// Called from `paint`, which runs after every change to the document, so the watch cannot
    /// drift away from the file it is supposed to be about.
    pub fn point_at(&mut self, file: Option<&Path>) {
        if let Ok(mut held) = self.file.lock() {
            *held = file.map(|f| f.to_path_buf());
        }
        let folder = file.and_then(|f| f.parent()).map(|f| f.to_path_buf());
        if folder == self.folder {
            return;
        }
        let Some(watcher) = self.watcher.as_mut() else { return };
        if let Some(old) = self.folder.take() {
            let _ = watcher.unwatch(&old);
        }
        let Some(new) = folder else { return };
        // Not recursive. The folder a document is in is the folder that reports it being moved
        // out, and a recursive watch rooted at somebody's Documents is a watch on everything
        // they own.
        match watcher.watch(&new, RecursiveMode::NonRecursive) {
            Ok(()) => self.folder = Some(new),
            Err(e) => tracing::warn!(folder = %new.display(), error = %e, "folder is not watchable"),
        }
    }
}

/// What one directory event means for the file at `current`.
///
/// `None` for the events that are about something else, which is nearly all of them — including
/// yDoc's own temporary file being renamed over the document on every single save.
pub fn read_event(event: &Event, current: &Path) -> Option<Move> {
    match &event.kind {
        // Both halves of the rename, paired by inotify on their cookie: the folder told us where
        // the file went.
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => {
            let from = event.paths.first()?;
            let to = event.paths.get(1)?;
            crate::document::follow_rename(current, from, to).map(Move::To)
        }
        // Only the leaving half, or a removal. The file went somewhere this watch cannot see, or
        // it is really gone; which of those it is takes a look at the disk, and that is the
        // caller's job. Checked against the disk first, because the path named may be a file
        // that merely shares a prefix with ours.
        EventKind::Modify(ModifyKind::Name(RenameMode::From | RenameMode::Any))
        | EventKind::Remove(_) => {
            let ours = event
                .paths
                .iter()
                .any(|p| p.as_path() == current || current.starts_with(p));
            (ours && fs::symlink_metadata(current).is_err()).then_some(Move::Away)
        }
        _ => None,
    }
}

/// Everything the folder has said so far, as one decision.
///
/// inotify reports the leaving half of a rename before it reports the pair, so a rename inside the
/// watched folder arrives as "it is gone" and then as "here is where it went". Both are true and
/// only the second is worth acting on. Taking the batch together rather than one event at a time
/// is what stops an ordinary rename from first spending a directory walk hunting for a file that
/// is one event away from naming itself.
pub fn latest(moves: Vec<Move>) -> Option<Move> {
    let mut away = false;
    let mut to = None;
    for moved in moves {
        match moved {
            Move::To(path) => to = Some(path),
            Move::Away => away = true,
        }
    }
    to.map(Move::To).or(away.then_some(Move::Away))
}

#[cfg(test)]
mod tests {
    //! Real inotify, real renames, a real temporary directory.
    //!
    //! What a document does about a move is decided by `document::follow_rename` and
    //! `document::moved_to`, and those are tested without a filesystem event in sight over in
    //! `tests/document-core`. What is left over is the wire, and the wire is the part that cannot
    //! be reasoned about: that the watch is pointed at the right folder, that the kernel's event
    //! reaches the channel, and that somebody else's file moving in the same folder wakes nobody.

    use super::*;
    use std::{sync::mpsc::Receiver, time::Duration};

    /// A private directory per test, removed however the test ends. On the real filesystem and
    /// not under a mount that fakes inotify, which is the whole point of these three.
    struct Dir(PathBuf);

    impl Dir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("ydoc-follow-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("a scratch directory");
            Self(fs::canonicalize(&path).expect("a real path"))
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A watch pointed at `file`, and the channel its folder reports to.
    fn watching(file: &Path) -> (Watch, Receiver<Move>) {
        let (sender, inbox) = std::sync::mpsc::channel();
        let mut watch = Watch::new(sender, || {});
        watch.point_at(Some(file));
        (watch, inbox)
    }

    /// Everything the folder has to say about the change that just happened, as one decision.
    ///
    /// The quiet period is what the app gets for free by draining the channel in one go: the
    /// events of a single rename arrive together, and the app reads them together.
    fn settled(inbox: &Receiver<Move>) -> Option<Move> {
        let mut seen = Vec::new();
        if let Ok(first) = inbox.recv_timeout(Duration::from_secs(5)) {
            seen.push(first);
            while let Ok(rest) = inbox.recv_timeout(Duration::from_millis(250)) {
                seen.push(rest);
            }
        }
        latest(seen)
    }

    #[test]
    fn a_rename_inside_the_folder_arrives_as_the_new_path() {
        let dir = Dir::new("renamed");
        let file = dir.0.join("notes.md");
        fs::write(&file, "body
").unwrap();
        let (_watch, inbox) = watching(&file);

        let to = dir.0.join("pricing.md");
        fs::rename(&file, &to).unwrap();

        // inotify says "it left" before it says "and here it is"; the second is the answer.
        match settled(&inbox) {
            Some(Move::To(now)) => assert_eq!(now, to),
            other => panic!("expected the new path; got {other:?}"),
        }
    }

    #[test]
    fn a_move_into_a_folder_this_watch_cannot_see_says_only_that_it_went() {
        let dir = Dir::new("away");
        let file = dir.0.join("notes.md");
        fs::write(&file, "body
").unwrap();
        let archive = dir.0.join("archive");
        fs::create_dir(&archive).unwrap();
        let (_watch, inbox) = watching(&file);

        // What Files does with cut and paste, and the case the issue was filed about: the folder
        // it lands in is not the folder being watched, so only the leaving half is reported and
        // nothing in the event says where it went.
        fs::rename(&file, archive.join("notes.md")).unwrap();

        match settled(&inbox) {
            Some(Move::Away) => {}
            other => panic!("expected the file to be reported as gone; got {other:?}"),
        }
    }

    #[test]
    fn somebody_elses_file_moving_in_the_same_folder_is_not_this_document() {
        let dir = Dir::new("other");
        let file = dir.0.join("notes.md");
        fs::write(&file, "body
").unwrap();
        let other = dir.0.join("other.md");
        fs::write(&other, "not ours
").unwrap();
        let (_watch, inbox) = watching(&file);

        fs::rename(&other, dir.0.join("renamed.md")).unwrap();
        fs::remove_file(dir.0.join("renamed.md")).unwrap();

        let heard = inbox.recv_timeout(Duration::from_millis(1500));
        assert!(heard.is_err(), "nothing happened to our document; got {heard:?}");
    }

    #[test]
    fn a_batch_that_names_where_the_file_went_beats_the_half_that_only_says_it_left() {
        let to = PathBuf::from("/home/p/Documents/pricing.md");
        let reduced = latest(vec![Move::Away, Move::To(to.clone())]);
        assert!(matches!(reduced, Some(Move::To(p)) if p == to));
        assert!(matches!(latest(vec![Move::Away]), Some(Move::Away)));
        assert!(latest(vec![]).is_none());
    }
}
