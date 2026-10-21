//! Notes under test: headless through yantrik-testing, with a vault of its own under the test's
//! temporary root, jobs run when the test says, the autosave's clock moved by hand, and the disk
//! made to fail where a test asks.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;
use yantrik_app::documents::disk::FaultyDisk;
use yantrik_testing::TestApp;

use crate::command::Command;
use crate::update::AUTOSAVE_AFTER;
use crate::Notes;

mod kept;
mod parity;
mod read_notes;
mod read_only;
mod saving;
mod startup;
mod store;
mod surface;
mod version;
mod window;

/// A Notes with its library read.
pub fn started() -> TestApp<Notes> {
    let app = TestApp::<Notes>::start();
    settle(&app);
    app
}

/// Runs jobs until none is left: a save's landing may start its follow-up, an action waiting on
/// a save starts its own job, and so on.
pub fn settle(app: &TestApp<Notes>) {
    for _ in 0..32 {
        if app.jobs_waiting() == 0 {
            return;
        }
        app.run_jobs();
    }
    panic!("jobs kept starting jobs");
}

/// The autosave's debounce runs out, and what it started lands.
pub fn autosave(app: &TestApp<Notes>) {
    app.advance(AUTOSAVE_AFTER);
    settle(app);
}

pub fn vault(app: &TestApp<Notes>) -> PathBuf {
    app.paths().data_dir().to_path_buf()
}

/// Everything under `dir`, by path: each file's bytes, and each folder (with no bytes), so a test
/// can say a vault was left exactly as it was.
pub fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut seen = BTreeMap::new();
    let mut folders = vec![dir.to_path_buf()];
    while let Some(folder) = folders.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                seen.insert(path.clone(), None);
                folders.push(path);
            } else {
                seen.insert(path.clone(), Some(std::fs::read(&path).unwrap_or_default()));
            }
        }
    }
    seen
}

/// The vault writes through `disk` from now on.
pub fn failing(app: &TestApp<Notes>) -> Arc<FaultyDisk> {
    let disk = Arc::new(FaultyDisk::new());
    app.run(Command::UseDisk(disk.clone()));
    disk
}

/// One call through the real dispatch, answered once its work has landed.
pub fn act(app: &TestApp<Notes>, name: &str, args: Value) -> Result<Value, String> {
    let pending = app.act(name, args)?;
    settle(app);
    pending.settle()
}

/// What `describe` says now, its state object.
pub fn state(app: &TestApp<Notes>) -> Value {
    app.describe()["state"].clone()
}

/// A note made through the surface, with `body` under the title, saved.
pub fn note(app: &TestApp<Notes>, title: &str, body: &str) -> Value {
    act(app, "new_note", serde_json::json!({ "title": title, "text": body })).expect("new_note")
}
