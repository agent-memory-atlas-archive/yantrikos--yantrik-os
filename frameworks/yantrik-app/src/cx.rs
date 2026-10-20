//! The UI-thread core: the one place the model lives, and how commands reach it
//! (design/app-framework-2026-10-21.md, §§2.3, 3, 4).
//!
//! [`Core`] owns the model, the window, the timers and the job table. It applies a command by
//! calling `update`, committing the effects it asked for, and calling `render` for the parts
//! that changed. Nothing else touches the model. [`Cx`] is the handle a view uses to bind
//! callbacks, and [`Core::post`] is the `Send` endpoint every other thread uses.
//!
//! Re-entrancy: a command raised while one is being applied (from `then`, or a Slint callback
//! fired by `render`) is queued and applied after the current one's render, never nested. One
//! drain applies at most [`PENDING_PER_DRAIN`] queued commands before yielding to the event
//! loop, so a `then` cycle cannot freeze drawing.
//!
//! Capacity: commands share the bounded inbox and a flood of them is refused; a job's outcome
//! does not. It goes through the inbox's reserved lane, bounded by the pool itself, so saturation
//! never loses a result and never leaves a key marked running (§4).

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::ops::Deref;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use slint::ComponentHandle as _;

use crate::app::{App, Changed, Event, ExitReason, WindowId};
use crate::effects::{Effects, Request};
use crate::jobs::{Cancel, Concurrency, Deliver, Executor, Finished, JobId, JobKey};
use crate::problems;
use crate::queue::{self, Inbox, Post, Wake};
use crate::refused::Refused;
use crate::timers::Timers;

/// How many queued (`then`) commands one drain applies before the rest wait for the next wake.
pub const PENDING_PER_DRAIN: usize = queue::CAPACITY;

/// What reaches the core from any thread.
pub enum Inbound<A: App> {
    Command(A::Command),
    Finished(Finished<A::Command>),
    Event(Event),
    /// The rest of a drain that reached [`PENDING_PER_DRAIN`]: posted by the core to itself.
    Continue,
}

/// A job the core is waiting on.
struct Waiting<C> {
    cancel: Cancel,
    key: Option<JobKey>,
    /// Applied if the job ends without a result (a panic, a refusal), so the app hears of it.
    failed: Option<C>,
}

/// The core, on the UI thread. One per app process; a test makes its own.
pub struct Core<A: App> {
    id: &'static str,
    model: RefCell<A::Model>,
    ui: A::Ui,
    timers: RefCell<Box<dyn Timers<A::Command>>>,
    executor: RefCell<Box<dyn Executor<A::Command>>>,
    jobs: RefCell<HashMap<JobId, Waiting<A::Command>>>,
    by_key: RefCell<HashMap<JobKey, JobId>>,
    next_job: Cell<u64>,
    post: Post<Inbound<A>>,
    inbox: Inbox<Inbound<A>>,
    pending: RefCell<VecDeque<A::Command>>,
    applying: Cell<bool>,
    last_refusal: RefCell<Option<Refused>>,
    exit: Cell<Option<ExitReason>>,
    window_shown: Cell<bool>,
}

/// Delivers a finished job to the core's queue.
struct PostDeliver<A: App>(Post<Inbound<A>>);

impl<A: App> Deliver<A::Command> for PostDeliver<A> {
    fn deliver(&self, finished: Finished<A::Command>) {
        // The reserved lane: a full inbox of commands cannot lose a result. A late result after
        // shutdown is dropped, as the design says it must be.
        let _ = self.0.post_control(Inbound::Finished(finished));
    }
}

impl<A: App> Core<A> {
    /// A core over `model` and `ui`, with the executor and timers it is given. `wake` is how a
    /// post from another thread gets the UI thread to call [`Core::drain`].
    pub fn new(
        model: A::Model,
        ui: A::Ui,
        executor: Box<dyn Executor<A::Command>>,
        timers: impl FnOnce(crate::timers::Sink<A::Command>) -> Box<dyn Timers<A::Command>>,
        wake: Wake,
    ) -> Rc<Core<A>> {
        let (post, inbox) = queue::bounded(queue::CAPACITY, wake);
        Rc::new_cyclic(|weak: &Weak<Core<A>>| {
            let sink_core = weak.clone();
            let sink: crate::timers::Sink<A::Command> = Rc::new(move |command| {
                if let Some(core) = sink_core.upgrade() {
                    core.run(command);
                }
            });
            Core {
                id: A::ID,
                model: RefCell::new(model),
                ui,
                timers: RefCell::new(timers(sink)),
                executor: RefCell::new(executor),
                jobs: RefCell::new(HashMap::new()),
                by_key: RefCell::new(HashMap::new()),
                next_job: Cell::new(1),
                post,
                inbox,
                pending: RefCell::new(VecDeque::new()),
                applying: Cell::new(false),
                last_refusal: RefCell::new(None),
                exit: Cell::new(None),
                window_shown: Cell::new(true),
            }
        })
    }

