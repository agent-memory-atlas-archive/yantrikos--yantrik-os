//! Yantrik Arcade — a game construction kit.
//!
//! A mind writes small specs in two bounded JSON grammars (a character, a game); a
//! deterministic compiler turns them into one self-contained HTML file with the game
//! loop, juice, sound and a chunky stylised creature already inside it. The window is
//! the workbench — library on the left, spec editor on the right — and the control
//! surface publishes the same seven commands the buttons call, so a mind can drive the
//! whole kit without a synthetic mouse.
//!
//! Three rules shaped the code:
//!
//! - ONE PATH. Every button and every surface action lands in `run_action`; there is
//!   no private reimplementation for either caller.
//! - NOTHING SLOW ON THE UI THREAD. Build is milliseconds and runs inline. Verify,
//!   screenshot and play each launch or reach a browser — seconds to minutes — so they
//!   declare `defers`, hand the work to a worker thread, and answer "started". The
//!   verdict lands in the library's verify.json and in `describe` when it is ready.
//! - REFUSALS ARE SENTENCES. A bad spec, an unknown game, a missing browser: each says
//!   in one sentence what is wrong and what to do, because the reader is usually a mind.
//!
//! The same binary has a headless CLI (`yantrik-arcade build|verify|screenshot …`) for
//! scripted runs and for testing on a machine with no display.

mod cli;
mod compile;
mod library;
mod play;
mod spec;
mod verify;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use yantrik_app_runtime::control::{Action, App, Param, View};
use yantrik_app_runtime::prelude::*;

use library::{GameEntry, Library};

slint::include_modules!();

const APP_ID: &str = "arcade";

fn main() {
    // CLI mode comes first and opens no window: a scripted build or verify must work
    // with no display and must not fight the running app for the instance lock.
    if let Some(code) = cli::run(std::env::args().collect()) {
        std::process::exit(code);
    }

    init_tracing("yantrik-arcade");

    // One window per app: a second launch defers to the running one (the shell focuses it).
    let Some(_instance) = instance::claim(APP_ID) else { return };

    let app = ArcadeApp::new().unwrap();

    // Same dark/accent choice as the shell, read from the shell's settings file.
    let theme = theme::load();
    app.global::<ThemeMode>().set_dark(theme.dark);
    app.global::<AccentPreset>().set_index(theme.accent_index);

    let core = Core::new();
    wire(&app, &core);
    refresh(&app, &core);
    control(&app, core.clone());

    app.run().unwrap();
}

// ── Jobs: the record of slow work ──────────────────────────────────

#[derive(Clone)]
enum JobStatus {
    Running,
    Done(String),
    Failed(String),
}

#[derive(Clone)]
struct Job {
    id: u64,
    action: String,
    subject: String,
    status: JobStatus,
}

/// What the workers and the window share: the list of slow jobs, newest last.
/// An Arc<Mutex> rather than anything clever because both ends are simple — a
/// worker writes one status, the UI thread reads the list.
#[derive(Clone)]
struct Core {
    jobs: Arc<Mutex<Vec<Job>>>,
    next_id: Arc<AtomicU64>,
}

impl Core {
    fn new() -> Core {
        Core { jobs: Arc::new(Mutex::new(Vec::new())), next_id: Arc::new(AtomicU64::new(1)) }
    }

    fn start_job(&self, action: &str, subject: &str) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let mut jobs = self.jobs.lock().unwrap();
        jobs.push(Job { id, action: action.into(), subject: subject.into(), status: JobStatus::Running });
        // The list is a glance, not a transcript: keep the newest eight.
        while jobs.len() > 8 {
            jobs.remove(0);
        }
        id
    }

    fn finish_job(&self, id: u64, result: Result<String, String>) {
        let mut jobs = self.jobs.lock().unwrap();
        if let Some(job) = jobs.iter_mut().find(|j| j.id == id) {
            job.status = match result {
                Ok(msg) => JobStatus::Done(msg),
                Err(msg) => JobStatus::Failed(msg),
            };
        }
    }

    fn running(&self) -> Vec<Job> {
        self.jobs
            .lock()
            .unwrap()
            .iter()
            .filter(|j| matches!(j.status, JobStatus::Running))
            .cloned()
            .collect()
    }

    fn jobs_json(&self) -> serde_json::Value {
        let jobs = self.jobs.lock().unwrap();
        serde_json::Value::Array(
            jobs.iter()
                .rev()
                .map(|j| {
                    let (status, detail) = match &j.status {
                        JobStatus::Running => ("running", String::new()),
                        JobStatus::Done(m) => ("done", m.clone()),
                        JobStatus::Failed(m) => ("failed", m.clone()),
                    };
                    serde_json::json!({
                        "id": j.id, "action": j.action, "subject": j.subject,
                        "status": status, "detail": detail,
                    })
                })
                .collect(),
        )
    }
}

