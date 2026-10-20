//! The UI-thread core: the one place the model lives, and how commands reach it
//! (design/app-framework-2026-10-21.md, §§2.3, 3, 4).
//!
//! [`Core`] owns the model, the window, the timers, the job table and the open settlements. It
//! applies a command by calling `update`, committing the effects it asked for, and calling
//! `render` for the parts that changed. Nothing else touches the model. [`Cx`] is the handle a
//! view uses to bind callbacks, and [`Core::post`] is the `Send` endpoint every other thread
//! uses.
//!
//! Re-entrancy: a command raised while one is being applied (from `then`, or a Slint callback
//! fired by `render`) is queued and applied after the current one's render, never nested.
//!
//! Invocations: a command that came through the surface runs under an [`Invocation`]; the jobs
//! and `then` chains it starts carry it, so a late result, a panic or a replacement is known to
//! belong to it, and an intent that settles explicitly is answered exactly once.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use slint::ComponentHandle as _;

use crate::app::{App, Changed, Event, ExitReason, WindowId};
use crate::effects::{Effects, Request};
use crate::intents::{Invocation, InvocationId, Origin};
use crate::jobs::{Cancel, Concurrency, Deliver, Executor, Finished, JobId, JobKey};
use crate::persist::{Lane, Report, Written};
use crate::problems;
use crate::queue::{self, Inbox, Post, Wake};
use crate::refused::Refused;
use crate::settings::Store;
use crate::timers::Timers;
use crate::watch::Seen;

mod persisted;
mod settlement;
use settlement::Open;

/// What reaches the core from any thread.
pub enum Inbound<A: App> {
    Command(A::Command),
    Finished(Finished<A::Command>),
    Event(Event<A::Settings>),
    /// The persistence lane finished a write.
    Written(Written),
    /// The housekeeping thread saw a file change.
    Seen(Seen),
}

/// A job the core is waiting on.
struct Waiting {
    cancel: Cancel,
    key: Option<JobKey>,
    invocation: Option<Invocation>,
}

