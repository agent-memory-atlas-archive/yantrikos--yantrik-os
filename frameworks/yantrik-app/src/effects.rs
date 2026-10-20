//! What a command asks the framework to do (design/app-framework-2026-10-21.md, §4).
//!
//! `update` changes the model and *requests* effects; it performs none. The requests are staged
//! here and committed by the runtime only when `update` returns `Ok`, so a refused command runs
//! nothing. In a test, `Effects::recording()` captures them as data.

use std::time::Duration;

use crate::app::{App, Changed, WindowId};
use crate::intents::{Invocation, InvocationId, Origin};
use crate::jobs::{Cancel, Concurrency, Job, JobId, JobKey};
use crate::refused::Refused;
use crate::timers::TimerId;

/// One request, as the runtime applies it after the command commits.
pub enum Request<A: App> {
    /// A command to apply after this one, in order. Not a transaction: it is judged on its own.
    Then(A::Command),
    /// Work off the UI thread. The job returns the command that carries its result.
    Spawn {
        id: JobId,
        key: Option<JobKey>,
        mode: Concurrency,
        job: Job<A::Command>,
    },
    Cancel(JobId),
    /// A command every `period`; arming the same id again restarts it.
    Every {
        id: TimerId,
        period: Duration,
        command: A::Command,
    },
    /// A command once, after `delay`; arming the same id again restarts it (a debounce).
    After {
        id: TimerId,
        delay: Duration,
        command: A::Command,
    },
    StopTimer(TimerId),
    /// A failure the app survived, filed as a problem record.
    Report {
        what: &'static str,
        error: String,
    },
    /// The invocation this command runs under has reached its outcome (`Settle::Explicit`).
    Settle(Result<(), Refused>),
    Close(WindowId),
    Exit,
}

/// The effects one command requests.
pub struct Effects<A: App> {
    requests: Vec<Request<A>>,
    changed: Changed,
    next_job: u64,
    invocation: Option<Invocation>,
}

impl<A: App> Effects<A> {
    /// For the runtime: job ids continue from `next_job`; `invocation` is what the command runs
    /// under, if it came through the surface.
    pub(crate) fn new(next_job: u64, invocation: Option<Invocation>) -> Effects<A> {
        Effects {
            requests: Vec::new(),
            changed: Changed::none(),
            next_job,
            invocation,
        }
    }

    /// For a test of `update`: nothing is run, and [`Effects::requests`] says what was asked.
    pub fn recording() -> Effects<A> {
        Effects::new(1, None)
    }

    /// The same, as if the command came through the surface from `origin`.
    pub fn recording_from(origin: Origin) -> Effects<A> {
        Effects::new(
            1,
            Some(Invocation {
                id: InvocationId(1),
                origin,
            }),
        )
    }

    /// Where this command came from: a button or a timer here, or the surface.
    pub fn origin(&self) -> Origin {
        self.invocation.map_or(Origin::Local, |i| i.origin)
    }

    /// The invocation this command runs under, if it came through the surface.
    pub fn invocation(&self) -> Option<InvocationId> {
        self.invocation.map(|i| i.id)
    }

    /// Mark a part of the model changed, so `render` redraws it.
    pub fn changed(&mut self, part: &'static str) {
        self.changed.add(part);
    }

    pub fn then(&mut self, command: impl Into<A::Command>) {
        self.requests.push(Request::Then(command.into()));
    }

    /// Work off the UI thread, unkeyed: it runs beside anything else.
    pub fn spawn(&mut self, job: impl FnOnce(&Cancel) -> A::Command + Send + 'static) -> JobId {
        self.spawn_as(None, Concurrency::Parallel, job)
    }

    /// Work off the UI thread under a key, so two of a kind cannot run at once:
    /// `ReplacePrevious` cancels the earlier one and drops its late result; `RejectIfRunning`
    /// drops this one.
    pub fn spawn_keyed(
        &mut self,
        key: JobKey,
        mode: Concurrency,
        job: impl FnOnce(&Cancel) -> A::Command + Send + 'static,
    ) -> JobId {
        self.spawn_as(Some(key), mode, job)
    }

    fn spawn_as(
        &mut self,
        key: Option<JobKey>,
        mode: Concurrency,
        job: impl FnOnce(&Cancel) -> A::Command + Send + 'static,
    ) -> JobId {
        let id = JobId(self.next_job);
        self.next_job += 1;
        self.requests.push(Request::Spawn {
            id,
            key,
            mode,
            job: Box::new(job),
        });
        id
    }

    pub fn cancel(&mut self, job: JobId) {
        self.requests.push(Request::Cancel(job));
    }

    pub fn every(&mut self, id: TimerId, period: Duration, command: A::Command) {
        self.requests.push(Request::Every {
            id,
            period,
            command,
        });
    }

    pub fn after(&mut self, id: TimerId, delay: Duration, command: A::Command) {
        self.requests.push(Request::After { id, delay, command });
    }

    pub fn stop_timer(&mut self, id: TimerId) {
        self.requests.push(Request::StopTimer(id));
    }

    pub fn report(&mut self, what: &'static str, error: &dyn std::error::Error) {
        self.requests.push(Request::Report {
            what,
            error: error.to_string(),
        });
    }

    /// The invocation this command runs under has reached its outcome: the Mind is answered
    /// from the committed model, or with the refusal. Nothing happens for a command that did
    /// not come through the surface, or whose intent settles on return.
    pub fn settle(&mut self, result: Result<(), Refused>) {
        self.requests.push(Request::Settle(result));
    }

    /// Close a window. A `CloseRequested` flow ends here once any saving is done.
    pub fn close(&mut self, window: WindowId) {
        self.requests.push(Request::Close(window));
    }

    /// End the process, in order.
    pub fn exit(&mut self) {
        self.requests.push(Request::Exit);
    }

    /// What was asked, in order. For tests and the runtime.
    pub fn requests(&self) -> &[Request<A>] {
        &self.requests
    }

    pub fn changed_parts(&self) -> &Changed {
        &self.changed
    }

    pub(crate) fn into_parts(self) -> (Vec<Request<A>>, Changed, u64) {
        (self.requests, self.changed, self.next_job)
    }
}

impl<A: App> Request<A> {
    /// The request's name, for a test that asserts on what was asked.
    pub fn name(&self) -> &'static str {
        match self {
            Request::Then(_) => "then",
            Request::Spawn { .. } => "spawn",
            Request::Cancel(_) => "cancel",
            Request::Every { .. } => "every",
            Request::After { .. } => "after",
            Request::StopTimer(_) => "stop_timer",
            Request::Report { .. } => "report",
            Request::Settle(_) => "settle",
            Request::Close(_) => "close",
            Request::Exit => "exit",
        }
    }
}
