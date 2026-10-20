//! The UI-thread core: the one place the model lives, and how commands reach it
//! (design/app-framework-2026-10-21.md, §§2.3, 3, 4).
//!
//! [`Core`] owns the model, the window, the timers, the job table, the open settlements and the
//! settings store. It applies a command by calling `update`, committing the effects it asked
//! for, and calling `render` for the parts that changed. Nothing else touches the model. [`Cx`]
//! is the handle a view uses to bind callbacks, and [`Core::post`] is the `Send` endpoint every
//! other thread uses.
//!
//! Re-entrancy: a command raised while one is being applied (from `then`, or a Slint callback
//! fired by `render`) is queued and applied after the current one's render, never nested. One
//! drain applies at most [`PENDING_PER_DRAIN`] queued commands before yielding to the event
//! loop, so a `then` cycle cannot freeze drawing.
//!
//! Capacity: commands share the bounded inbox and a flood of them is refused; a job's outcome
//! does not. It goes through the inbox's reserved lane, bounded by the pool itself, so saturation
//! never loses a result and never leaves a key marked running (§4).
//!
//! Invocations: a command that came through the surface runs under an [`Invocation`]; the jobs
//! and `then` chains it starts carry it, so a late result, a panic or a replacement is known to
//! belong to it, and an intent that settles explicitly is answered exactly once, and only when
//! nothing of it is still running or queued.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::ops::Deref;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::{Duration, Instant};

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

/// How many queued (`then`) commands one drain applies before the rest wait for the next wake.
pub const PENDING_PER_DRAIN: usize = queue::CAPACITY;

/// What reaches the core from any thread.
pub enum Inbound<A: App> {
    Command(A::Command),
    Finished(Finished<A::Command>),
    Event(Event<A::Settings>),
    /// The persistence lane finished a write.
    Written(Written),
    /// The housekeeping thread saw a file change.
    Seen(Seen),
    /// The rest of a drain that reached [`PENDING_PER_DRAIN`]: posted by the core to itself, and
    /// by nothing else (the token cannot be made outside this crate).
    Continue(Continuation),
}

/// The core's own token for [`Inbound::Continue`].
pub struct Continuation(());

/// A job the core is waiting on.
struct Waiting<C> {
    cancel: Cancel,
    key: Option<JobKey>,
    invocation: Option<Invocation>,
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
    settings: RefCell<Store<A::Settings>>,
    lane: RefCell<Box<dyn Lane>>,
    jobs: RefCell<HashMap<JobId, Waiting<A::Command>>>,
    by_key: RefCell<HashMap<JobKey, JobId>>,
    next_job: Cell<u64>,
    next_invocation: Cell<u64>,
    open: RefCell<HashMap<InvocationId, Open<A>>>,
    current: Cell<Option<Invocation>>,
    post: Post<Inbound<A>>,
    inbox: Inbox<Inbound<A>>,
    pending: RefCell<VecDeque<(Option<Invocation>, A::Command)>>,
    applying: Cell<bool>,
    /// Queued commands this wake may still apply: one [`PENDING_PER_DRAIN`] per wake, shared by
    /// every drain within it, so a flood is bounded per wake and not per call.
    budget: Cell<usize>,
    /// Inside [`Core::drain`]: a `run` from within it draws on the wake's budget, not a fresh one.
    in_wake: Cell<bool>,
    /// Whether a `Continue` is already in the inbox: at most one is ever outstanding.
    continue_posted: Cell<bool>,
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
    /// How a post from another thread gets the UI thread to call [`Core::drain`].
    pub wake: Wake,
}

impl<A: App> Core<A> {
    /// A core over `model` and `ui`, with the parts it is given.
    pub fn new(model: A::Model, ui: A::Ui, parts: Parts<A>) -> Rc<Core<A>> {
        let (post, inbox) = queue::bounded(queue::CAPACITY, parts.wake);
        let report: Arc<dyn Report> = Arc::new(persisted::PostWritten(post.clone()));
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
                timers: RefCell::new((parts.timers)(sink)),
                executor: RefCell::new(parts.executor),
                settings: RefCell::new(parts.settings),
                lane: RefCell::new((parts.lane)(report)),
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
                budget: Cell::new(PENDING_PER_DRAIN),
                in_wake: Cell::new(false),
                continue_posted: Cell::new(false),
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
        self.fresh_wake();
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
        self.fresh_wake();
        let mut fx = Effects::starting(self.next_job.get());
        A::start(&self.model.borrow(), &mut fx);
        let (requests, changed, next) = fx.into_parts();
        self.next_job.set(next);
        self.applying.set(true);
        self.current.set(None);
        self.commit(requests);
        if !changed.is_empty() {
            A::render(&self.ui, WindowId::PRIMARY, &self.model.borrow(), &changed);
        }
        self.applying.set(false);
        self.drain_pending();
    }

    /// Apply a local command now (on the UI thread), or queue it if one is being applied.
    pub fn run(&self, command: A::Command) {
        if self.applying.get() {
            self.queue(None, command);
            return;
        }
        self.fresh_wake();
        let _ = self.apply(None, command);
        self.drain_pending();
    }

