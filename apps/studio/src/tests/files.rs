//! One picture: its record in the right-hand pane, the Trash, opening it, the folder as it
//! changes, and which files an agent may name.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;
use yantrik_app::app::WindowId;
use yantrik_app::desktop::DesktopOutcome;
use yantrik_app::effects::Request;

use super::{apply, apply_as_agent, bare, fake};
use crate::backend;
use crate::command::{Command, Found, Next};
use crate::gallery::{self, Listed, Sidecar};
use crate::model::file_name;
use crate::update::POLL_EVERY;

fn detail(f: &super::Fixture) -> String {
    f.app.ui(|ui| ui.get_detail().to_string())
}

#[test]
fn choosing_a_row_reads_its_record_back_and_choosing_it_again_puts_the_pane_back() {
    let f = fake("choose");
    f.act("generate", json!({ "prompt": "a lighthouse in fog", "seed": 3, "width": 256, "height": 192, "steps": 4 }))
        .unwrap();
    assert_eq!(f.settled(), "");
    assert_eq!(detail(&f), "", "the right pane starts as an invitation, not as a record");

    f.app.run(Command::Choose(0));
    // The row shows as chosen at once; its record is read by a worker and drawn when it lands.
    assert!(f.rows()[0].selected);
    f.quiet();
    let shown = detail(&f);
    assert!(shown.contains("Prompt: a lighthouse in fog"), "{shown}");
    assert!(shown.contains("Kept out: "), "{shown}");
    assert!(shown.contains("Seed: 3"), "{shown}");
    assert!(shown.contains("Size: 256\u{d7}192"), "{shown}");
    assert!(shown.contains("Steps: 4"), "{shown}");
    assert!(shown.contains("Made by: placeholder"), "{shown}");
    assert!(!shown.contains("fake"), "{shown}");
    assert!(shown.contains("Took: "), "{shown}");
    assert!(shown.contains("When: "), "{shown}");

    // A second click on a selected row is what a person does to undo it.
    f.app.run(Command::Choose(0));
    f.quiet();
    assert_eq!(detail(&f), "");
    assert!(!f.rows()[0].selected);
}

/// A record that arrives for a row the person has since left is not drawn over the row they chose.
#[test]
fn a_record_for_a_row_the_person_has_left_is_not_drawn() {
    let f = fake("choose-late");
    f.act("generate", json!({ "prompt": "first", "seed": 1, "count": 2, "width": 64, "height": 64 })).unwrap();
    assert_eq!(f.settled(), "");
    let prompts: Vec<String> = f.app.model(|m| m.gallery.iter().map(|row| row.prompt.clone()).collect());
    assert_eq!(prompts.len(), 2);
    // Two reads in flight, for two rows, answered in order: the later choice is the one that stays.
    f.app.run(Command::Choose(0));
    f.app.run(Command::Choose(1));
    // The first read lands first, for a row that is no longer the chosen one: nothing is drawn.
    assert!(f.app.run_one_job());
    assert_eq!(detail(&f), "", "a record for a row the person had left was drawn");
    f.quiet();
    let seed_of_row_1 = f.app.model(|m| m.gallery[1].seed);
    assert!(detail(&f).contains(&format!("Seed: {seed_of_row_1}")), "{}", detail(&f));
    assert!(f.rows()[1].selected);
}

#[test]
fn a_chosen_picture_that_is_deleted_leaves_the_pane_empty() {
    let f = fake("choose-delete");
    let name = f.one_picture("a harbour");
    f.app.run(Command::Choose(0));
    f.quiet();
    assert!(!detail(&f).is_empty());
    f.act("delete", json!({ "path": name })).unwrap();
    assert_eq!(detail(&f), "");
    assert!(f.rows().is_empty(), "the deleted picture is still on screen");
}

#[test]
fn deleting_moves_the_picture_and_its_record_to_the_trash_and_nothing_is_read_on_the_ui_thread() {
    let f = fake("delete");
    f.act("generate", json!({ "prompt": "a picture to regret", "width": 64, "height": 64 })).unwrap();
    assert_eq!(f.settled(), "");
    let path = f.app.model(|m| m.gallery[0].path.clone());
    // The command only asks a worker: until it runs, the picture is where it was.
    f.app.run(Command::Delete { of: path.display().to_string() });
    assert!(path.exists(), "the update moved a file itself");
    f.quiet();
    assert!(!path.exists());
    assert!(!Sidecar::path_for(&path).exists(), "the record was left beside a picture that is gone");
    assert!(f.app.model(|m| m.gallery.is_empty()), "the gallery still shows it");
    assert!(f.notice().contains("Trash"), "{:?}", f.notice());
    let listed = crate::trash::items(&f.world.places.trash).unwrap();
    assert_eq!(listed.len(), 2, "{listed:?}");
}