    pub fn id(&self) -> &'static str {
        self.id
    }

    pub fn ui(&self) -> &A::Ui {
        &self.ui
    }

    /// The `Send` endpoint for other threads.
    pub fn post(&self) -> Post<Inbound<A>> {
        self.post.clone()
    }

    /// Read the model.
    pub fn read<R>(&self, f: impl FnOnce(&A::Model) -> R) -> R {
        f(&self.model.borrow())
    }

    /// The last command's refusal, if it was refused.
    pub fn last_refusal(&self) -> Option<Refused> {
        self.last_refusal.borrow().clone()
    }

    /// Why the process is ending, once something asked it to.
    pub fn exit_reason(&self) -> Option<ExitReason> {
        self.exit.get()
    }

    /// Draw everything: the first render. A callback a binding fires during it is queued, as
    /// one fired during any other render is.
    pub fn render_all(&self) {
        self.applying.set(true);
        A::render(
            &self.ui,
            WindowId::PRIMARY,
            &self.model.borrow(),
            &Changed::all(),
        );
        self.applying.set(false);
        self.drain_pending();
    }

    /// `App::start`, and its effects. `start` cannot change the model (it reads it), so nothing
    /// is redrawn unless it says so; the first render has just happened.
    pub fn start(&self) {
        let mut fx = Effects::starting(self.next_job.get());
        A::start(&self.model.borrow(), &mut fx);
        let (requests, changed, next) = fx.into_parts();
        self.next_job.set(next);
        self.applying.set(true);
        self.commit(requests);
        if !changed.is_empty() {
            A::render(&self.ui, WindowId::PRIMARY, &self.model.borrow(), &changed);
        }
        self.applying.set(false);
        self.drain_pending();
    }

    /// Apply a command now (on the UI thread), or queue it if one is being applied.
    pub fn run(&self, command: A::Command) {
        if self.applying.get() {
            self.pending.borrow_mut().push_back(command);
            return;
        }
        self.apply(command);
        self.drain_pending();
    }

    /// Applies what is queued, up to [`PENDING_PER_DRAIN`]; the rest wait for the next wake, so
    /// the event loop draws between floods.
    fn drain_pending(&self) {
        for _ in 0..PENDING_PER_DRAIN {
            let next = self.pending.borrow_mut().pop_front();
            let Some(command) = next else { return };
            self.apply(command);
        }
        if !self.pending.borrow().is_empty() {
            tracing::warn!(
                app = self.id,
                "{PENDING_PER_DRAIN} queued commands applied in one drain; the rest wait for the next wake"
            );
            let _ = self.post.post_control(Inbound::Continue);
        }
    }

    /// Commands queued and not yet applied.
    pub fn pending_commands(&self) -> usize {
        self.pending.borrow().len()
    }

    /// One command: update, commit its effects if it was accepted, render what changed.
    fn apply(&self, command: A::Command) {
        self.applying.set(true);
        let mut fx = Effects::new(self.next_job.get());
        let result = A::update(&mut self.model.borrow_mut(), command, &mut fx);
        let (requests, changed, next) = fx.into_parts();
        match result {
            Ok(()) => {
                self.next_job.set(next);
                *self.last_refusal.borrow_mut() = None;
                self.commit(requests);
                if !changed.is_empty() {
                    A::render(&self.ui, WindowId::PRIMARY, &self.model.borrow(), &changed);
                }
            }
            Err(refused) => {
                // Nothing it asked for runs; the sentence is the person's and the Mind's.
                tracing::warn!(app = self.id, kind = refused.kind, "{}", refused.sentence);
                if refused.is_internal() {
                    problems::record_failure(self.id, refused.kind, &refused.sentence);
                }
                *self.last_refusal.borrow_mut() = Some(refused);
            }
        }
        self.applying.set(false);
    }

    /// The effects of an accepted command, in order. Always under `applying`, so a command an
    /// effect raises is queued.
    fn commit(&self, requests: Vec<Request<A>>) {
        for request in requests {
            match request {
                Request::Then(command) => self.pending.borrow_mut().push_back(command),
                Request::Spawn {
                    id,
                    key,
                    mode,
                    failed,
                    job,
                } => self.spawn(id, key, mode, failed, job),
                Request::Cancel(id) => self.cancel(id),
                Request::Every {
                    id,
                    period,
                    command,
                } => {
                    self.timers
                        .borrow_mut()
                        .every(id, period, Box::new(move || command.clone()));
                }
                Request::After { id, delay, command } => {
                    self.timers
                        .borrow_mut()
                        .after(id, delay, Box::new(move || command.clone()));
                }
                Request::StopTimer(id) => self.timers.borrow_mut().stop(id),
                Request::Report { what, error } => {
                    tracing::error!(app = self.id, what, error, "a failure the app survived");
                    problems::record_failure(self.id, what, &error);
                }
                Request::Close(window) => self.close_window(window),
                Request::Exit => self.request_exit(ExitReason::App),
            }
        }
    }

    fn spawn(
        &self,
        id: JobId,
        key: Option<JobKey>,
        mode: Concurrency,
        failed: Option<A::Command>,
        job: crate::jobs::Job<A::Command>,
    ) {
        if let Some(key) = &key {
            let running = self.by_key.borrow().get(key).copied();
            match (running, mode) {
                (Some(_), Concurrency::RejectIfRunning) => {
                    tracing::debug!(
                        app = self.id,
                        ?key,
                        "a job of this kind is running; this one is not started"
                    );
                    // The app hears of it, as it would of a panic: its `failed` command runs.
                    if let Some(failed) = failed {
                        self.pending.borrow_mut().push_back(failed);
                    }
                    return;
                }
                (Some(old), Concurrency::ReplacePrevious) => self.cancel(old),
                _ => {}
            }
            self.by_key.borrow_mut().insert(key.clone(), id);
        }
        let cancel = Cancel::new();
        self.jobs.borrow_mut().insert(
            id,
            Waiting {
                cancel: cancel.clone(),
                key,
                failed,
            },
        );
        let deliver: Arc<dyn Deliver<A::Command>> = Arc::new(PostDeliver(self.post.clone()));
        self.executor.borrow_mut().spawn(id, cancel, job, deliver);
    }

    /// Forgets a job's key, if it still names this job.
    fn forget_key(&self, id: JobId, key: Option<JobKey>) {
        if let Some(key) = key {
            let mut by_key = self.by_key.borrow_mut();
            if by_key.get(&key) == Some(&id) {
                by_key.remove(&key);
            }
        }
    }

    /// Stops waiting on a job: its cancel flag is set and its late result is dropped.
    fn cancel(&self, id: JobId) {
        if let Some(waiting) = self.jobs.borrow_mut().remove(&id) {
            waiting.cancel.set();
            self.forget_key(id, waiting.key);
        }
    }

    /// A finished job: its result is applied only if the core is still waiting on it. A job that
    /// ended without one (a panic, a refusal) runs the `failed` command it was spawned with.
    fn finished(&self, finished: Finished<A::Command>) {
        match finished {
            Finished::Done { id, command } => {
                let waited = self.jobs.borrow_mut().remove(&id);
                match waited {
                    Some(waiting) => {
                        self.forget_key(id, waiting.key);
                        self.run(command);
                    }
                    None => tracing::debug!(app = self.id, job = id.0, "a late result, dropped"),
                }
            }
            Finished::Panicked { id, what } => {
                tracing::error!(app = self.id, job = id.0, what, "a job panicked");
                problems::record_failure(self.id, "job", &what);
                self.ended_without_result(id);
            }
            Finished::Refused { id, why } => {
                tracing::warn!(app = self.id, job = id.0, why, "a job was not started");
                problems::record_failure(self.id, "job_refused", &why);
                self.ended_without_result(id);
            }
        }
    }

    fn ended_without_result(&self, id: JobId) {
        let waited = self.jobs.borrow_mut().remove(&id);
        if let Some(waiting) = waited {
            self.forget_key(id, waiting.key);
            if let Some(failed) = waiting.failed {
                self.run(failed);
            }
        }
    }

    /// Everything posted since the last drain, in order. The wake calls this.
    pub fn drain(&self) {
        for inbound in self.inbox.drain() {
            match inbound {
                Inbound::Command(command) => self.run(command),
                Inbound::Finished(finished) => self.finished(finished),
                Inbound::Event(event) => self.event(event),
                Inbound::Continue => self.drain_pending(),
            }
        }
    }

    fn event(&self, event: Event) {
        let command = A::event(event.clone(), &self.model.borrow());
        match (command, event) {
            (Some(command), _) => self.run(command),
            (None, Event::CloseRequested(window)) => self.close_window(window),
            (None, Event::ExitRequested(reason)) => self.request_exit(reason),
            (None, Event::Activated(_)) => {}
        }
    }

    fn close_window(&self, window: WindowId) {
        if window != WindowId::PRIMARY {
            return;
        }
        if self.window_shown.replace(false) {
            let _ = self.ui.hide();
        }
        // One window for now: closing it ends the process (`Lifetime::WhileWindowsOpen`).
        self.request_exit(ExitReason::LastWindowClosed);
    }

    fn request_exit(&self, reason: ExitReason) {
        if self.exit.get().is_none() {
            self.exit.set(Some(reason));
            let _ = slint::quit_event_loop();
        }
    }

    /// The end, in order: no more posts, no more timers, every job cancelled, a bounded wait
    /// for the pool. Nothing in the model is touched here.
    pub fn shutdown(&self, grace: std::time::Duration) -> bool {
        self.inbox.close();
        self.timers.borrow_mut().stop_all();
        let ids: Vec<JobId> = self.jobs.borrow().keys().copied().collect();
        for id in ids {
            self.cancel(id);
        }
        let drained = self.executor.borrow().drain(grace);
        if !drained {
            tracing::warn!(
                app = self.id,
                "a job did not stop within the grace period; exiting without it"
            );
        }
        drained
    }

    /// Jobs the core is waiting on.
    pub fn jobs_waiting(&self) -> usize {
        self.jobs.borrow().len()
    }

    pub fn timers_armed(&self) -> usize {
        self.timers.borrow().armed()
    }

    /// A refusal raised before a command existed (a converter in `try_cmd1` could not make one).
    fn refuse(&self, refused: Refused) {
        tracing::warn!(app = self.id, kind = refused.kind, "{}", refused.sentence);
        *self.last_refusal.borrow_mut() = Some(refused);
    }
}