    /// A call from outside [`Core::drain`] (a Slint callback, a timer, the surface's hop, the
    /// first render) is a wake of its own, with the whole budget.
    fn fresh_wake(&self) {
        if !self.in_wake.get() {
            self.budget.set(PENDING_PER_DRAIN);
        }
    }

    /// Apply a command that came through the surface, under an invocation. Returns the first
    /// command's refusal, if it was refused. An explicit settlement opened for `id` is settled
    /// here with the reply when the invocation started no job and queued nothing more.
    pub fn run_invocation(
        &self,
        origin: Origin,
        command: A::Command,
        id: Option<InvocationId>,
    ) -> Result<(), Refused> {
        if self.post.is_closed() {
            return Err(Refused::new("closing", "The app is closing."));
        }
        let id = id.unwrap_or_else(|| {
            let id = InvocationId(self.next_invocation.get());
            self.next_invocation.set(id.0 + 1);
            id
        });
        let invocation = Invocation { id, origin };
        if self.applying.get() {
            // A surface call while a command is applied cannot happen on one UI thread, but the
            // queue is the safe answer if it ever does.
            self.queue(Some(invocation), command);
            return Ok(());
        }
        self.fresh_wake();
        let result = self.apply(Some(invocation), command);
        self.drain_pending();
        if result.is_ok() {
            self.settle_if_idle(id);
        }
        result
    }

    /// Queues a command behind the one being applied, under `invocation` if it has one; an open
    /// settlement counts it, so the invocation is not settled while it waits.
    fn queue(&self, invocation: Option<Invocation>, command: A::Command) {
        if let Some(invocation) = invocation {
            self.count_queued(invocation.id, 1);
        }
        self.pending.borrow_mut().push_back((invocation, command));
    }

    /// Applies what is queued while this wake's budget lasts; the rest wait for the next wake,
    /// so the event loop draws between floods, and one `Continue` at most is left in the inbox
    /// to bring that wake about. A queued command's refusal is its invocation's terminal
    /// outcome.
    fn drain_pending(&self) {
        while self.budget.get() > 0 {
            let next = self.pending.borrow_mut().pop_front();
            let Some((invocation, command)) = next else {
                return;
            };
            self.budget.set(self.budget.get() - 1);
            if let Some(invocation) = invocation {
                self.count_queued(invocation.id, -1);
            }
            let result = self.apply(invocation, command);
            if let Some(invocation) = invocation {
                match result {
                    Ok(()) => self.settle_if_idle(invocation.id),
                    Err(refused) => self.settle(invocation.id, Err(refused.sentence)),
                }
            }
        }
        if !self.pending.borrow().is_empty() && !self.continue_posted.replace(true) {
            tracing::warn!(
                app = self.id,
                "{PENDING_PER_DRAIN} queued commands applied in one wake; the rest wait for the next"
            );
            if self.post.post_control(Inbound::Continue(Continuation(()))).is_err() {
                self.continue_posted.set(false);
            }
        }
    }

