//! How a Yantrik app is tested without a display (design/app-framework-2026-10-21.md, §9).
//!
//! Slint's platform is process-wide and thread-affine: components must be made on the thread
//! that installed it. Cargo runs tests on many threads, so this crate keeps **one UI thread per
//! test process**, installs the headless platform there once, and runs every [`TestApp`] call
//! on it. A test holds a handle and sends closures across; each call is one step on that
//! thread, so two tests' apps never touch each other's state.
//!
//! Nothing happens on its own: jobs run when the test calls [`TestApp::run_jobs`], timers fire
//! when it calls [`TestApp::advance`], and posts from other threads land on [`TestApp::tick`].
//! A test therefore chooses the order things land in, which is how a race is tested.
//!
//! The Mind's side is tested through the real dispatch: [`TestApp::act`] goes through the same
//! registry the runner serves, with the authority the test pins, and no socket.
//!
//! This is the one headless platform; the six copies in app test files are retired as each app
//! moves onto the framework.

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::Value;
use yantrik_app::app::{App, Boot, Event, ExitReason, WindowId};
use yantrik_app::cx::{Core, Cx, Inbound, Parts};
use yantrik_app::intents::{self, Intents};
use yantrik_app::jobs::{Cancel, Deliver, Executor, Job, JobId, Manual};
use yantrik_app::paths::Paths;
use yantrik_app::persist::{Lane, ManualLane, Report, Write};
use yantrik_app::queue::Post;
use yantrik_app::refused::Refused;
use yantrik_app::timers::{Fire, ManualTimers, Sink, TimerId, Timers};
use yantrik_app::watch::Seen;
use yantrik_app::window::Window;
use yantrik_ipc_transport::gate::{Authority, Mode};
use yantrik_surface::{AgentTokenScope, CallerScope, Later, LaterScope, LocalRegistry};

pub mod headless {
    //! Slint on a software window with no window system.

    use slint::platform::{
        software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
        EventLoopProxy, Platform, WindowAdapter,
    };
    use std::rc::Rc;

    struct Proxy;

    impl EventLoopProxy for Proxy {
        fn quit_event_loop(&self) -> Result<(), slint::EventLoopError> {
            Ok(())
        }

        fn invoke_from_event_loop(
            &self,
            event: Box<dyn FnOnce() + Send>,
        ) -> Result<(), slint::EventLoopError> {
            // Onto the one UI thread, as a step of its own, whichever thread asked: a closure
            // that upgrades a `slint::Weak` must run where the component was made. A panic in
            // it is said and does not take the thread (and every later test) with it.
            let step: super::Step = Box::new(move || {
                if let Err(panic) =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(event))
                {
                    let what = panic
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                        .unwrap_or_else(|| "a non-string payload".into());
                    eprintln!(
                        "yantrik-testing: a closure posted to the UI thread panicked: {what}"
                    );
                }
            });
            super::ui_thread()
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .send(step)
                .map_err(|_| slint::EventLoopError::EventLoopTerminated)
        }
    }

    struct Headless;

    impl Platform for Headless {
        fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
            Ok(MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer))
        }

        fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
            Some(Box::new(Proxy))
        }
    }

    /// Installs the platform on the calling thread. Once per process; a second call is nothing.
    pub fn install() {
        // Err means a platform is already set: fine.
        let _ = slint::platform::set_platform(Box::new(Headless));
    }
}

// ── The one UI thread ───────────────────────────────────────────────

type Step = Box<dyn FnOnce() + Send>;

static UI_THREAD: OnceLock<Mutex<mpsc::Sender<Step>>> = OnceLock::new();
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

thread_local! {
    /// Every app under test, by id, on the UI thread. `Rc<dyn Any>` holds an `Rc<Under<A>>`.
    static APPS: RefCell<HashMap<u64, Rc<dyn Any>>> = RefCell::new(HashMap::new());
}

fn ui_thread() -> &'static Mutex<mpsc::Sender<Step>> {
    UI_THREAD.get_or_init(|| {
        let (sender, receiver) = mpsc::channel::<Step>();
        std::thread::Builder::new()
            .name("yantrik-testing-ui".into())
            .spawn(move || {
                headless::install();
                while let Ok(step) = receiver.recv() {
                    step();
                }
            })
            .expect("the test UI thread");
        Mutex::new(sender)
    })
}

