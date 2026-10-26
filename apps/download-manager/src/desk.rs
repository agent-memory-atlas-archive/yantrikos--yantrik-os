//! The engine's commands, one at a time, in the order they were asked for.
//!
//! Every command to the engine reads or writes the disk (the list is saved on each transition, a
//! resume measures the partial file, a cancel removes it), so none runs on the UI thread. The
//! framework's jobs run on a pool, in any order: a pause pressed and then a resume could reach the
//! engine as the resume and then the pause. So `update` hands each command to the desk, in the
//! order it applies them, and the job waits for the desk's reply. The channel is the order; the
//! thread is the only one that ever calls the engine's commands.
//!
//! A command that panics ends that command, not the desk: its job finds no reply and says so.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc;

use crate::engine::Engine;

type Task = Box<dyn FnOnce(&Engine) + Send>;

/// The line of commands, and the thread that works through it.
pub struct Desk {
    line: mpsc::Sender<Task>,
}

/// What one command will have come to, once the desk gets to it.
pub struct Reply<T>(mpsc::Receiver<T>);

impl<T> Reply<T> {
    /// Waits for the desk. `None` if the command panicked or the desk is gone.
    pub fn wait(self) -> Option<T> {
        self.0.recv().ok()
    }
}

impl Desk {
    pub fn start(engine: Engine) -> Desk {
        let (line, tasks) = mpsc::channel::<Task>();
        let started = std::thread::Builder::new().name("download-desk".into()).spawn(move || {
            while let Ok(task) = tasks.recv() {
                if catch_unwind(AssertUnwindSafe(|| task(&engine))).is_err() {
                    tracing::error!("a command to the download engine panicked");
                }
            }
        });
        if let Err(e) = started {
            tracing::error!(error = %e, "the download desk could not be started; every command will fail");
        }
        Desk { line }
    }

    /// Queue `command` behind everything asked before it. Called from `update`: it only sends.
    pub fn ask<T: Send + 'static>(&self, command: impl FnOnce(&Engine) -> T + Send + 'static) -> Reply<T> {
        let (answer, reply) = mpsc::sync_channel(1);
        let task: Task = Box::new(move |engine| {
            let _ = answer.send(command(engine));
        });
        // A desk that never started leaves the task unqueued: its sender drops, and the reply is
        // `None`.
        let _ = self.line.send(task);
        Reply(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn engine() -> Engine {
        let dir = std::env::temp_dir().join(format!("yantrik-dl-desk-{}", std::process::id()));
        Engine::open_in(dir.join("state"), dir.join("files"))
    }

    #[test]
    fn commands_run_in_the_order_they_were_asked_whichever_thread_waits_first() {
        let desk = Desk::start(engine());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let replies: Vec<_> = (0..20)
            .map(|n| {
                let seen = seen.clone();
                desk.ask(move |_| {
                    std::thread::sleep(std::time::Duration::from_millis(if n % 2 == 0 { 3 } else { 0 }));
                    seen.lock().unwrap().push(n);
                    n
                })
            })
            .collect();
        // Waited on last-first, as a pool might run the jobs.
        let got: Vec<_> = replies.into_iter().rev().map(|r| r.wait()).collect();
        assert_eq!(got.len(), 20);
        assert_eq!(*seen.lock().unwrap(), (0..20).collect::<Vec<_>>());
    }

    #[test]
    fn a_command_that_panics_costs_only_itself() {
        let desk = Desk::start(engine());
        let bad = desk.ask(|_| -> u8 { panic!("a stand-in fault") });
        let good = desk.ask(|_| 7u8);
        assert_eq!(bad.wait(), None);
        assert_eq!(good.wait(), Some(7));
    }
}