// ── The one code path ──────────────────────────────────────────────

/// Show the outcome of one command in the window and hand it back to whoever asked.
/// The button ignores the value; the surface returns it. Both go through here, so an
/// agent and a person cannot get different answers about whether something worked.
fn settle(ui: &ArcadeApp, result: Result<serde_json::Value, String>) -> Result<serde_json::Value, String> {
    match &result {
        Ok(value) => {
            // Success replaces any old complaint with what just happened.
            ui.set_notice(summary_of(value).into());
        }
        Err(message) => {
            ui.set_notice(message.clone().into());
        }
    }
    result
}

/// One line out of a successful result, for the banner.
fn summary_of(value: &serde_json::Value) -> String {
    for key in ["summary", "built", "saved", "deleted", "started"] {
        if let Some(text) = value.get(key).and_then(|v| v.as_str()) {
            return text.to_string();
        }
    }
    if value.get("started").is_some() {
        return "started".into();
    }
    String::new()
}

/// Every command, from a button or from the socket, lands here.
///
/// `args` uses the surface's spelling (`spec`, `game`); the buttons build the same
/// object out of the window's fields before calling.
fn run_action(
    ui: &ArcadeApp,
    core: &Core,
    name: &str,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    match name {
        "new_character" => {
            let spec = arg_str(args, "spec", "the character JSON to save")?;
            let entry = Library::open().save_character(&spec).map_err(|e| refuse(ui, e))?;
            refresh(ui, core);
            Ok(serde_json::json!({
                "summary": format!("saved character {:?}", entry.name),
                "saved": format!("saved character {:?}", entry.name),
                "slug": entry.slug, "name": entry.name, "archetype": entry.archetype,
            }))
        }
        "new_game" => {
            let spec = arg_str(args, "spec", "the game JSON to save")?;
            let entry = Library::open().save_game(&spec).map_err(|e| refuse(ui, e))?;
            ui.set_game_name(entry.slug.clone().into());
            refresh(ui, core);
            Ok(serde_json::json!({
                "summary": format!("saved game {:?}; build it next", entry.title),
                "saved": format!("saved game {:?}", entry.title),
                "slug": entry.slug, "title": entry.title, "built": entry.built,
            }))
        }
        "build" => {
            let game = game_arg(ui, args)?;
            let (slug, path) = Library::open().build_game(&game).map_err(|e| refuse(ui, e))?;
            ui.set_game_name(slug.clone().into());
            refresh(ui, core);
            Ok(serde_json::json!({
                "summary": format!("built {slug}"),
                "built": format!("built {slug} → {}", path.display()),
                "slug": slug, "path": path.display().to_string(),
            }))
        }
        "play" => {
            let game = game_arg(ui, args)?;
            let id = spawn_play(ui, core, &game).map_err(|e| refuse(ui, e))?;
            refresh(ui, core);
            Ok(started("play", &game, id))
        }
        "verify" => {
            let game = game_arg(ui, args)?;
            let id = spawn_verify(ui, core, &game).map_err(|e| refuse(ui, e))?;
            refresh(ui, core);
            Ok(started("verify", &game, id))
        }
        "screenshot" => {
            let game = game_arg(ui, args)?;
            let id = spawn_screenshot(ui, core, &game).map_err(|e| refuse(ui, e))?;
            refresh(ui, core);
            Ok(started("screenshot", &game, id))
        }
        "delete" => {
            let game = game_arg(ui, args)?;
            let message = Library::open().delete_game(&game).map_err(|e| refuse(ui, e))?;
            ui.set_game_name(SharedString::default());
            refresh(ui, core);
            Ok(serde_json::json!({ "summary": message.clone(), "deleted": message }))
        }
        other => Err(format!(
            "Arcade has no action `{other}`; it knows new_character, new_game, build, play, verify, screenshot and delete"
        )),
    }
}

