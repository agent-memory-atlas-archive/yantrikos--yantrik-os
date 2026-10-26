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
    let writers: Vec<_> = (0..8)
        .map(|_| {
            let engine = engine.clone();
            std::thread::spawn(move || (0..60).for_each(|_| engine.save()))
        })
        .collect();
    engine.settle_for_exit(Duration::from_millis(50));
    writers.into_iter().for_each(|w| w.join().unwrap());
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