/// Runs `f` on the UI thread and waits for its answer. A panic there is a panic here, with its
/// message.
fn on_ui<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> R {
    let (reply, answer) = mpsc::channel();
    let step: Step = Box::new(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        let _ = reply.send(result);
    });
    ui_thread()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .send(step)
        .expect("the test UI thread is running");
    match answer.recv().expect("the test UI thread answered") {
        Ok(r) => r,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

// ── What the core is given ──────────────────────────────────────────

/// The manual executor, shared between the core and the test (both on the UI thread).
#[derive(Clone)]
struct SharedJobs<C: Send + 'static>(Arc<Mutex<Manual<C>>>);

impl<C: Send + 'static> Executor<C> for SharedJobs<C> {
    fn spawn(&mut self, id: JobId, cancel: Cancel, job: Job<C>, deliver: Arc<dyn Deliver<C>>) {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .spawn(id, cancel, job, deliver);
    }

    fn running(&self) -> usize {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).running()
    }

    fn drain(&self, grace: Duration) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .drain(grace)
    }
}

/// The virtual clock, shared between the core and the test.
type Clock<C> = Rc<RefCell<ManualTimers<C>>>;

struct SharedTimers<C: 'static>(Clock<C>);

impl<C: 'static> Timers<C> for SharedTimers<C> {
    fn every(&mut self, id: TimerId, period: Duration, fire: Fire<C>) {
        self.0.borrow_mut().every(id, period, fire);
    }

    fn after(&mut self, id: TimerId, delay: Duration, fire: Fire<C>) {
        self.0.borrow_mut().after(id, delay, fire);
    }

    fn stop(&mut self, id: TimerId) {
        self.0.borrow_mut().stop(id);
    }

    fn stop_all(&mut self) {
        self.0.borrow_mut().stop_all();
    }

    fn armed(&self) -> usize {
        self.0.borrow().armed()
    }
}

/// The manual persistence lane, shared between the core and the test.
#[derive(Clone)]
struct SharedLane(Arc<ManualLane>);

impl Lane for SharedLane {
    fn write(&mut self, write: Write) {
        // `ManualLane::write` takes `&mut`, but its queue is behind a lock: the shared handle
        // may push through it.
        self.0.push(write);
    }

    fn waiting(&self) -> usize {
        self.0.waiting()
    }

    fn drain(&self, grace: Duration) -> bool {
        self.0.drain(grace)
    }
}

/// What lives on the UI thread for one app under test.
struct Under<A: App> {
    core: Rc<Core<A>>,
    jobs: SharedJobs<A::Command>,
    clock: Clock<A::Command>,
    lane: Arc<ManualLane>,
    paths: Paths,
    registry: Rc<LocalRegistry>,
}

// ── Acting as the Mind ──────────────────────────────────────────────

/// One call of an intent, as the test wants it dispatched. By default the person's own, in a
/// mode that asks about nothing, with no revision expected.
#[derive(Debug, Clone)]
pub struct Call {
    pub name: String,
    pub args: Value,
    pub agent_token: Option<String>,
    pub mode: String,
    pub granted: bool,
    pub expect_revision: Option<String>,
}

impl Call {
    pub fn new(name: &str, args: Value) -> Call {
        Call {
            name: name.into(),
            args,
            agent_token: None,
            mode: "bypass_all".into(),
            granted: false,
            expect_revision: None,
        }
    }

    /// The call arrives as an agent's, with this token.
    pub fn as_agent(mut self, token: &str) -> Call {
        self.agent_token = Some(token.into());
        self
    }

    /// The machine's mode (`ask`, `auto`, `bypass`, `bypass_all`).
    pub fn in_mode(mut self, mode: &str) -> Call {
        self.mode = mode.into();
        self
    }

    /// A person's Allow was spent for exactly this call.
    pub fn granted(mut self) -> Call {
        self.granted = true;
        self
    }

    /// The caller acted on this revision of the app; a different one is refused as stale.
    pub fn expecting(mut self, revision: &str) -> Call {
        self.expect_revision = Some(revision.into());
        self
    }
}

/// An accepted call whose answer may still be owed (`Settle::Explicit`): the rest of the answer
/// is run by [`Pending::settle`] on the test's thread, as the RPC thread would, while the test
/// runs the jobs on the UI thread.
pub struct Pending {
    envelope: Value,
    later: Option<Later>,
}