fn started(action: &str, game: &str, job: u64) -> serde_json::Value {
    serde_json::json!({
        "summary": format!("{action} of {game} started on a worker; `describe` reports it when it lands"),
        "started": true,
        "job": job,
        "action": action,
        "game": game,
    })
}

fn refuse(ui: &ArcadeApp, message: impl Into<String>) -> String {
    let message = message.into();
    ui.set_notice(message.clone().into());
    message
}

fn arg_str(args: &serde_json::Value, name: &str, what: &str) -> Result<String, String> {
    args.get(name)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .ok_or_else(|| format!("`{name}` is required: {what}"))
}

/// Which game an action means: the caller's `game` argument, or the window's
/// selection. A missing selection is a sentence, not a guess at the first row.
fn game_arg(ui: &ArcadeApp, args: &serde_json::Value) -> Result<String, String> {
    if let Some(game) = args.get("game").and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty()) {
        return Ok(game.trim().to_string());
    }
    let selected = ui.get_game_name().to_string();
    if selected.trim().is_empty() {
        return Err("no game is selected; pass `game=` or pick one in the window's list".into());
    }
    Ok(selected.trim().to_string())
}

// ── The slow three: workers ────────────────────────────────────────

/// Resolve the game and check the build exists *now*, so an unknown or unbuilt game
/// is refused inline with its sentence; only real browser work goes to the thread.
fn prepared_built_game(ui: &ArcadeApp, game: &str) -> Result<(Library, String, std::path::PathBuf), String> {
    let lib = Library::open();
    let (slug, _spec) = lib.load_game_spec(game).map_err(|e| refuse(ui, e))?;
    let html = lib.game_html(&slug);
    if !html.exists() {
        return Err(refuse(
            ui,
            format!("{slug} has not been built yet; `build game={slug}` first"),
        ));
    }
    Ok((lib, slug, html))
}

fn spawn_verify(ui: &ArcadeApp, core: &Core, game: &str) -> Result<u64, String> {
    let (lib, slug, html) = prepared_built_game(ui, game)?;
    let id = core.start_job("verify", &slug);
    let core2 = core.clone();
    let weak = ui.as_weak();
    std::thread::Builder::new()
        .name(format!("arcade-verify-{id}"))
        .spawn(move || {
            let shot = lib.screenshot_path(&slug);
            let result = verify::verify_game(&html, Some(&shot)).and_then(|report| {
                let value = serde_json::to_value(&report).map_err(|e| e.to_string())?;
                lib.write_verify(&slug, &value)?;
                if report.passed {
                    Ok(format!("{slug}: passed all {} gates", report.gates.len()))
                } else {
                    let first = report.gates.iter().find(|g| !g.passed);
                    Err(match first {
                        Some(g) => format!("{slug} failed verification at {}: {}", g.gate, g.detail),
                        None => format!("{slug} failed verification"),
                    })
                }
            });
            core2.finish_job(id, result.clone());
            post_result(weak, core2, result);
        })
        .map_err(|e| format!("cannot start the verification worker: {e}"))?;
    Ok(id)
}

fn spawn_screenshot(ui: &ArcadeApp, core: &Core, game: &str) -> Result<u64, String> {
    let (lib, slug, html) = prepared_built_game(ui, game)?;
    let id = core.start_job("screenshot", &slug);
    let core2 = core.clone();
    let weak = ui.as_weak();
    std::thread::Builder::new()
        .name(format!("arcade-shot-{id}"))
        .spawn(move || {
            let shot = lib.screenshot_path(&slug);
            let result = verify::screenshot_game(&html, &shot)
                .map(|()| format!("{} → {}", slug, shot.display()));
            core2.finish_job(id, result.clone());
            post_result(weak, core2, result);
        })
        .map_err(|e| format!("cannot start the screenshot worker: {e}"))?;
    Ok(id)
}