#[test]
fn this_app_will_not_delete_a_file_outside_its_own_gallery() {
    let f = fake("outside");
    let elsewhere = f.world.dir.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let picture = elsewhere.join("important.png");
    std::fs::write(&picture, backend::draw("x", 1, 16, 16)).unwrap();
    f.app.run(Command::Delete { of: picture.display().to_string() });
    f.quiet();
    assert!(f.notice().contains("not in Studio's gallery"), "{}", f.notice());
    assert!(f.notice().contains("Files can delete anything"), "{}", f.notice());
    assert!(picture.exists(), "it was deleted anyway");
}

#[test]
fn a_bare_filename_means_the_gallery_and_a_path_means_itself() {
    let f = fake("names");
    f.act("generate", json!({ "prompt": "a lighthouse", "width": 64, "height": 64 })).unwrap();
    assert_eq!(f.settled(), "");
    let path = f.app.model(|m| m.gallery[0].path.clone());
    let name = file_name(&path);
    let canonical = std::fs::canonicalize(&path).unwrap();
    for named in [name.clone(), path.display().to_string()] {
        let found = f.act("upscale", json!({ "path": named, "factor": 2 })).unwrap();
        assert_eq!(found["of"], json!(named));
        assert_eq!(f.settled(), "");
    }
    let made: Vec<String> = f.app.model(|m| m.gallery.iter().map(|row| row.made_from.clone()).collect());
    assert_eq!(made.iter().filter(|each| **each == format!("upscale of {name}")).count(), 2, "{made:?}");
    assert!(canonical.exists());
}

#[test]
fn a_file_that_is_not_a_picture_is_not_treated_as_one() {
    let f = fake("not-a-picture");
    std::fs::write(f.world.dir.join("notes.txt"), "hello").unwrap();
    f.app.run(Command::Delete { of: f.world.dir.join("notes.txt").display().to_string() });
    f.quiet();
    assert!(f.notice().contains("not a picture"), "{}", f.notice());
    assert!(f.world.dir.join("notes.txt").exists(), "it was moved anyway");
    f.app.run(Command::Delete { of: f.world.dir.display().to_string() });
    f.quiet();
    assert!(f.notice().contains("a folder"), "{}", f.notice());
}

/// `open` hands a resolved picture to the desktop, and says so only once the desktop has: nothing
/// is spawned by the update, and the answer is the desktop's own outcome.
#[test]
fn open_is_answered_by_the_desktop_not_by_the_update() {
    let (world, mut m) = bare("open", r#"{"backend":{"kind":"fake"}}"#);
    let path = world.dir.join("seen.png");
    std::fs::write(&path, backend::draw("x", 1, 16, 16)).unwrap();

    let fx = apply_as_agent(&mut m, Command::Resolved { next: Next::Open, result: Ok(Found { path: path.clone(), sidecar: None }) });
    let asked: Vec<_> = fx.requests().iter().map(Request::name).collect();
    assert_eq!(asked, ["spawn"], "the desktop is asked on a worker, and nothing is answered yet");
    assert_eq!(m.answer, serde_json::Value::Null);

    let fx = apply_as_agent(&mut m, Command::Opened { shown: "seen.png".into(), outcome: DesktopOutcome::Done });
    assert_eq!(m.answer, json!({ "opened": "seen.png" }));
    assert!(fx.requests().iter().any(|r| r.name() == "settle"));

    let fx = apply_as_agent(
        &mut m,
        Command::Opened { shown: "seen.png".into(), outcome: DesktopOutcome::Failed("no opener".into()) },
    );
    assert_eq!(m.notice, "no opener");
    assert!(matches!(fx.requests().last(), Some(Request::Settle(Err(_)))));
}

#[test]
fn a_picture_somebody_else_puts_in_the_folder_turns_up_in_the_gallery() {
    let f = fake("poll");
    assert!(f.app.model(|m| m.gallery.is_empty()));
    let day = gallery::day_folder(&f.gallery(), chrono::Local::now());
    std::fs::create_dir_all(&day).unwrap();
    std::fs::write(day.join("copied-in.png"), backend::draw("a harbour", 5, 32, 32)).unwrap();

    // The poll's timer comes round; the folder is read on a worker.
    f.app.advance(POLL_EVERY + Duration::from_millis(10));
    f.quiet();
    let rows = f.app.model(|m| m.gallery.clone());
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].name, "copied-in.png");
    assert!(!rows[0].has_sidecar, "a record was invented for a file that has none");
    assert_eq!(rows[0].json()["sidecar"], json!("missing"));
}

#[test]
fn being_brought_to_the_front_looks_at_the_folder_again() {
    let f = fake("activated");
    let day = gallery::day_folder(&f.gallery(), chrono::Local::now());
    std::fs::create_dir_all(&day).unwrap();
    std::fs::write(day.join("while-away.png"), backend::draw("a harbour", 5, 32, 32)).unwrap();
    f.app.deactivate(WindowId::PRIMARY);
    f.app.activate(WindowId::PRIMARY);
    f.quiet();
    assert_eq!(f.app.model(|m| m.gallery.len()), 1, "what arrived while the window was behind was not noticed");
}