    /// Commands queued and not yet applied.
    pub fn pending_commands(&self) -> usize {
        self.pending.borrow().len()
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
    /// under. Always under `applying`, so a command an effect raises is queued.
    fn commit(&self, requests: Vec<Request<A>>) {
        let invocation = self.current.get();
        for request in requests {
            match request {
                Request::Then(command) => self.queue(invocation, command),
                Request::Spawn {
                    id,
                    key,
                    mode,
                    failed,
                    job,
                } => self.spawn(id, key, mode, failed, job, invocation),
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
        invocation: Option<Invocation>,
    ) {
        let mut replaces = None;
        if let Some(key) = &key {
            let running = self.by_key.borrow().get(key).copied();
            match (running, mode) {
                (Some(_), Concurrency::RejectIfRunning) => {
                    tracing::debug!(
                        app = self.id,
                        ?key,
                        "a job of this kind is running; this one is not started"
                    );
                    // The work was refused: an invocation waiting on it is told so, not told
                    // "done" from a model the job never touched; and the app hears of it through
                    // its `failed` command, as it would of a panic.
                    if let Some(invocation) = invocation {
                        self.settle(
                            invocation.id,
                            Err("a job of this kind is already running".into()),
                        );
                    }
                    if let Some(failed) = failed {
                        self.queue(invocation, failed);
                    }
                    return;
                }
                (Some(old), Concurrency::ReplacePrevious) => replaces = Some(old),
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
                failed,
            },
        );
        if let Some(invocation) = invocation {
            if let Some(open) = self.open.borrow_mut().get_mut(&invocation.id) {
                open.jobs += 1;
            }
        }
        // The new job is counted before the old one is cancelled, so a replacement within one
        // invocation never leaves it with nothing running for an instant and settles it.
        if let Some(old) = replaces {
            self.cancel(old);
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
    /// invocation it started from. A job that ended without a result (a panic, a refusal) runs
    /// the `failed` command it was spawned with, and an invocation waiting on it is told.
    fn finished(&self, finished: Finished<A::Command>) {
        match finished {
            Finished::Done { id, command } => {
                let waited = self.jobs.borrow_mut().remove(&id);
                let Some(waiting) = waited else {
                    tracing::debug!(app = self.id, job = id.0, "a late result, dropped");
                    return;
                };
                self.forget_key(id, waiting.key);
                if let Some(invocation) = waiting.invocation {
                    self.job_landed(invocation.id);
                }
                if self.applying.get() {
                    self.queue(waiting.invocation, command);
                } else {
                    let result = self.apply(waiting.invocation, command);
                    self.drain_pending();
                    if let Some(invocation) = waiting.invocation {
                        match result {
                            Ok(()) => self.settle_if_idle(invocation.id),
                            Err(refused) => self.settle(invocation.id, Err(refused.sentence)),
                        }
                    }
                }
            }
            Finished::Panicked { id, what } => {
                tracing::error!(app = self.id, job = id.0, what, "a job panicked");
                problems::record_failure(self.id, "job", &what);
                self.ended_without_result(id, format!("the work failed: {what}"));
            }
            Finished::Refused { id, why } => {
                tracing::warn!(app = self.id, job = id.0, why, "a job was not started");
                problems::record_failure(self.id, "job_refused", &why);
                self.ended_without_result(id, format!("the work could not start: {why}"));
            }
        }
    }

    fn ended_without_result(&self, id: JobId, outcome: String) {
        let waited = self.jobs.borrow_mut().remove(&id);
        let Some(waiting) = waited else {
            return;
        };
        self.forget_key(id, waiting.key);
        if let Some(invocation) = waiting.invocation {
            if self.job_landed(invocation.id) {
                self.settle(invocation.id, Err(outcome));
            }
        }
        if let Some(failed) = waiting.failed {
            if self.applying.get() {
                self.queue(waiting.invocation, failed);
            } else {
                let _ = self.apply(waiting.invocation, failed);
                self.drain_pending();
            }
        }
    }

    /// Everything posted since the last drain, in order: one wake, with one budget for what is
    /// queued. The wake calls this.
    pub fn drain(&self) {
        self.in_wake.set(true);
        self.budget.set(PENDING_PER_DRAIN);
        for inbound in self.inbox.drain() {
            match inbound {
                Inbound::Command(command) => self.run(command),
                Inbound::Finished(finished) => self.finished(finished),
                Inbound::Event(event) => self.event(event),
                Inbound::Written(written) => self.written(written),
                Inbound::Seen(seen) => self.seen(seen),
                Inbound::Continue(_) => {
                    self.continue_posted.set(false);
                    self.drain_pending();
                }
            }
        }
        self.in_wake.set(false);
    }

    /// An event: the app's command for it, or the default.
    pub(crate) fn event(&self, event: Event<A::Settings>) {
        let command = A::event(event.clone(), &self.model.borrow());
        match (command, event) {
            (Some(command), _) => self.run(command),
            (None, Event::CloseRequested(window)) => self.close_window(window),
            (None, Event::ExitRequested(reason)) => self.request_exit(reason),
            (None, Event::Activated(_)) => {}
            (None, Event::SettingsSaved) => {}
            (None, Event::SettingsChangedOutside(_)) => {}
            (None, Event::SettingsNotSaved(sentence)) => {
                problems::record_failure(self.id, "settings", &sentence);
            }
            // The app's own value stands and the file is brought back to it; never silently
            // (§6): the outside edit that lost is written down, so it can be found.
            (None, Event::SettingsConflict { mine, .. }) => {
                let why = "the settings file was edited outside the app while a save was \
                           pending; the app's value stands and was written over the edit";
                tracing::warn!(app = self.id, why);
                problems::record_failure(self.id, "settings_conflict", why);
                self.resolve_settings(mine);
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

    /// The end, in order: no more posts, no more timers, the writes owed handed to the lane,
    /// every open answer refused, every job cancelled, then one bounded wait shared by the lane
    /// and the pool (§4: the grace is the whole of it, not one per drain). Nothing in the model
    /// is touched here.
    pub fn shutdown(&self, grace: Duration) -> bool {
        let deadline = Instant::now() + grace;
        self.inbox.close();
        self.timers.borrow_mut().stop_all();
        // A save asked for is a save made, before the process ends.
        self.flush_settings();
        // Every owed answer is refused as a closing, before a cancelled job can call it replaced.
        let owed: Vec<InvocationId> = self.open.borrow().keys().copied().collect();
        for id in owed {
            self.settle(id, Err("the app is closing".into()));
        }
        let ids: Vec<JobId> = self.jobs.borrow().keys().copied().collect();
        for id in ids {
            self.cancel(id);
        }
        let written = self
            .lane
            .borrow()
            .drain(deadline.saturating_duration_since(Instant::now()));
        if !written {
            tracing::warn!(
                app = self.id,
                "a write did not finish within the grace period; it is not durable"
            );
        }
        let drained = self
            .executor
            .borrow()
            .drain(deadline.saturating_duration_since(Instant::now()));
        if !drained {
            tracing::warn!(
                app = self.id,
                "a job did not stop within the grace period; exiting without it"
            );
        }
        written && drained
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

    /// The `Send` endpoint, for tests and for anything off the UI thread.
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