fn spawn_play(ui: &ArcadeApp, core: &Core, game: &str) -> Result<u64, String> {
    let (_lib, slug, html) = prepared_built_game(ui, game)?;
    let id = core.start_job("play", &slug);
    let core2 = core.clone();
    let weak = ui.as_weak();
    std::thread::Builder::new()
        .name(format!("arcade-play-{id}"))
        .spawn(move || {
            let result = play::play(&html);
            core2.finish_job(id, result.clone());
            post_result(weak, core2, result);
        })
        .map_err(|e| format!("cannot start the play worker: {e}"))?;
    Ok(id)
}

/// A worker's verdict, onto the UI thread: banner, models, status line.
fn post_result(weak: slint::Weak<ArcadeApp>, core: Core, result: Result<String, String>) {
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = weak.upgrade() {
            match result {
                Ok(msg) => ui.set_notice(msg.into()),
                Err(msg) => ui.set_notice(msg.into()),
            }
            refresh(&ui, &core);
        }
    });
}

// ── The window ─────────────────────────────────────────────────────

fn wire(app: &ArcadeApp, core: &Core) {
    let core_action = core.clone();
    let weak = app.as_weak();
    app.on_action(move |id| {
        let Some(ui) = weak.upgrade() else { return };
        // Button ids are the surface names, dashed where the surface underscores.
        let name = match id.as_str() {
            "new-character" => "new_character",
            "new-game" => "new_game",
            other => other,
        };
        let args = match name {
            "new_character" | "new_game" => serde_json::json!({ "spec": ui.get_spec_text().to_string() }),
            _ => serde_json::json!({ "game": ui.get_game_name().to_string() }),
        };
        let result = run_action(&ui, &core_action, name, &args);
        let _ = settle(&ui, result);
    });

    let core_games = core.clone();
    let weak2 = app.as_weak();
    app.on_pick_game(move |slug| {
        if let Some(ui) = weak2.upgrade() {
            ui.set_game_name(slug);
            refresh(&ui, &core_games);
        }
    });

    let weak3 = app.as_weak();
    app.on_pick_character(move |slug| {
        // Clicking a character loads its spec into the editor: the fastest way to
        // make the next one is to look at the last one.
        if let Some(ui) = weak3.upgrade() {
            match Library::open().load_character(slug.as_str()) {
                Ok(spec) => {
                    if let Ok(json) = serde_json::to_string_pretty(&spec) {
                        ui.set_spec_text(json.into());
                    }
                    ui.set_notice("".into());
                }
                Err(msg) => {
                    ui.set_notice(msg.into());
                }
            }
        }
    });
}

/// Rebuild both lists and the status line from the library on disk. Called after
/// every command and when a worker lands; cheap enough to never need caching.
fn refresh(ui: &ArcadeApp, core: &Core) {
    let lib = Library::open();
    let games = lib.list_games();
    let characters = lib.list_characters();
    let selected = ui.get_game_name().to_string();

    let rows: Vec<GameRow> = games
        .iter()
        .map(|g| GameRow {
            slug: g.slug.clone().into(),
            title: g.title.clone().into(),
            detail: detail_line(g).into(),
            selected: g.slug == selected || g.title == selected,
        })
        .collect();
    ui.set_games(ModelRc::new(VecModel::from(rows)));

    let crows: Vec<CharacterRow> = characters
        .iter()
        .map(|c| CharacterRow {
            slug: c.slug.clone().into(),
            name: c.name.clone().into(),
            archetype: c.archetype.clone().into(),
            selected: false,
        })
        .collect();
    ui.set_characters(ModelRc::new(VecModel::from(crows)));

    let running = core.running();
    let status = match running.first() {
        Some(job) => format!("{} {} running…", job.action, job.subject),
        None => format!("{} games, {} characters", games.len(), characters.len()),
    };
    ui.set_status(status.into());
    ui.set_busy(!running.is_empty());
}

/// A game row's second line: what exists on disk, terse.
fn detail_line(g: &GameEntry) -> String {
    if !g.built {
        return "not built".into();
    }
    match (&g.last_build, &g.verification) {
        (Some(when), Some(v)) => format!("built {when} · {v}"),
        (Some(when), None) => format!("built {when} · not verified"),
        (None, Some(v)) => v.clone(),
        (None, None) => "built".into(),
    }
}