/// An unchanged folder is not announced as a change, which is what keeps an idle window from
/// redrawing and an idle app from costing a share of a core.
#[test]
fn a_poll_that_finds_the_same_pictures_redraws_nothing() {
    let (world, mut m) = bare("quiet-poll", r#"{"backend":{"kind":"fake"}}"#);
    let same = Listed { stamp: "x".into(), content: m.content.clone(), rows: m.gallery.clone() };
    let fx = apply(&mut m, Command::Listed(Box::new(same)));
    assert!(fx.changed_parts().is_empty(), "{:?}", fx.changed_parts());
    let fx = apply(&mut m, Command::Unchanged);
    assert!(fx.changed_parts().is_empty());
    drop(world);
}

/// Review of #942, finding 1: an agent names only files in the person's home, outside its protected
/// places. The person is not asked; a bare filename is a picture in the gallery.
#[test]
fn an_agent_names_only_files_in_the_home() {
    let f = fake("agent-files");
    let outside: PathBuf = f.world.dir.join("outside.png");
    std::fs::write(&outside, backend::draw("SENTINEL", 1, 16, 16)).unwrap();
    let outside = outside.display().to_string();

    // The person is not asked.
    f.act("upscale", json!({ "path": &outside })).unwrap();
    assert_eq!(f.settled(), "");

    for (action, key) in [("upscale", "path"), ("open", "path"), ("variations", "of")] {
        for asked in [
            outside.as_str(),
            "~/Pictures/Screenshots/shot.png",
            "~/.local/share/yantrik/snippets/snippets.json",
            "/etc/hostname",
        ] {
            let problem = f.agent(action, json!({ key: asked })).unwrap_err();
            assert!(problem.starts_with("an agent names only a file in the person's home"), "{action} {asked}: {problem}");
        }
    }
    // `set_backend` is the person's alone (tests/backends.rs): an agent naming a workflow is
    // refused before the file is looked at.
    let problem = f
        .agent("set_backend", json!({ "kind": "comfyui", "workflow": "/etc/hostname" }))
        .unwrap_err();
    assert_eq!(problem, crate::update::AGENT_REFUSED);
    assert!(!f.world.places.settings.as_ref().unwrap().exists(), "a refused change was written");

    // A bare name is a picture in the gallery, resolved there; delete keeps to the gallery itself.
    let problem = f.agent("variations", json!({ "of": "studio-1.png" })).unwrap_err();
    assert!(problem.contains("there is no studio-1.png in the gallery"), "{problem}");
    let problem = f.agent("delete", json!({ "path": &outside })).unwrap_err();
    assert!(problem.contains("only deletes its own pictures"), "{problem}");
    assert!(PathBuf::from(&outside).exists());
}

/// A hand-edited `studio.json` that exists and cannot be used says so where the person and a Mind
/// look, not only in the log: which file, and what is wrong. The variable's value is never said.
#[test]
fn a_configuration_that_was_not_used_says_which_file_and_what_is_wrong() {
    const NAME: &str = "STUDIO-BAD-KEY";
    std::env::set_var(NAME, "sk-never-said-5521");
    let dir = std::env::temp_dir().join(format!("studio-refused-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("studio.json");
    std::fs::write(
        &file,
        format!(r#"{{"backend":{{"kind":"openai-images","model":"m","api_key_env":"{NAME}"}}}}"#),
    )
    .unwrap();
    let config = crate::config::Config::read(&file);
    assert_eq!(config.refused.as_deref().map(|s| s.contains("was not used")), Some(true));

    let f = super::with_config("refused-config", config);
    let said = f.notice();
    let window = f.app.ui(|ui: &crate::StudioApp| (ui.get_notice().to_string(), ui.get_facts().to_string()));
    let described = f.describe();
    for (where_, text) in [
        ("the notice", said.clone()),
        ("the window's notice", window.0.clone()),
        ("the pane", window.1.clone()),
        ("describe", described["state"].to_string()),
        ("the summary", f.summary()),
    ] {
        assert!(text.contains("studio.json was not used"), "{where_}: {text}");
        assert!(text.contains("has to be the name of an environment variable"), "{where_}: {text}");
        assert!(text.contains(NAME), "{where_} does not name the variable: {text}");
        assert!(!text.contains("sk-never-said"), "{where_} says the value: {text}");
    }
    assert_eq!(window.0, said, "the window and the model disagree");
    assert_eq!(described["state"]["backend"]["note"].as_str().unwrap(), described["state"]["notice"].as_str().unwrap());
    // The app still runs, on placeholders.
    assert_eq!(described["state"]["backend"]["kind"], serde_json::json!("fake"));
    std::env::remove_var(NAME);
    let _ = std::fs::remove_dir_all(&dir);
}
