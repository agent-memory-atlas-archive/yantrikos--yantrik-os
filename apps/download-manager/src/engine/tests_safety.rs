//! The review of #958 at the engine: concurrent saves (M3), private files (M4), a finished
//! download's file under cancel and retry (M1), and the agent mark kept across a restart.

use super::tests::{saved, Store};
use super::*;

fn put(engine: &Engine, download: Download) {
    let mut state = engine.state.lock().unwrap();
    state.controls.insert(download.id, Arc::new(AtomicU8::new(control::RUN)));
    state.items.push(download);
}

#[test]
fn many_concurrent_saves_and_a_close_leave_the_list_valid_and_whole() {
    let store = Store::new();
    let engine = store.engine();
    for id in 1..=8 {
        put(&engine, saved(id, &format!("f{id}.bin"), Status::Paused, &store.0, 10, 100));
    }
    // A reader parses the list and watches the notice for the whole run: a later good save
    // overwrites a torn file and clears "cannot save", so looking only at the end misses both.
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reader = {
        let (engine, done, file) = (engine.clone(), done.clone(), store.state_file());
        std::thread::spawn(move || {
            let mut bad = Vec::new();
            while !done.load(Ordering::Relaxed) {
                if let Ok(raw) = std::fs::read_to_string(&file) {
                    if decode(&raw).is_err() {
                        bad.push("unparseable state.json".to_string());
                    }
                }
                if !engine.notice().is_empty() {
                    bad.push(engine.notice());
                }
            }
            bad
        })
    };
    let writers: Vec<_> = (0..8)
        .map(|_| {
            let engine = engine.clone();
            std::thread::spawn(move || (0..150).for_each(|_| engine.save()))
        })
        .collect();
    engine.settle_for_exit(Duration::from_millis(50));
    writers.into_iter().for_each(|w| w.join().unwrap());
    done.store(true, Ordering::Relaxed);
    let bad = reader.join().unwrap();
    assert!(bad.is_empty(), "a reader saw a torn list or a failed save: {bad:?}");
    let raw = std::fs::read_to_string(store.state_file()).unwrap();
    let (items, _) = decode(&raw).expect("state.json parses");
    assert_eq!(items.len(), 8, "every row is in it");
    assert_eq!(engine.notice(), "", "no save failed");
    assert_eq!(store.entries(), vec!["state.json".to_string()], "no temporary is left behind");
}

#[cfg(unix)]
mod modes {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn the_list_and_its_folder_are_the_persons_alone() {
        let store = Store::new();
        let nested = store.0.join("a/downloads");
        let engine = Engine::open(nested.clone());
        put(&engine, saved(1, "x.bin", Status::Paused, &store.0, 1, 2));
        engine.save();
        assert_eq!(mode(&nested), 0o700);
        assert_eq!(mode(&nested.join("state.json")), 0o600);
    }