// ── The control surface ────────────────────────────────────────────

fn view(ui: &ArcadeApp, core: &Core) -> View {
    let lib = Library::open();
    let games = lib.list_games();
    let characters = lib.list_characters();
    let running = core.running();

    let mut summary = format!("Arcade — {} games, {} characters", games.len(), characters.len());
    if let Some(job) = running.first() {
        summary = format!("Arcade — {} of {} running", job.action, job.subject);
    }

    View::new(summary)
        .with("library", lib.root().display().to_string())
        .with("selected", ui.get_game_name().to_string())
        .with(
            "games",
            games
                .iter()
                .take(20)
                .map(|g| {
                    serde_json::json!({
                        "slug": g.slug, "title": g.title, "built": g.built,
                        "last_build": g.last_build, "verification": g.verification,
                    })
                })
                .collect::<Vec<_>>(),
        )
        .with(
            "characters",
            characters
                .iter()
                .take(20)
                .map(|c| serde_json::json!({ "name": c.name, "slug": c.slug, "archetype": c.archetype }))
                .collect::<Vec<_>>(),
        )
        .with("jobs", core.jobs_json())
        // The grammar, read off the same constants the validator enforces. It lives in the state
        // as well as in the argument text because a caller that has already been refused once
        // reads `describe` next, and that read should end the guessing rather than continue it.
        .with(
            "spec_grammar",
            serde_json::json!({
                "vocabularies": spec::vocabularies(),
                "ranges": spec::ranges(),
                "note": "Every enumerated field and its words, and every numeric field and its                          range, exactly as new_character and new_game enforce them.",
            }),
        )
}

fn control(ui: &ArcadeApp, core: Core) {
    let weak = ui.as_weak();
    let core_d = core.clone();
    let mut app = App::new(APP_ID).describe(move || match weak.upgrade() {
        Some(ui) => view(&ui, &core_d),
        None => View::new("Arcade — the window is closed"),
    });
    for (spec, run) in surface(ui, core) {
        app = app.action(spec, run);
    }
    // Served from the UI thread, after the first refresh, before run().
    app.serve();
}

type Handler = Box<dyn Fn(&serde_json::Value) -> Result<serde_json::Value, String>>;

