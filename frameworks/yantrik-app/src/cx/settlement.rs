//! Settlements: the one answer an explicit intent is owed (design/app-framework-2026-10-21.md,
//! §3.1, settlement). Part of [`Core`]; here so the core's own file stays readable.
//!
//! An invocation opened with [`Core::open_settlement`] is answered exactly once: by `fx.settle`,
//! by its last job landing (the reply read after the result committed), by that job panicking,
//! by it being replaced, by the deadline (the waiter times out on its own), or by shutdown.

use std::rc::Rc;
use std::sync::mpsc;

use serde_json::Value;

use super::Core;
use crate::app::App;
use crate::intents::InvocationId;

/// An invocation whose answer is still owed (`Settle::Explicit`).
pub(super) struct Open<A: App> {
    pub(super) answer: mpsc::SyncSender<Result<Value, String>>,
    pub(super) reply: Rc<dyn Fn(&A::Model) -> Value>,
    /// Jobs started under it and not yet landed. At zero with no settle yet, it settles itself.
    pub(super) jobs: usize,
}

impl<A: App> Core<A> {
    /// One fewer job open under `id`. Says whether that was the last one, and the invocation is
    /// still owed its answer.
    pub(super) fn job_landed(&self, id: InvocationId) -> bool {
        let mut open = self.open.borrow_mut();
        match open.get_mut(&id) {
            Some(o) => {
                o.jobs = o.jobs.saturating_sub(1);
                o.jobs == 0
            }
            None => false,
        }
    }

    /// Opens an explicit settlement: the Mind is answered through `answer` when the invocation
    /// reaches its outcome, with `reply` read from the model then.
    pub fn open_settlement(
        &self,
        answer: mpsc::SyncSender<Result<Value, String>>,
        reply: Rc<dyn Fn(&A::Model) -> Value>,
    ) -> InvocationId {
        let id = InvocationId(self.next_invocation.get());
        self.next_invocation.set(id.0 + 1);
        self.open.borrow_mut().insert(
            id,
            Open {
                answer,
                reply,
                jobs: 0,
            },
        );
        id
    }

    /// Forgets a settlement whose first command was refused: the refusal is the answer.
    pub fn drop_settlement(&self, id: InvocationId) {
        self.open.borrow_mut().remove(&id);
    }

    /// Settles `id` with the reply from the committed model if nothing is still running under it.
    pub(super) fn settle_if_idle(&self, id: InvocationId) {
        let idle = self.open.borrow().get(&id).is_some_and(|o| o.jobs == 0);
        if idle {
            self.settle(id, Ok(()));
        }
    }

    /// The one terminal outcome of an invocation. A second settle is nothing.
    pub(super) fn settle(&self, id: InvocationId, result: Result<(), String>) {
        let Some(open) = self.open.borrow_mut().remove(&id) else {
            return;
        };
        let answer = result.map(|()| (open.reply)(&self.model.borrow()));
        // The waiter is gone only if it timed out; the outcome is then already a refusal.
        let _ = open.answer.try_send(answer);
    }

    /// Invocations still owed their answer.
    pub fn settlements_open(&self) -> usize {
        self.open.borrow().len()
    }
}
