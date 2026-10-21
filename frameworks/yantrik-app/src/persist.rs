//! The ordered persistence lane (design/app-framework-2026-10-21.md, §6).
//!
//! Every durable write an app asks for (its settings, a draft) goes through one lane, off the
//! UI thread, serialised per destination and tagged with the revision it carries. An older
//! write never lands after a newer one: when a newer revision for the same key is already
//! waiting, the older is skipped. What came of each write returns to the core as a
//! [`Written`], and the core applies it by revision, so an old outcome never replaces newer
//! state. Workers never write these files themselves.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use yantrik_foundation::json_store::write_atomically;

/// What is being written: a settings file, a draft by name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PersistKey {
    Settings,
    Draft(String),
}

/// One write, as the lane is given it.
pub struct Write {
    pub key: PersistKey,
    pub revision: u64,
    pub path: PathBuf,
    pub bytes: Vec<u8>,
}

/// What came of one write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    pub key: PersistKey,
    pub revision: u64,
    pub result: Result<(), String>,
}

/// Where outcomes go: the core's queue in the runner, a channel in a test.
pub trait Report: Send + Sync {
    fn written(&self, written: Written);
}

/// The lane as the core drives it. [`Threaded`] in the runner; [`ManualLane`] in a test.
pub trait Lane: Send {
    fn write(&mut self, write: Write);
    /// Writes not yet done.
    fn waiting(&self) -> usize;
    /// Waits up to `grace` for the lane to drain. Says whether it did.
    fn drain(&self, grace: Duration) -> bool;
}

/// The writes still queued, newest revision per key winning.
#[derive(Default)]
struct Queue {
    writes: Vec<Write>,
    newest: HashMap<PersistKey, u64>,
}

impl Queue {
    fn push(&mut self, write: Write) {
        let newest = self.newest.entry(write.key.clone()).or_insert(0);
        if write.revision > *newest {
            *newest = write.revision;
        }
        self.writes.push(write);
    }

    /// The oldest write still worth doing; stale ones are skipped, reported as superseded.
    fn pop(&mut self, report: &dyn Report) -> Option<Write> {
        while !self.writes.is_empty() {
            let write = self.writes.remove(0);
            let newest = self.newest.get(&write.key).copied().unwrap_or(0);
            if write.revision < newest {
                report.written(Written { key: write.key, revision: write.revision, result: Err("superseded".into()) });
                continue;
            }
            return Some(write);
        }
        None
    }
}

fn perform(write: Write, report: &dyn Report) {
    let result = write_atomically(&write.path, &write.bytes).map_err(|e| e.to_string());
    report.written(Written { key: write.key, revision: write.revision, result });
}

/// One thread per app, named `<id>-persist`.
pub struct Threaded {
    queue: Arc<Mutex<Queue>>,
    sender: mpsc::Sender<()>,
    /// Writes taken from the queue and not yet finished. What waits is the queue's length plus
    /// this; a write the queue skips as superseded is never counted, so the count cannot leak.
    in_flight: Arc<AtomicUsize>,
}

impl Threaded {
    pub fn new(name: &str, report: Arc<dyn Report>) -> Threaded {
        let queue = Arc::new(Mutex::new(Queue::default()));
        let in_flight = Arc::new(AtomicUsize::new(0));
        let (sender, receiver) = mpsc::channel::<()>();
        let q = queue.clone();
        let busy = in_flight.clone();
        std::thread::Builder::new()
            .name(format!("{name}-persist"))
            .spawn(move || {
                while receiver.recv().is_ok() {
                    loop {
                        // Taken and counted under the one lock, so `waiting` never reads zero
                        // between the pop and the count.
                        let next = {
                            let mut queue = q.lock().unwrap_or_else(|p| p.into_inner());
                            let next = queue.pop(report.as_ref());
                            if next.is_some() {
                                busy.fetch_add(1, Ordering::AcqRel);
                            }
                            next
                        };
                        let Some(write) = next else { break };
                        perform(write, report.as_ref());
                        busy.fetch_sub(1, Ordering::AcqRel);
                    }
                }
            })
            .expect("the persist thread");
        Threaded { queue, sender, in_flight }
    }
}