/// The core, on the UI thread. One per app process; a test makes its own.
pub struct Core<A: App> {
    id: &'static str,
    model: RefCell<A::Model>,
    ui: A::Ui,
    timers: RefCell<Box<dyn Timers<A::Command>>>,
    executor: RefCell<Box<dyn Executor<A::Command>>>,
    settings: RefCell<Store<A::Settings>>,
    lane: RefCell<Box<dyn Lane>>,
    paths: crate::paths::Paths,
    /// Each draft's latest revision, so a write's outcome is known to be the latest or not.
    drafts: RefCell<HashMap<String, u64>>,
    jobs: RefCell<HashMap<JobId, Waiting>>,
    by_key: RefCell<HashMap<JobKey, JobId>>,
    next_job: Cell<u64>,
    next_invocation: Cell<u64>,
    open: RefCell<HashMap<InvocationId, Open<A>>>,
    current: Cell<Option<Invocation>>,
    post: Post<Inbound<A>>,
    inbox: Inbox<Inbound<A>>,
    pending: RefCell<VecDeque<(Option<Invocation>, A::Command)>>,
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

/// Makes the timers over the sink that applies a fired command.
pub type MakeTimers<C> = Box<dyn FnOnce(crate::timers::Sink<C>) -> Box<dyn Timers<C>>>;
/// Makes the persistence lane over where its outcomes go.
pub type MakeLane = Box<dyn FnOnce(Arc<dyn Report>) -> Box<dyn Lane>>;

/// What the core is built over, besides the model and the window.
pub struct Parts<A: App> {
    /// The job pool.
    pub executor: Box<dyn Executor<A::Command>>,
    pub timers: MakeTimers<A::Command>,
    pub lane: MakeLane,
    /// The settings as opened for the boot.
    pub settings: Store<A::Settings>,
    /// Where the app's files are: the drafts go under the state dir.
    pub paths: crate::paths::Paths,
    /// How a post from another thread gets the UI thread to call [`Core::drain`].
    pub wake: Wake,
}

impl<A: App> Core<A> {
    /// A core over `model` and `ui`, with the parts it is given.
    pub fn new(model: A::Model, ui: A::Ui, parts: Parts<A>) -> Rc<Core<A>> {
        let (post, inbox) = queue::bounded(queue::CAPACITY, parts.wake);
        let report: Arc<dyn Report> = Arc::new(persisted::PostWritten(post.clone()));
        Rc::new_cyclic(|weak: &std::rc::Weak<Core<A>>| {
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
                timers: RefCell::new((parts.timers)(sink)),
                executor: RefCell::new(parts.executor),
                settings: RefCell::new(parts.settings),
                lane: RefCell::new((parts.lane)(report)),
                paths: parts.paths,
                drafts: RefCell::new(HashMap::new()),
                jobs: RefCell::new(HashMap::new()),
                by_key: RefCell::new(HashMap::new()),
                next_job: Cell::new(1),
                next_invocation: Cell::new(1),
                open: RefCell::new(HashMap::new()),
                current: Cell::new(None),
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

    /// Where the app's files are.
    pub fn paths(&self) -> &crate::paths::Paths {
        &self.paths
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
        let mut fx = Effects::new(self.next_job.get(), None);
        A::start(&self.model.borrow(), &mut fx);
        let (requests, changed, next) = fx.into_parts();
        self.next_job.set(next);
        self.current.set(None);
        self.commit(requests);
        if !changed.is_empty() {
            A::render(&self.ui, WindowId::PRIMARY, &self.model.borrow(), &changed);
        }
        self.drain_pending();
    }

    /// Apply a local command now (on the UI thread), or queue it if one is being applied.
    pub fn run(&self, command: A::Command) {
        if self.applying.get() {
            self.pending.borrow_mut().push_back((None, command));
            return;
        }
        let _ = self.apply(None, command);
        self.drain_pending();
    }

    /// Apply a command that came through the surface, under an invocation. Returns the first
    /// command's refusal, if it was refused. An explicit settlement opened for `id` is settled
    /// here with the reply when the invocation started no job.
    pub fn run_invocation(
        &self,
        origin: Origin,
        command: A::Command,
        id: Option<InvocationId>,
    ) -> Result<(), Refused> {
        let id = id.unwrap_or_else(|| {
            let id = InvocationId(self.next_invocation.get());
            self.next_invocation.set(id.0 + 1);
            id
        });
        let invocation = Invocation { id, origin };
        if self.applying.get() {
            // A surface call while a command is applied cannot happen on one UI thread, but the
            // queue is the safe answer if it ever does.
            self.pending
                .borrow_mut()
                .push_back((Some(invocation), command));
            return Ok(());
        }
        let result = self.apply(Some(invocation), command);
        self.drain_pending();
        if result.is_ok() {
            self.settle_if_idle(id);
        }
        result
    }

    fn drain_pending(&self) {
        loop {
            let next = self.pending.borrow_mut().pop_front();
            let Some((invocation, command)) = next else {
                return;
            };
            let _ = self.apply(invocation, command);
            if let Some(invocation) = invocation {
                self.settle_if_idle(invocation.id);
            }
        }
    }

    /// One command: update, commit its effects if it was accepted, render what changed.
    fn apply(&self, invocation: Option<Invocation>, command: A::Command) -> Result<(), Refused> {
        self.applying.set(true);
        self.current.set(invocation);
        let mut fx = Effects::new(self.next_job.get(), invocation);
        let result = A::update(&mut self.model.borrow_mut(), command, &mut fx);
        let (requests, changed, next) = fx.into_parts();
        match &result {
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
                *self.last_refusal.borrow_mut() = Some(refused.clone());
            }
        }
        self.current.set(None);
        self.applying.set(false);
        result
    }

    /// The effects of an accepted command, in order. `self.current` is the invocation it ran
    /// under.
    fn commit(&self, requests: Vec<Request<A>>) {
        let invocation = self.current.get();
        for request in requests {
            match request {
                Request::Then(command) => {
                    self.pending.borrow_mut().push_back((invocation, command))
                }
                Request::Spawn { id, key, mode, job } => self.spawn(id, key, mode, job, invocation),
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
                Request::Settle(result) => {
                    if let Some(invocation) = invocation {
                        self.settle(invocation.id, result.map_err(|r| r.sentence));
                    }
                }
                Request::SaveSettings {
                    value,
                    replace_unusable,
                } => self.save_settings(value, replace_unusable),
                Request::SaveDraft { name, bytes, delay } => self.save_draft(name, bytes, delay),
                Request::DiscardDraft(name) => self.discard_draft(name),
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
        invocation: Option<Invocation>,
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
                invocation,
            },
        );
        if let Some(invocation) = invocation {
            if let Some(open) = self.open.borrow_mut().get_mut(&invocation.id) {
                open.jobs += 1;
            }
        }
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

    /// Stops waiting on a job: its cancel flag is set and its late result is dropped. An
    /// invocation whose last job this was is settled as replaced.
    fn cancel(&self, id: JobId) {
        let Some(waiting) = self.jobs.borrow_mut().remove(&id) else {
            return;
        };
        waiting.cancel.set();
        self.forget_key(id, waiting.key);
        if let Some(invocation) = waiting.invocation {
            if self.job_landed(invocation.id) {
                self.settle(
                    invocation.id,
                    Err("the work was replaced by a newer request".into()),
                );
            }
        }
    }

    /// A finished job: its result is applied only if the core is still waiting on it, under the
    /// invocation it started from.
    fn finished(&self, finished: Finished<A::Command>) {
        match finished {
            Finished::Done { id, command } => {
                let waited = self.jobs.borrow_mut().remove(&id);
                let Some(waiting) = waited else {
                    tracing::debug!(app = self.id, job = id.0, "a late result, dropped");
                    return;
                };
                self.forget_key(id, waiting.key);
                let last = waiting.invocation.is_some_and(|i| self.job_landed(i.id));
                if self.applying.get() {
                    self.pending
                        .borrow_mut()
                        .push_back((waiting.invocation, command));
                } else {
                    let _ = self.apply(waiting.invocation, command);
                    self.drain_pending();
                }
                if let (true, Some(invocation)) = (last, waiting.invocation) {
                    self.settle_if_idle(invocation.id);
                }
            }
            Finished::Panicked { id, what } => {
                let waited = self.jobs.borrow_mut().remove(&id);
                tracing::error!(app = self.id, job = id.0, what, "a job panicked");
                problems::record_failure(self.id, "job", &what);
                if let Some(waiting) = waited {
                    self.forget_key(id, waiting.key);
                    if let Some(invocation) = waiting.invocation {
                        if self.job_landed(invocation.id) {
                            self.settle(invocation.id, Err(format!("the work failed: {what}")));
                        }
                    }
                }
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
                Inbound::Written(written) => self.written(written),
                Inbound::Seen(seen) => self.seen(seen),
            }
        }
    }

    /// An event: the app's command for it, or the default. The runner raises the first
    /// `Event::Launch` through this; a test may raise any.
    pub fn event(&self, event: Event<A::Settings>) {
        let command = A::event(event.clone(), &self.model.borrow());
        match (command, event) {
            (Some(command), _) => self.run(command),
            (None, Event::Launch(_)) => {}
            (None, Event::CloseRequested(window)) => self.close_window(window),
            (None, Event::ExitRequested(reason)) => self.request_exit(reason),
            (None, Event::Activated(_)) => {}
            (None, Event::SettingsSaved) => {}
            (None, Event::SettingsChangedOutside(_)) => {}
            (None, Event::SettingsNotSaved(sentence)) => {
                problems::record_failure(self.id, "settings", &sentence);
            }
            // The app's own value stands; the file is brought back to it.
            (None, Event::SettingsConflict { mine, .. }) => self.resolve_settings(mine),
            (None, Event::DraftSaved(_)) => {}
            (None, Event::DraftNotSaved { name, why }) => {
                problems::record_failure(self.id, "draft", &format!("{name}: {why}"));
            }
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

    /// The end, in order: no more posts, no more timers, every job cancelled, every open answer
    /// refused, a bounded wait for the pool and for the writes still in the lane. Nothing in the
    /// model is touched here.
    pub fn shutdown(&self, grace: std::time::Duration) -> bool {
        self.inbox.close();
        self.timers.borrow_mut().stop_all();
        // A save asked for is a save made, before the process ends.
        self.flush_settings();
        if !self.lane.borrow().drain(grace) {
            tracing::warn!(app = self.id, "a write did not finish within the grace period");
        }
        // Every owed answer is refused as a closing, before a cancelled job can call it replaced.
        let owed: Vec<InvocationId> = self.open.borrow().keys().copied().collect();
        for id in owed {
            self.settle(id, Err("the app is closing".into()));
        }
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

    /// The `Send` endpoint, for tests and for anything off the UI thread.
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