    #[test]
    fn a_folder_that_is_already_open_is_tightened_at_start() {
        let store = Store::new();
        std::fs::set_permissions(&store.0, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(store.state_file(), "{\"version\":1,\"next_id\":1,\"downloads\":[]}").unwrap();
        std::fs::set_permissions(store.state_file(), std::fs::Permissions::from_mode(0o644)).unwrap();
        let engine = store.engine();
        assert_eq!(mode(&store.0), 0o700);
        engine.save();
        assert_eq!(mode(&store.state_file()), 0o600, "and the next save is private too");
    }
}

#[test]
fn cancel_takes_a_finished_rows_entry_and_never_its_file() {
    let store = Store::new();
    let engine = store.engine();
    store.part("done.bin", 100);
    put(&engine, saved(1, "done.bin", Status::Completed, &store.0, 100, 100));
    engine.cancel(1).unwrap();
    assert!(engine.get(1).is_none());
    assert!(store.0.join("done.bin").exists());
    // And it stays gone after a restart.
    assert!(store.engine().get(1).is_none());
}

#[test]
fn retry_of_a_missing_row_leaves_what_now_sits_at_its_path() {
    let store = Store::new();
    let engine = store.engine();
    let mut gone = saved(1, "moved.bin", Status::Missing, &store.0, 100, 100);
    gone.url = "http://127.0.0.1:1/moved.bin".into();
    put(&engine, gone);
    store.part("moved.bin", 5);
    engine.retry(1).unwrap();
    for _ in 0..500 {
        if engine.get(1).is_some_and(|d| d.status == Status::Failed) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(std::fs::metadata(store.0.join("moved.bin")).unwrap().len(), 5, "the other file is untouched");
    assert_eq!(engine.get(1).unwrap().filename, "moved (1).bin");
}

#[test]
fn the_agent_mark_survives_a_restart() {
    let mut d = saved(1, "a.bin", Status::Paused, Path::new("/tmp"), 1, 2);
    d.agent = true;
    let (back, _) = decode(&encode(&[d], 2)).unwrap();
    assert!(back[0].agent);
}

#[test]
fn cancel_and_retry_of_a_failed_row_that_wrote_nothing_keep_the_persons_files() {
    // The row failed before its file was created. Since then the person saved their own files at
    // the download's path (and at a `.part` name beside it).
    let store = Store::new();
    let engine = store.engine();
    let mut row = saved(1, "report.pdf", Status::Failed, &store.0, 0, 100);
    row.error = "HTTP 404".into();
    put(&engine, row.clone());
    put(&engine, { let mut r = row.clone(); r.id = 2; r });
    std::fs::write(store.0.join("report.pdf"), "the person's own report").unwrap();
    std::fs::write(store.0.join("report.pdf.part"), "the person's own part").unwrap();

    engine.cancel(1).unwrap();
    assert_eq!(std::fs::read_to_string(store.0.join("report.pdf")).unwrap(), "the person's own report");
    assert_eq!(std::fs::read_to_string(store.0.join("report.pdf.part")).unwrap(), "the person's own part");

    let _ = engine.retry(2);
    assert_eq!(std::fs::read_to_string(store.0.join("report.pdf")).unwrap(), "the person's own report");
    assert_eq!(std::fs::read_to_string(store.0.join("report.pdf.part")).unwrap(), "the person's own part");
    assert_ne!(engine.get(2).unwrap().filename, "report.pdf", "the retry takes a free name");
}

#[test]
fn cancel_removes_the_partial_file_it_created_and_nothing_swapped_in_after() {
    let store = Store::new();
    let engine = store.engine();
    // Its own partial file: recorded when it was created, still the same plain file.
    std::fs::write(store.0.join("own.bin"), "partial").unwrap();
    let mut own = saved(1, "own.bin", Status::Paused, &store.0, 7, 100);
    own.file_id = identity(&std::fs::metadata(store.0.join("own.bin")).unwrap());
    put(&engine, own);
    // The same, but the person replaced the file with another one of the same name.
    std::fs::write(store.0.join("swap.bin"), "partial").unwrap();
    let mut swapped = saved(2, "swap.bin", Status::Paused, &store.0, 7, 100);
    swapped.file_id = identity(&std::fs::metadata(store.0.join("swap.bin")).unwrap());
    put(&engine, swapped);
    // Made before the old one goes, so the two cannot share an inode number.
    std::fs::write(store.0.join("theirs.tmp"), "theirs").unwrap();
    std::fs::rename(store.0.join("theirs.tmp"), store.0.join("swap.bin")).unwrap();

    engine.cancel(1).unwrap();
    engine.cancel(2).unwrap();
    assert!(!store.0.join("own.bin").exists(), "its own partial file goes");
    assert_eq!(std::fs::read_to_string(store.0.join("swap.bin")).unwrap(), "theirs");
}

#[test]
fn resume_never_appends_to_a_file_that_is_not_the_downloads_own() {
    let store = Store::new();
    let engine = store.engine();
    // Its own partial file, then the person replaced it with theirs (made first, so the inode differs).
    std::fs::write(store.0.join("big.iso"), "partial").unwrap();
    let mut row = saved(1, "big.iso", Status::Paused, &store.0, 7, 100);
    row.file_id = identity(&std::fs::metadata(store.0.join("big.iso")).unwrap());
    put(&engine, row);
    std::fs::write(store.0.join("theirs.tmp"), "the person's own file").unwrap();
    std::fs::rename(store.0.join("theirs.tmp"), store.0.join("big.iso")).unwrap();
    // A failed row that never wrote anything, with the person's file at its path.
    put(&engine, saved(2, "doc.pdf", Status::Failed, &store.0, 0, 100));
    std::fs::write(store.0.join("doc.pdf"), "also theirs").unwrap();

    assert!(engine.resume(1).is_err());
    assert!(engine.resume(2).is_err());
    assert_eq!(std::fs::read_to_string(store.0.join("big.iso")).unwrap(), "the person's own file");
    assert_eq!(std::fs::read_to_string(store.0.join("doc.pdf")).unwrap(), "also theirs");
    assert_eq!(engine.get(1).unwrap().status, Status::Paused, "the row is left as it was");
}