impl Lane for Threaded {
    fn write(&mut self, write: Write) {
        self.queue.lock().unwrap_or_else(|p| p.into_inner()).push(write);
        let _ = self.sender.send(());
    }

    fn waiting(&self) -> usize {
        self.queue.lock().unwrap_or_else(|p| p.into_inner()).writes.len()
            + self.in_flight.load(Ordering::Acquire)
    }

    fn drain(&self, grace: Duration) -> bool {
        let deadline = Instant::now() + grace;
        while self.waiting() > 0 {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        true
    }
}

/// Writes only when a test says so, in order, on the calling thread.
pub struct ManualLane {
    queue: Arc<Mutex<Queue>>,
    report: Arc<dyn Report>,
}

impl ManualLane {
    pub fn new(report: Arc<dyn Report>) -> ManualLane {
        ManualLane { queue: Arc::new(Mutex::new(Queue::default())), report }
    }

    /// Queues a write through a shared handle (the queue is behind a lock).
    pub fn push(&self, write: Write) {
        self.queue.lock().unwrap_or_else(|p| p.into_inner()).push(write);
    }

    /// Does the oldest write still worth doing. Says whether there was one.
    pub fn write_one(&self) -> bool {
        let next = self.queue.lock().unwrap_or_else(|p| p.into_inner()).pop(self.report.as_ref());
        match next {
            Some(write) => {
                perform(write, self.report.as_ref());
                true
            }
            None => false,
        }
    }

    pub fn write_all(&self) {
        while self.write_one() {}
    }
}

impl Lane for ManualLane {
    fn write(&mut self, write: Write) {
        self.push(write);
    }

    fn waiting(&self) -> usize {
        self.queue.lock().unwrap_or_else(|p| p.into_inner()).writes.len()
    }

    /// A test's shutdown does the writes it owes, as the thread would.
    fn drain(&self, _grace: Duration) -> bool {
        self.write_all();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Log(Mutex<Vec<Written>>);
    impl Report for Log {
        fn written(&self, w: Written) {
            self.0.lock().unwrap().push(w);
        }
    }

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("yantrik-persist-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn writes_land_in_order_and_a_stale_one_is_skipped() {
        let log = Arc::new(Log(Mutex::new(Vec::new())));
        let path = dir("order").join("s.json");
        let mut lane = ManualLane::new(log.clone());
        lane.write(Write { key: PersistKey::Settings, revision: 1, path: path.clone(), bytes: b"one".to_vec() });
        lane.write(Write { key: PersistKey::Settings, revision: 2, path: path.clone(), bytes: b"two".to_vec() });
        lane.write(Write { key: PersistKey::Draft("a".into()), revision: 1, path: path.with_extension("draft"), bytes: b"d".to_vec() });
        lane.write_all();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two");
        let outcomes: Vec<(u64, bool)> = log.0.lock().unwrap().iter().map(|w| (w.revision, w.result.is_ok())).collect();
        assert_eq!(outcomes, vec![(1, false), (2, true), (1, true)], "revision 1 of the settings was superseded, not written");
    }

    #[test]
    fn the_thread_writes_and_drains() {
        let log = Arc::new(Log(Mutex::new(Vec::new())));
        let path = dir("thread").join("s.json");
        let mut lane = Threaded::new("test", log.clone());
        lane.write(Write { key: PersistKey::Settings, revision: 1, path: path.clone(), bytes: b"x".to_vec() });
        assert!(lane.drain(Duration::from_secs(5)));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "x");
        assert_eq!(log.0.lock().unwrap().len(), 1);
        // Fifty saves of one key in a burst: most are superseded, and none of them is left
        // counted as waiting, so the drain ends when the last one lands, not at its deadline.
        for revision in 2..=51 {
            lane.write(Write { key: PersistKey::Settings, revision, path: path.clone(), bytes: revision.to_string().into_bytes() });
        }
        let started = Instant::now();
        assert!(lane.drain(Duration::from_secs(5)));
        assert!(started.elapsed() < Duration::from_secs(2), "drained when done, not at the deadline");
        assert_eq!(lane.waiting(), 0);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "51");
    }
}