/// The seven commands, with the grades from the charter. `new_character`, `new_game`
/// and `build` are plain file writes (standard). `play`, `verify` and `screenshot`
/// reach a browser and defer to workers. `delete` goes to the Trash and is
/// recoverable from Files, which keeps it standard rather than dangerous.
fn surface(ui: &ArcadeApp, core: Core) -> Vec<(Action, Handler)> {
    fn handler(
        ui: &ArcadeApp,
        core: &Core,
        name: &'static str,
    ) -> Handler {
        let weak = ui.as_weak();
        let core = core.clone();
        Box::new(move |args| {
            let Some(ui) = weak.upgrade() else {
                return Err("the Arcade window is closing".into());
            };
            let result = run_action(&ui, &core, name, args);
            settle(&ui, result)
        })
    }

    vec![
        (
            Action::new("new_character", "Save a character spec into the library")
                .arg(Param::text("spec").describe(&format!(
                    "The character JSON. Required: name, archetype, proportions (head_body, limb_length, width). Optional: ears, tail, palette (base/belly/accent/nose/eye as #rrggbb), expression, stance. The enumerated fields take {}. Ranges and the rest are in `describe`'s spec_grammar. A value outside any of them is refused with a sentence naming the field.",
                    spec::vocabulary_line("character")
                ))),
            handler(ui, &core, "new_character"),
        ),
        (
            Action::new("new_game", "Save a game spec into the library")
                .arg(Param::text("spec").describe(&format!(
                    "The game JSON. Required: title, arena (size, theme), player (character, speed), collectible (kind, count), hazards[] (kind, speed, count), lives, music. `player.character` may name a saved character or inline a whole one, in which case the character vocabulary applies to it too. The enumerated fields take {}. Ranges are in `describe`'s spec_grammar.",
                    spec::vocabulary_line("game")
                ))),
            handler(ui, &core, "new_game"),
        ),
        (
            Action::new("build", "Compile a saved game into its one HTML file")
                .arg(Param::text("game").describe("Title or slug of a saved game")),
            handler(ui, &core, "build"),
        ),
        (
            Action::new("play", "Open a built game in the desktop Browser")
                .defers()
                .arg(Param::text("game").describe("Title or slug of a built game")),
            handler(ui, &core, "play"),
        ),
        (
            Action::new("verify", "Run the headless gates: boots, clean console, frame renders, input moves, bot wins, bot loses, frame budget")
                .defers()
                .arg(Param::text("game").describe("Title or slug of a built game")),
            handler(ui, &core, "verify"),
        ),
        (
            Action::new("screenshot", "Take a headless PNG of a built game")
                .defers()
                .arg(Param::text("game").describe("Title or slug of a built game")),
            handler(ui, &core, "screenshot"),
        ),
        (
            // Sensitive, like Notes' own `trash` and unlike everything else here: a built game
            // is work somebody asked for, and the Trash is a recovery a person has to know
            // about. Not `dangerous` — that grade is for deleting a path the caller names,
            // which is what Files does; this one can only reach Arcade's own library.
            Action::new("delete", "Move a game to the Trash, where Files can bring it back")
                .risk("sensitive")
                .arg(Param::text("game").describe("Title or slug of a saved game")),
            handler(ui, &core, "delete"),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jobs_start_running_and_land() {
        let core = Core::new();
        let id = core.start_job("verify", "meadow-run");
        assert_eq!(core.running().len(), 1);
        assert_eq!(core.running()[0].action, "verify");
        core.finish_job(id, Ok("passed all 7 gates".into()));
        assert!(core.running().is_empty());
        let json = core.jobs_json();
        assert_eq!(json[0]["status"], "done");
        assert_eq!(json[0]["detail"], "passed all 7 gates");
    }

    #[test]
    fn a_failed_job_says_failed() {
        let core = Core::new();
        let id = core.start_job("play", "x");
        core.finish_job(id, Err("the Browser did not answer".into()));
        let json = core.jobs_json();
        assert_eq!(json[0]["status"], "failed");
        assert_eq!(json[0]["detail"], "the Browser did not answer");
    }

    #[test]
    fn the_job_list_stays_a_glance() {
        let core = Core::new();
        for i in 0..20 {
            let id = core.start_job("verify", &format!("game-{i}"));
            core.finish_job(id, Ok("ok".into()));
        }
        assert!(core.jobs.lock().unwrap().len() <= 8);
    }

    #[test]
    fn game_rows_say_what_exists_on_disk() {
        let unbuilt = GameEntry {
            slug: "a".into(), title: "A".into(), built: false,
            last_build: None, verification: None,
        };
        assert_eq!(detail_line(&unbuilt), "not built");
        let built = GameEntry {
            slug: "a".into(), title: "A".into(), built: true,
            last_build: Some("2026-09-22 10:00".into()),
            verification: Some("verified 2026-09-22 10:05: passed all 7 gates".into()),
        };
        assert_eq!(detail_line(&built), "built 2026-09-22 10:00 · verified 2026-09-22 10:05: passed all 7 gates");
        let stale = GameEntry {
            slug: "a".into(), title: "A".into(), built: true,
            last_build: Some("2026-09-22 10:00".into()), verification: None,
        };
        assert_eq!(detail_line(&stale), "built 2026-09-22 10:00 · not verified");
    }

    #[test]
    fn summary_of_picks_the_human_line() {
        assert_eq!(summary_of(&serde_json::json!({"summary": "built x"})), "built x");
        assert_eq!(summary_of(&serde_json::json!({"built": "built x → /p"})), "built x → /p");
        assert_eq!(summary_of(&serde_json::json!({"started": true})), "started");
    }

    #[test]
    fn a_missing_argument_is_named() {
        let err = arg_str(&serde_json::json!({}), "spec", "the character JSON").unwrap_err();
        assert!(err.contains("`spec`"), "{err}");
        let err = arg_str(&serde_json::json!({"spec": "  "}), "spec", "the character JSON").unwrap_err();
        assert!(err.contains("`spec`"), "{err}");
    }
}
