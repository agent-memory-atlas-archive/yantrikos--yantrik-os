//! The UI-thread core: the one place the model lives, and how commands reach it
//! (design/app-framework-2026-10-21.md, §§2.3, 3, 4).
//!
//! [`Core`] owns the model, the window, the timers and the job table. It applies a command by
//! calling `update`, committing the effects it asked for, and calling `render` for the parts
//! that changed. Nothing else touches the model. [`Cx`] is the handle a view uses to bind
//! callbacks, and [`Core::post`] is the `Send` endpoint every other thread uses.
//!
//! Re-entrancy: a command raised while one is being applied (from `then`, or a Slint callback
//! fired by `render`) is queued and applied after the current one's render, never nested.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use slint::ComponentHandle as _;

use crate::app::{App, Changed, Event, ExitReason, WindowId};
use crate::effects::{Effects, Request};
use crate::jobs::{Cancel, Concurrency, Deliver, Executor, Finished, JobId, JobKey};
use crate::problems;
use crate::queue::{self, Inbox, Post, Wake};
use crate::refused::Refused;
use crate::timers::Timers;

/// What reaches the core from any thread.
pub enum Inbound<A: App> {
    Command(A::Command),
    Finished(Finished<A::Command>),
    Event(Event),
}

/// A job the core is waiting on.
struct Waiting {
    cancel: Cancel,
    key: Option<JobKey>,
}

/// The core, on the UI thread. One per app process; a test makes its own.
pub struct Core<A: App> {
    id: &'static str,
    model: RefCell<A::Model>,
    ui: A::Ui,
    timers: RefCell<Box<dyn Timers<A::Command>>>,
    executor: RefCell<Box<dyn Executor<A::Command>>>,
    jobs: RefCell<HashMap<JobId, Waiting>>,
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
        // A late result after shutdown is dropped, as the design says it must be.
        let _ = self.0.post(Inbound::Finished(finished));
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
        let core = Rc::new_cyclic(|weak: &std::rc::Weak<Core<A>>| {
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
        });
        core
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

    /// Draw everything: the first render.
    pub fn render_all(&self) {
        A::render(
            &self.ui,
            WindowId::PRIMARY,
            &self.model.borrow(),
            &Changed::all(),
        );
    }

    /// `App::start`, and its effects.
    pub fn start(&self) {
        let mut fx = Effects::new(self.next_job.get());
        A::start(&self.model.borrow(), &mut fx);
        let (requests, changed, next) = fx.into_parts();
        self.next_job.set(next);
        self.commit(requests);
        if !changed.is_empty() {
            A::render(&self.ui, WindowId::PRIMARY, &self.model.borrow(), &changed);
        }
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

    fn drain_pending(&self) {
        loop {
            let next = self.pending.borrow_mut().pop_front();
            let Some(command) = next else { return };
            self.apply(command);
        }
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

    /// The effects of an accepted command, in order.
    fn commit(&self, requests: Vec<Request<A>>) {
        for request in requests {
            match request {
                Request::Then(command) => self.pending.borrow_mut().push_back(command),
                Request::Spawn { id, key, mode, job } => self.spawn(id, key, mode, job),
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
            },
        );
        let deliver: Arc<dyn Deliver<A::Command>> = Arc::new(PostDeliver(self.post.clone()));
        self.executor.borrow_mut().spawn(id, cancel, job, deliver);
    }

    /// Stops waiting on a job: its cancel flag is set and its late result is dropped.
    fn cancel(&self, id: JobId) {
        if let Some(waiting) = self.jobs.borrow_mut().remove(&id) {
            waiting.cancel.set();
            if let Some(key) = waiting.key {
                let mut by_key = self.by_key.borrow_mut();
                if by_key.get(&key) == Some(&id) {
                    by_key.remove(&key);
                }
            }
        }
    }

    /// A finished job: its result is applied only if the core is still waiting on it.
    fn finished(&self, finished: Finished<A::Command>) {
        match finished {
            Finished::Done { id, command } => {
                let waited = self.jobs.borrow_mut().remove(&id);
                match waited {
                    Some(waiting) => {
                        if let Some(key) = waiting.key {
                            let mut by_key = self.by_key.borrow_mut();
                            if by_key.get(&key) == Some(&id) {
                                by_key.remove(&key);
                            }
                        }
                        self.run(command);
                    }
                    None => tracing::debug!(app = self.id, job = id.0, "a late result, dropped"),
                }
            }
            Finished::Panicked { id, what } => {
                self.cancel(id);
                tracing::error!(app = self.id, job = id.0, what, "a job panicked");
                problems::record_failure(self.id, "job", &what);
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
}

/// The handle a view binds callbacks with. Cloning shares the core; it never leaves the UI
/// thread (it holds an `Rc`).
pub struct Cx<A: App>(pub(crate) Rc<Core<A>>);

impl<A: App> Clone for Cx<A> {
    fn clone(&self) -> Self {
        Cx(self.0.clone())
    }
}

impl<A: App> Cx<A> {
    pub fn new(core: Rc<Core<A>>) -> Cx<A> {
        Cx(core)
    }

    pub fn id(&self) -> &'static str {
        self.0.id
    }

    pub fn run(&self, command: impl Into<A::Command>) {
        self.0.run(command.into());
    }

    pub fn read<R>(&self, f: impl FnOnce(&A::Model) -> R) -> R {
        self.0.read(f)
    }

    /// The window, for a view that binds a list model at wire time. Not for `update`.
    pub fn ui(&self) -> &A::Ui {
        &self.0.ui
    }

    /// The `Send` endpoint, for the surface's thread (step 3) and for tests.
    pub fn post(&self) -> Post<Inbound<A>> {
        self.0.post()
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
            Err(refused) => cx.0.refuse(refused),
        }
    }
}

impl<A: App> Core<A> {
    /// A refusal raised before a command existed (a converter in `cmd1` could not make one).
    fn refuse(&self, refused: Refused) {
        tracing::warn!(app = self.id, kind = refused.kind, "{}", refused.sentence);
        *self.last_refusal.borrow_mut() = Some(refused);
    }
}
