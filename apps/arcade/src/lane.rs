//! The library's commands, one at a time, in the order they were asked for.
//!
//! Every command to the library reads or writes the disk, so none runs on the UI thread. The
//! framework's jobs run on a pool, in any order: a `build` asked right after an `update_game`
//! could reach the library first and compile the spec that was just replaced. So `update` hands
//! each command to the lane in the order it applies them, and its job waits for the lane's reply.
//! The channel is the order; the lane's thread is the only one that ever touches the library.
//!
//! A command that panics ends that command, not the lane: its job finds no reply and says so.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc;

use crate::library::Library;

type Task = Box<dyn FnOnce(&Library) + Send>;

/// The line of commands, and the thread that works through it.
pub struct Lane {
    line: mpsc::Sender<Task>,
}

/// What one command will have come to, once the lane gets to it.
pub struct Reply<T>(mpsc::Receiver<T>);

impl<T> Reply<T> {
    /// Waits for the lane. `None` if the command panicked or the lane is gone.
    pub fn wait(self) -> Option<T> {
        self.0.recv().ok()
    }
}

impl Lane {
    pub fn start(library: Library) -> Lane {
        let (line, tasks) = mpsc::channel::<Task>();
        let started = std::thread::Builder::new().name("arcade-library".into()).spawn(move || {
            while let Ok(task) = tasks.recv() {
                if catch_unwind(AssertUnwindSafe(|| task(&library))).is_err() {
                    tracing::error!("a command to the Arcade library panicked");
                }
            }
        });
        if let Err(e) = started {
            tracing::error!(error = %e, "the Arcade library lane could not be started; every command will fail");
        }
        Lane { line }
    }

    /// Queue `command` behind everything asked before it. Called from `update`: it only sends.
    pub fn ask<T: Send + 'static>(&self, command: impl FnOnce(&Library) -> T + Send + 'static) -> Reply<T> {
        let (answer, reply) = mpsc::sync_channel(1);
        let task: Task = Box::new(move |library| {
            let _ = answer.send(command(library));
        });
        // A lane that never started leaves the task unqueued: its sender drops, and the reply is
        // `None`.
        let _ = self.line.send(task);
        Reply(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn library() -> Library {
        Library::at(std::env::temp_dir().join(format!("yantrik-arcade-lane-{}", std::process::id())))
    }

    #[test]
    fn commands_run_in_the_order_they_were_asked_whichever_thread_waits_first() {
        let lane = Lane::start(library());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let replies: Vec<_> = (0..20)
            .map(|n| {
                let seen = seen.clone();
                lane.ask(move |_| {
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
        let lane = Lane::start(library());
        let bad = lane.ask(|_| -> u8 { panic!("a stand-in fault") });
        let good = lane.ask(|_| 7u8);
        assert_eq!(bad.wait(), None);
        assert_eq!(good.wait(), Some(7));
    }
}