impl std::fmt::Debug for Pending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pending")
            .field("envelope", &self.envelope)
            .field("owed", &self.later.is_some())
            .finish()
    }
}

impl Pending {
    /// The envelope as the dispatch returned it. For an explicit settlement its `result` is a
    /// placeholder until [`Pending::settle`] runs.
    pub fn envelope(&self) -> &Value {
        &self.envelope
    }

    /// Whether the answer is still owed.
    pub fn is_owed(&self) -> bool {
        self.later.is_some()
    }

    /// Waits for the outcome (the deadline is the intent's) and returns the settled result.
    pub fn settle(self) -> Result<Value, String> {
        match self.later {
            Some(work) => work(),
            None => Ok(self.envelope.get("result").cloned().unwrap_or(Value::Null)),
        }
    }
}

// ── The handle ──────────────────────────────────────────────────────

/// An app under test: the runner's lifecycle with nothing happening on its own. Every method
/// runs on the test process's one UI thread and waits for its answer.
pub struct TestApp<A: App> {
    id: u64,
    _app: std::marker::PhantomData<fn() -> A>,
}

impl<A: App> TestApp<A> {
    /// Boots the app as the runner would, with its files at `paths` and `args` on its command
    /// line: the settings opened, the model from the boot, the window headless, `wire`, the
    /// first render, `start`, the intents in a registry (not served). Effects `start` asked for
    /// are staged: jobs wait for [`TestApp::run_jobs`], timers for [`TestApp::advance`], writes
    /// for [`TestApp::run_writes`]. Starting twice at the same `paths` is a restart.
    pub fn start_at(paths: Paths, args: Vec<std::ffi::OsString>) -> TestApp<A> {
        TestApp::start_at_with(
            paths,
            args,
            Arc::new(yantrik_mind::fake::FakeMind::new()),
            yantrik_app::services::Services::fakes(),
        )
    }

    /// [`TestApp::start_at`] with the Mind and the services the app is given: fakes the test
    /// keeps handles to, so it can set what they answer and read what they were asked.
    pub fn start_at_with(
        paths: Paths,
        args: Vec<std::ffi::OsString>,
        mind: Arc<dyn yantrik_mind::Mind>,
        services: yantrik_app::services::Services,
    ) -> TestApp<A> {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        on_ui(move || {
            let (boot, store) = Boot::<A::Settings>::open(paths.clone(), args, mind, services)
                .unwrap_or_else(|e| panic!("{}: the launch was refused: {e}", A::ID));
            let model = A::model(&boot)
                .unwrap_or_else(|e| panic!("{}: the model did not start: {e}", A::ID));
            let ui = A::Ui::make().unwrap_or_else(|e| panic!("{}: no headless window: {e}", A::ID));
            let jobs = SharedJobs(Arc::new(Mutex::new(Manual::new())));
            let made: Rc<RefCell<Option<Clock<A::Command>>>> = Rc::new(RefCell::new(None));
            let made_out = made.clone();
            let lane_made: Arc<Mutex<Option<Arc<ManualLane>>>> = Arc::new(Mutex::new(None));
            let lane_out = lane_made.clone();
            let core = Core::<A>::new(
                model,
                ui,
                Parts {
                    executor: Box::new(jobs.clone()),
                    timers: Box::new(move |sink: Sink<A::Command>| {
                        let timers = Rc::new(RefCell::new(ManualTimers::new(sink)));
                        *made_out.borrow_mut() = Some(timers.clone());
                        Box::new(SharedTimers(timers))
                    }),
                    lane: Box::new(move |report: Arc<dyn Report>| {
                        let lane = Arc::new(ManualLane::new(report));
                        *lane_out.lock().unwrap_or_else(|p| p.into_inner()) = Some(lane.clone());
                        Box::new(SharedLane(lane))
                    }),
                    settings: store,
                    paths: paths.clone(),
                    wake: Arc::new(|| {}),
                },
            );
            let clock = made.borrow().clone().expect("the timers were made");
            let lane = lane_made
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone()
                .expect("the lane was made");
            // Hermetic: a window opened in a test wears the default theme, not this machine's.
            core.set_theme(yantrik_app::theme::ThemeSettings::default());
            core.flush_settings();
            A::wire(&Cx::new(core.clone()));
            core.render_all();
            core.start();
            // As the runner does: the launch, once, after `start`.
            core.event(Event::Launch(boot.launch.clone()));
            let mut table = Intents::new();
            A::intents(&mut table);
            let registry = Rc::new(intents::registry(&core, table));
            core.attach_registry(registry.clone());
            let under: Rc<dyn Any> = Rc::new(Under {
                core,
                jobs,
                clock,
                lane,
                paths,
                registry,
            });
            APPS.with(|apps| apps.borrow_mut().insert(id, under));
        });
        TestApp {
            id,
            _app: std::marker::PhantomData,
        }
    }

