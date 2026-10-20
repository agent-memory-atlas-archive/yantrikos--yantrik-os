//! The runner: the lifecycle every app gets, in a fixed order
//! (design/app-framework-2026-10-21.md, §2.2). An app's `main` is
//! `yantrik_app::run::<MyApp>()` and nothing else.

use std::process::ExitCode;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use crate::app::{App, Boot, Event, WindowId};
use crate::cx::{Core, Cx, Inbound};
use crate::intents::{self, Intents};
use crate::jobs::Threads;
use crate::timers::SlintTimers;
use crate::window::Window;
use crate::{boot, instance, problems, theme};

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

fn drain_on_ui_thread() {
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
    let boot = Boot {
        args: std::env::args_os().skip(1).collect(),
    };

    // One instance per app: a second launch asks the running copy to come forward, and exits.
    let Some(_instance) = instance::claim(A::ID) else {
        return ExitCode::SUCCESS;
    };

    let model = match A::model(&boot) {
        Ok(model) => model,
        Err(e) => return startup_failed(A::ID, "model", &e.sentence),
    };
    let ui = match A::Ui::make() {
        Ok(ui) => ui,
        Err(e) => return startup_failed(A::ID, "window", &e.to_string()),
    };
    ui.apply_theme(&theme::load());
    ui.connect_chrome();

    let wake: crate::queue::Wake = Arc::new(|| {
        let _ = slint::invoke_from_event_loop(drain_on_ui_thread);
    });
    let core = Core::<A>::new(
        model,
        ui,
        Box::new(Threads::new(A::ID, JOB_THREADS, JOBS_WAITING)),
        |sink| Box::new(SlintTimers::new(sink)),
        wake,
    );
    install(&core);

    let cx = Cx::new(core.clone());
    A::wire(&cx);
    core.render_all();
    core.start();

    // The surface: the app's intents, and `present` for the desktop's own handover. Served only
    // now that commands can be applied.
    let mut table = Intents::new();
    A::intents(&mut table);
    let registry = intents::registry(&core, table);
    crate::control::App::from_registry(registry).serve();

    let closed = crate::run_until_closed(core.ui(), A::ID);
    core.shutdown(GRACE);
    DRAIN.with(|cell| *cell.borrow_mut() = None);
    if closed || core.exit_reason().is_some() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
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
