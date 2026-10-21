//! One copy per library: a second Notes on a library another copy holds does not start, and the
//! lock in the vault is never a note.

use std::sync::Arc;

use yantrik_app::app::{App as _, Boot};
use yantrik_app::mind::fake::FakeMind;
use yantrik_app::paths::Paths;
use yantrik_app::Services;
use yantrik_testing::TestApp;

use super::{settle, started, state, vault};
use crate::claim::{ALREADY_RUNNING, LEGACY_NAME, VAULT_NAME};
use crate::Notes;

fn boot(paths: &Paths) -> Boot {
    Boot::<()>::open(paths.clone(), Vec::new(), Arc::new(FakeMind::new()), Services::fakes())
        .expect("a launch with no arguments")
        .0
}

/// What a second start at `paths` says, or `None` when it started.
fn second_start(paths: &Paths) -> Option<String> {
    Notes::model(&boot(paths)).err().map(|e| e.sentence)
}

#[test]
fn a_second_copy_on_a_held_library_does_not_start() {
    let paths = Paths::temporary(Notes::ID);
    let _first = TestApp::<Notes>::start_at(paths.clone(), Vec::new());
    assert_eq!(second_start(&paths).as_deref(), Some(ALREADY_RUNNING));
}

#[test]
fn the_old_builds_lock_alone_keeps_this_build_out() {
    let paths = Paths::temporary(Notes::ID);
    std::fs::create_dir_all(paths.state_dir()).unwrap();
    // As the build before the framework held it: the same file, flocked.
    let old_build = yantrik_app::lock::hold(&paths.state_dir().join(LEGACY_NAME)).unwrap();
    assert_eq!(second_start(&paths).as_deref(), Some(ALREADY_RUNNING));
    drop(old_build);
    assert_eq!(second_start(&paths), None, "free once the old build is gone");
}

#[test]
fn the_vault_lock_alone_keeps_a_second_profile_on_the_same_vault_out() {
    let paths = Paths::temporary(Notes::ID);
    std::fs::create_dir_all(paths.data_dir()).unwrap();
    let other_profile = yantrik_app::lock::hold(&paths.data_dir().join(VAULT_NAME)).unwrap();
    assert_eq!(second_start(&paths).as_deref(), Some(ALREADY_RUNNING));
    drop(other_profile);
    assert_eq!(second_start(&paths), None);
}

#[test]
fn the_vault_lock_is_never_a_note() {
    let app = started();
    assert!(vault(&app).join(VAULT_NAME).exists(), "the lock lives in the vault");
    assert_eq!(state(&app)["note_count"], 0);
    app.run(crate::command::Command::Window(crate::command::WindowAction::Reload));
    settle(&app);
    assert_eq!(state(&app)["note_count"], 0, "nor after a reload");
    assert_eq!(state(&app)["notice"], "", "and not a file that could not be loaded either");
}