    /// Boots the app with files of its own under the temporary directory and no arguments.
    pub fn start() -> TestApp<A> {
        TestApp::start_at(Paths::temporary(A::ID), Vec::new())
    }

    /// Where this app's files are.
    pub fn paths(&self) -> Paths {
        self.with(|u| u.paths.clone())
    }

    /// One step on the UI thread, with this app's core.
    fn with<R: Send + 'static>(&self, f: impl FnOnce(&Under<A>) -> R + Send + 'static) -> R {
        let id = self.id;
        on_ui(move || {
            APPS.with(|apps| {
                let under = apps
                    .borrow()
                    .get(&id)
                    .cloned()
                    .expect("the app under test is still here");
                let under = under
                    .downcast::<Under<A>>()
                    .expect("the app under test is of this type");
                f(&under)
            })
        })
    }

    /// Applies a command now, as a button would.
    pub fn run(&self, command: A::Command) {
        self.with(move |u| u.core.run(command));
    }

    /// Takes everything posted from other threads (a job's result, an event) and applies it.
    pub fn tick(&self) {
        self.with(|u| u.core.drain());
    }

    /// Runs every waiting job, in order, and applies what they returned.
    pub fn run_jobs(&self) {
        self.with(|u| {
            u.jobs.0.lock().unwrap_or_else(|p| p.into_inner()).run_all();
            u.core.drain();
        });
    }

    /// Runs the oldest waiting job only, and applies what it returned.
    pub fn run_one_job(&self) -> bool {
        self.with(|u| {
            let ran = u.jobs.0.lock().unwrap_or_else(|p| p.into_inner()).run_one();
            u.core.drain();
            ran
        })
    }

    /// Moves the virtual clock, firing every timer that comes due. Each timer's command runs
    /// with the clock released, so a command that arms or stops a timer is fine.
    pub fn advance(&self, by: Duration) {
        self.with(move |u| {
            let until = u.clock.borrow().now() + by;
            loop {
                let fired = u.clock.borrow_mut().fire_next(until);
                match fired {
                    Some(command) => u.core.run(command),
                    None => break,
                }
            }
            u.clock.borrow_mut().settle(until);
            u.core.drain();
        });
    }

    /// Does every write waiting in the persistence lane, in order, and applies what came of
    /// them (`Event::SettingsSaved`, or not).
    pub fn run_writes(&self) {
        self.with(|u| {
            u.lane.write_all();
            u.core.drain();
        });
    }

    pub fn writes_waiting(&self) -> usize {
        self.with(|u| u.core.writes_waiting())
    }

    /// A framework event, raised by the test as the runner would raise it (a launch handed over,
    /// a close request): the app's `event` answers it, or the default happens.
    pub fn event(&self, event: Event<A::Settings>) {
        self.with(move |u| u.core.event(event));
    }

    /// What the housekeeping thread would have seen, raised by the test: the settings file
    /// edited outside the app, or the shell's theme changed.
    pub fn seen(&self, seen: Seen) {
        self.with(move |u| {
            let _ = u.core.post().post(Inbound::Seen(seen));
            u.core.drain();
        });
    }

    /// The windows open, the first one first.
    pub fn windows(&self) -> Vec<WindowId> {
        self.with(|u| u.core.windows_open())
    }

    /// Reads an open window (`None` once it is closed), as `ui` reads the first.
    pub fn window<R: Send + 'static>(
        &self,
        id: WindowId,
        f: impl FnOnce(&A::Ui) -> R + Send + 'static,
    ) -> Option<R> {
        self.with(move |u| u.core.window(id).map(|ui| f(&ui)))
    }

    /// A second launch asked for the app (`present`).
    pub fn present(&self) -> Result<(), String> {
        self.with(|u| u.core.present())
    }

    /// The person pressed a window's close button: `Event::CloseRequested` for it.
    pub fn close_requested(&self, id: WindowId) {
        self.event(Event::CloseRequested(id));
    }

    /// The documents' files the app watches (`fx.watch`), by key.
    pub fn watched(&self) -> Vec<(String, std::path::PathBuf)> {
        self.with(|u| u.core.watched().entries())
    }

    /// One tick of the watch over the documents' files: each real file is looked at, and what
    /// changed reaches the app as `Event::FileChanged`, as the housekeeping thread would post it.
    pub fn look_at_files(&self) {
        let changed = self.with(|u| u.core.watched()).poll();
        for (key, seen, arming) in changed {
            self.seen(Seen::File { key, seen, arming });
        }
    }

    /// The settings as committed to disk, by the store's account.
    pub fn committed_settings(&self) -> A::Settings {
        self.with(|u| u.core.committed_settings())
    }

    pub fn settings_pending(&self) -> bool {
        self.with(|u| u.core.settings_pending())
    }

    /// Commands queued behind the one being applied (`then`), not yet applied.
    pub fn pending_commands(&self) -> usize {
        self.with(|u| u.core.pending_commands())
    }

    pub fn model<R: Send + 'static>(&self, f: impl FnOnce(&A::Model) -> R + Send + 'static) -> R {
        self.with(move |u| u.core.read(f))
    }

    pub fn ui<R: Send + 'static>(&self, f: impl FnOnce(&A::Ui) -> R + Send + 'static) -> R {
        self.with(move |u| f(u.core.ui()))
    }

    pub fn last_refusal(&self) -> Option<Refused> {
        self.with(|u| u.core.last_refusal())
    }

    pub fn exit_reason(&self) -> Option<ExitReason> {
        self.with(|u| u.core.exit_reason())
    }

    pub fn jobs_waiting(&self) -> usize {
        self.with(|u| u.core.jobs_waiting())
    }

    pub fn timers_armed(&self) -> usize {
        self.with(|u| u.core.timers_armed())
    }

    pub fn settlements_open(&self) -> usize {
        self.with(|u| u.core.settlements_open())
    }

    /// The `Send` endpoint another thread posts to; what it posts lands on [`TestApp::tick`].
    pub fn post(&self) -> Post<Inbound<A>> {
        self.with(|u| u.core.post())
    }

    /// What the Mind reads: the real `describe`, through the registry.
    pub fn describe(&self) -> Value {
        self.with(|u| u.registry.describe())
    }

    /// The app's current revision, as `describe` reports it.
    pub fn revision(&self) -> String {
        self.describe()
            .get("revision")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// The person's own call of an intent, through the real dispatch (the argument checks, the
    /// ceiling and mode, the revision guard), in a mode that asks about nothing.
    pub fn act(&self, name: &str, args: Value) -> Result<Pending, String> {
        self.act_as(Call::new(name, args))
    }

    /// A call as the test describes it.
    pub fn act_as(&self, call: Call) -> Result<Pending, String> {
        self.with(move |u| {
            let _agent = AgentTokenScope::enter(call.agent_token.clone());
            let _who = CallerScope::enter(None);
            let later = LaterScope::enter();
            let authority = Authority {
                ceiling: "dangerous".into(),
                mode: Mode::named(&call.mode),
                granted: call.granted,
                asks_above: None,
            };
            let envelope = u.registry.act(
                &call.name,
                &call.args,
                call.expect_revision.as_deref(),
                "test-call",
                &authority,
            )?;
            Ok(Pending {
                envelope,
                later: later.take(),
            })
        })
    }

    /// The end, as the runner does it.
    pub fn shutdown(&self) -> bool {
        self.with(|u| u.core.shutdown(Duration::from_millis(100)))
    }
}

impl<A: App> Drop for TestApp<A> {
    fn drop(&mut self) {
        // The window and the core are dropped where they were made.
        let id = self.id;
        let step: Step = Box::new(move || {
            APPS.with(|apps| apps.borrow_mut().remove(&id));
        });
        if let Some(sender) = UI_THREAD.get() {
            let _ = sender.lock().unwrap_or_else(|p| p.into_inner()).send(step);
        }
    }
}