/// The handle a view binds callbacks with. It holds the core weakly: the callbacks live in the
/// window, the window lives in the core, and a strong handle there would keep the core alive
/// for ever. Cloning shares it; it never leaves the UI thread.
pub struct Cx<A: App>(pub(crate) Weak<Core<A>>);

impl<A: App> Clone for Cx<A> {
    fn clone(&self) -> Self {
        Cx(self.0.clone())
    }
}

/// The window, borrowed through a [`Cx`] for as long as the guard lives.
pub struct UiRef<A: App>(Rc<Core<A>>);

impl<A: App> Deref for UiRef<A> {
    type Target = A::Ui;

    fn deref(&self) -> &A::Ui {
        &self.0.ui
    }
}

impl<A: App> Cx<A> {
    pub fn new(core: Rc<Core<A>>) -> Cx<A> {
        Cx(Rc::downgrade(&core))
    }

    pub fn id(&self) -> &'static str {
        A::ID
    }

    /// The core, while it lives. At wire time it always does; a callback fired as the window is
    /// torn down may find it gone, and then does nothing.
    fn core(&self) -> Option<Rc<Core<A>>> {
        self.0.upgrade()
    }

    fn core_or_panic(&self) -> Rc<Core<A>> {
        self.core().expect("the core outlives the window bound to it")
    }

    pub fn run(&self, command: impl Into<A::Command>) {
        match self.core() {
            Some(core) => core.run(command.into()),
            None => tracing::debug!(app = A::ID, "a command after the core was dropped; ignored"),
        }
    }

    pub fn read<R>(&self, f: impl FnOnce(&A::Model) -> R) -> R {
        self.core_or_panic().read(f)
    }

    /// The window, for a view that binds a list model at wire time. Not for `update`.
    pub fn ui(&self) -> UiRef<A> {
        UiRef(self.core_or_panic())
    }

    /// The `Send` endpoint, for the surface's thread (step 3) and for tests.
    pub fn post(&self) -> Post<Inbound<A>> {
        self.core_or_panic().post()
    }

    /// A Slint callback of no arguments that runs the command `f` makes.
    pub fn cmd(&self, f: impl Fn() -> A::Command + 'static) -> impl Fn() + 'static {
        let cx = self.clone();
        move || cx.run(f())
    }

    /// A Slint callback of one argument that runs the command `f` makes.
    pub fn cmd1<T>(&self, f: impl Fn(T) -> A::Command + 'static) -> impl Fn(T) + 'static {
        let cx = self.clone();
        move |t| cx.run(f(t))
    }

    pub fn cmd2<T, U>(&self, f: impl Fn(T, U) -> A::Command + 'static) -> impl Fn(T, U) + 'static {
        let cx = self.clone();
        move |t, u| cx.run(f(t, u))
    }

    /// A Slint callback of one argument whose converter may refuse (a list id that does not
    /// parse): the refusal is the person's and the Mind's, as a command's would be.
    pub fn try_cmd1<T>(
        &self,
        f: impl Fn(T) -> Result<A::Command, Refused> + 'static,
    ) -> impl Fn(T) + 'static {
        let cx = self.clone();
        move |t| match f(t) {
            Ok(command) => cx.run(command),
            Err(refused) => {
                if let Some(core) = cx.core() {
                    core.refuse(refused);
                }
            }
        }
    }
}
