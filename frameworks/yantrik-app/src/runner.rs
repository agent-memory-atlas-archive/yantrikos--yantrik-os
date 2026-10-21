//! The runner: the lifecycle every app gets, in a fixed order
//! (design/app-framework-2026-10-21.md, §2.2). An app's `main` is
//! `yantrik_app::run::<MyApp>()` and nothing else.

use std::process::ExitCode;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use crate::app::{App, Boot, Event, WindowId};
use crate::cx::{Core, Cx, Inbound, Parts};
use crate::instance::Held;
use crate::intents::{self, Intents};
use crate::jobs::Threads;
use crate::paths::Paths;
use crate::persist::Threaded;
use crate::present::Presented;
use crate::timers::SlintTimers;
use crate::watch::Watch;
use crate::window::Window;
use crate::{boot, instance, launch, problems, theme};

/// How long shutdown waits for the job pool before exiting without it.
pub const GRACE: Duration = Duration::from_secs(2);
/// The job pool: threads running at once, and jobs that may wait for one.
const JOB_THREADS: usize = 4;
const JOBS_WAITING: usize = 64;

thread_local! {
    /// How the wake reaches the core: a closure installed on the UI thread, so the wake itself
    /// captures nothing and is `Send`.
    static DRAIN: std::cell::RefCell<Option<Box<dyn Fn()>>> = const { std::cell::RefCell::new(None) };
}

pub(crate) fn drain_on_ui_thread() {
    DRAIN.with(|cell| {
        if let Some(drain) = cell.borrow().as_ref() {
            drain();
        }
    });
}

/// Runs the app to its end. The exit code says how it ended: 0 for a close or a display that
/// went away, 1 for a failure, with a problem record.
pub fn run<A: App>() -> ExitCode {
    boot::init_tracing(A::ID);
    boot::choose_backend();
    let paths = Paths::for_app(A::ID);
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    // The launch is parsed before the claim: a command line this app does not take is said and
    // refused here, whichever copy would have run it. Opening the settings and drafts writes
    // nothing, so a copy that hands over and exits leaves no trace.
    let (mut boot, store) = match Boot::<A::Settings>::open(paths.clone(), args, yantrik_mind::live()) {
        Ok(opened) => opened,
        Err(e) => return startup_failed(A::ID, "launch", &e.sentence),
    };
    // The intents are declared, not read from the model, so a deep link's intent is admitted
    // before the claim: a link this app cannot take is refused here, bothering no running copy.
    let mut table = Intents::new();
    A::intents(&mut table);
    if let Some(intent) = boot.launch.intent.take() {
        match intents::admit(&table.specs(), intent, false) {
            Ok(admitted) => boot.launch.intent = Some(admitted),
            Err(why) => return startup_failed(A::ID, "launch", &why),
        }
    }

    // One instance per app and profile: a second launch hands what it was asked to open to the
    // running copy, which shows its window, and exits.
    let instance = paths.instance_name();
    // What the running copy did not answer, kept for the end: a copy that is alive and silent
    // twice has lost the launch, and the person hears so rather than nothing.
    let unanswered = std::cell::RefCell::new(None::<String>);
    let claimed = instance::claim_or_hand_over(&instance, |_| {
        match launch::hand_over(&instance, &boot.launch) {
            Presented::Unanswered(why) => {
                tracing::warn!(app = A::ID, "the running copy did not answer the hand-over: {why}");
                *unanswered.borrow_mut() = Presented::Unanswered(why).sentence(A::ID);
                Held::OwnerGone
            }
            answered => {
                match answered.sentence(A::ID) {
                    None => tracing::info!(app = A::ID, "Already running; handed the launch to it"),
                    Some(said) => {
                        tracing::warn!(app = A::ID, answer = ?answered, "{said}");
                        eprintln!("{said}");
                    }
                }
                *unanswered.borrow_mut() = None;
                Held::HandedOver
            }
        }
    });
    let Some(_instance) = claimed else {
        return match unanswered.into_inner() {
            Some(said) => startup_failed(A::ID, "launch", &said),
            None => ExitCode::SUCCESS,
        };
    };

    if let crate::settings::Health::Recovery(e) = &boot.settings_health {
        tracing::warn!(app = A::ID, "{e}; running on the defaults");
        problems::record_failure(A::ID, "settings", &e.to_string());
    }
    let model = match A::model(&boot) {
        Ok(model) => model,
        Err(e) => return startup_failed(A::ID, "model", &e.sentence),
    };
    let ui = match A::Ui::make() {
        Ok(ui) => ui,
        Err(e) => return startup_failed(A::ID, "window", &e.to_string()),
    };
    let theme_now = theme::load();
    ui.apply_theme(&theme_now);
    ui.connect_chrome();

    let wake: crate::queue::Wake = Arc::new(|| {
        let _ = slint::invoke_from_event_loop(drain_on_ui_thread);
    });
    let core = Core::<A>::new(
        model,
        ui,
        Parts {
            executor: Box::new(Threads::new(A::ID, JOB_THREADS, JOBS_WAITING)),
            timers: Box::new(|sink| Box::new(SlintTimers::new(sink))),
            lane: Box::new(|report| Box::new(Threaded::new(A::ID, report))),
            settings: store,
            paths: boot.paths.clone(),
            wake,
        },
    );
    install(&core);
    core.set_theme(theme_now);
    // A file of an older schema is rewritten at this one, once.
    core.flush_settings();
    // The settings file (an outside edit), the shell's theme and the documents' files, watched
    // until the end.
    let _watch = Watch::start(
        A::ID,
        boot.paths.config_file().to_path_buf(),
        theme_now,
        core.watched(),
        core.notice(),
    );

    let cx = Cx::new(core.clone());
    A::wire(&cx);
    core.render_all();
    core.start();
    // The launch is applied once, here (§2.2 step 11): the files to open, or the deep link.
    core.event(Event::Launch(boot.launch.clone()));

    // The surface: the app's intents, `present` for the desktop's own handover, and `launch`
    // for a second copy's. Served only now that commands can be applied, on the instance's own
    // socket (`app-<id>[-<profile>].sock`).
    let registry = intents::registry(&core, table);
    let registry = crate::control::App::from_registry(registry).serve_as(&instance);
    core.attach_registry(registry);

    let ending = crate::event_loop::run_until_ending(core.ui(), A::ID);
    core.shutdown(GRACE);
    DRAIN.with(|cell| *cell.borrow_mut() = None);
    // A close, an `fx.exit()` and a logout are all 0; only a loop that failed with the display
    // still there is 1, and that one has its problem record.
    match ending {
        crate::event_loop::Ending::Failure if core.exit_reason().is_none() => ExitCode::from(1),
        _ => ExitCode::SUCCESS,
    }
}

/// The drain and the window's close handler, on the UI thread.
fn install<A: App>(core: &Rc<Core<A>>) {
    let for_drain = core.clone();
    DRAIN.with(|cell| *cell.borrow_mut() = Some(Box::new(move || for_drain.drain())));
    let post = core.post();
    use slint::ComponentHandle as _;
    core.ui().window().on_close_requested(move || {
        // The app decides (`App::event`): a close now, or a flow that ends in `fx.close`.
        let _ = post.post(Inbound::Event(Event::CloseRequested(WindowId::PRIMARY)));
        drain_on_ui_thread();
        slint::CloseRequestResponse::KeepWindowShown
    });
}

fn startup_failed(id: &str, what: &str, sentence: &str) -> ExitCode {
    eprintln!("{id}: {sentence}");
    tracing::error!(app = id, what, "{sentence}");
    problems::record_failure(id, what, sentence);
    ExitCode::from(1)
}
