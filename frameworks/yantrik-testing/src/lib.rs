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
use yantrik_app::app::{App, Boot, ExitReason};
use yantrik_app::cx::{Core, Cx, Inbound};
use yantrik_app::intents::{self, Intents};
use yantrik_app::jobs::{Cancel, Deliver, Executor, Job, JobId, Manual};
use yantrik_app::queue::Post;
use yantrik_app::refused::Refused;
use yantrik_app::timers::{Fire, ManualTimers, Sink, TimerId, Timers};
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
            // that upgrades a `slint::Weak` must run where the component was made.
            super::ui_thread()
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .send(event)
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

/// What lives on the UI thread for one app under test.
struct Under<A: App> {
    core: Rc<Core<A>>,
    jobs: SharedJobs<A::Command>,
    clock: Clock<A::Command>,
    registry: LocalRegistry,
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
    /// Boots the app as the runner would: the model from `boot`, the window headless, `wire`,
    /// the first render, `start`, the intents in a registry (not served). Effects `start` asked
    /// for are staged: jobs wait for [`TestApp::run_jobs`], timers for [`TestApp::advance`].
    pub fn start_with(boot: Boot) -> TestApp<A> {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        on_ui(move || {
            let model = A::model(&boot)
                .unwrap_or_else(|e| panic!("{}: the model did not start: {e}", A::ID));
            let ui = A::Ui::make().unwrap_or_else(|e| panic!("{}: no headless window: {e}", A::ID));
            let jobs = SharedJobs(Arc::new(Mutex::new(Manual::new())));
            let made: Rc<RefCell<Option<Clock<A::Command>>>> = Rc::new(RefCell::new(None));
            let made_out = made.clone();
            let core = Core::<A>::new(
                model,
                ui,
                Box::new(jobs.clone()),
                move |sink: Sink<A::Command>| {
                    let timers = Rc::new(RefCell::new(ManualTimers::new(sink)));
                    *made_out.borrow_mut() = Some(timers.clone());
                    Box::new(SharedTimers(timers))
                },
                Arc::new(|| {}),
            );
            let clock = made.borrow().clone().expect("the timers were made");
            A::wire(&Cx::new(core.clone()));
            core.render_all();
            core.start();
            let mut table = Intents::new();
            A::intents(&mut table);
            let registry = intents::registry(&core, table);
            let under: Rc<dyn Any> = Rc::new(Under {
                core,
                jobs,
                clock,
                registry,
            });
            APPS.with(|apps| apps.borrow_mut().insert(id, under));
        });
        TestApp {
            id,
            _app: std::marker::PhantomData,
        }
    }

    pub fn start() -> TestApp<A> {
        TestApp::start_with(Boot::default())
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
