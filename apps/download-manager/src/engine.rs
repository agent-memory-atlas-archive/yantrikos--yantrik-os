//! The download engine — what the buttons in this window actually do.
//!
//! Until now this app was a window with no machinery behind it: every callback logged a line and
//! returned, so the list was always empty and nothing ever reached disk. That is why it had no
//! control surface. `docs/app-control.md` is explicit that an app whose data is a stub should not
//! publish one, because a confident lie in JSON is worse than a screenshot a model has to guess
//! at. So the engine comes first and the surface follows it.
//!
//! # Shape
//!
//! One [`Engine`] holds every download and is cheap to clone — it is a handle onto shared state,
//! not the state itself. Each transfer runs on its own thread, writing to the file and to the
//! shared record as it goes; the UI thread never blocks on the network and never touches a
//! socket. Threads talk to the window only by bumping [`Engine::revision`], which the window
//! polls: a worker that tried to push into Slint would need a weak handle and an
//! `invoke_from_event_loop` per chunk, and would repaint the list sixty times a second for a fast
//! transfer. Polling a revision costs one atomic load per tick and repaints only on real change.
//!
//! # Stopping
//!
//! Pause, cancel and "the window is closing" are the same question — *should this thread still be
//! writing?* — so they share one [`Control`] flag per download, checked once per chunk. A paused
//! download keeps its partial file and its byte count, which is the entire reason resume can be
//! more than a restart.
//!
//! # Resume is a request, not a guarantee
//!
//! Resuming sends `Range: bytes=N-` and hopes. A server that honours it answers `206` and the
//! remaining bytes get appended. A server that does not answers `200` and the body it sends is the
//! *whole file* — appending that to a partial one produces a corrupt file that still looks
//! complete. So a `200` to a ranged request truncates and starts over, and `resume_supported`
//! records which answer came back so the window can stop promising something this server will not
//! do.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

/// Read size per loop turn. Also the granularity at which a pause is noticed, which is why it is
/// not larger: 64 KiB on a slow link is a fraction of a second of latency on the stop button.
const CHUNK: usize = 64 * 1024;

/// How often speed and ETA are recomputed. Shorter than this and the number jitters with every
/// TCP window; longer and it lags behind a transfer that has actually stalled.
const SPEED_WINDOW: Duration = Duration::from_millis(500);

// ── Status ──────────────────────────────────────────────────────────

/// Where one download is in its life.
///
/// The strings are the ones `download_manager.slint` already switches on; they are part of the
/// UI contract, not an implementation detail, and the control surface publishes the same words so
/// a caller and a person are reading the same vocabulary.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Queued,
    Downloading,
    Paused,
    Completed,
    Failed,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Queued => "queued",
            Status::Downloading => "downloading",
            Status::Paused => "paused",
            Status::Completed => "completed",
            Status::Failed => "failed",
        }
    }

    /// Whether this download is still going to move on its own.
    pub fn is_active(self) -> bool {
        matches!(self, Status::Queued | Status::Downloading)
    }
}

/// What a worker thread should do at its next chunk boundary.
mod control {
    pub const RUN: u8 = 0;
    pub const PAUSE: u8 = 1;
    pub const CANCEL: u8 = 2;
}

// ── One download ────────────────────────────────────────────────────

/// Everything known about one transfer.
///
/// Cloned out of the engine for reading, so neither the window nor the control surface holds the
/// lock while it formats a view.
#[derive(Clone, Debug)]
pub struct Download {
    pub id: i32,
    pub url: String,
    pub filename: String,
    pub save_dir: PathBuf,
    pub status: Status,
    pub downloaded: u64,
    /// `None` when the server never said — a chunked response has no length, and reporting a
    /// percentage for it would be inventing one.
    pub total: Option<u64>,
    pub speed_bps: f64,
    pub eta_secs: Option<u64>,
    pub checksum_expected: String,
    /// `none`, `verifying`, `pass`, `fail` — the words the UI switches on.
    pub checksum_status: String,
    pub file_hash: String,
    pub content_type: String,
    pub started_at: String,
    pub ended_at: String,
    pub error: String,
    pub resume_supported: bool,
    pub selected: bool,
}

impl Download {
    pub fn path(&self) -> PathBuf {
        self.save_dir.join(&self.filename)
    }

    /// 0.0–1.0, or 0.0 when the total is unknown.
    pub fn progress(&self) -> f32 {
        match self.total {
            Some(total) if total > 0 => (self.downloaded as f64 / total as f64).min(1.0) as f32,
            _ => 0.0,
        }
    }

    /// `"45.2 MB / 128.0 MB"`, or just what has arrived when the size is unknown.
    pub fn size_text(&self) -> String {
        match self.total {
            Some(total) => format!("{} / {}", format_bytes(self.downloaded), format_bytes(total)),
            None => format_bytes(self.downloaded),
        }
    }

    pub fn speed_text(&self) -> String {
        if self.status == Status::Downloading && self.speed_bps > 0.0 {
            format!("{}/s", format_bytes(self.speed_bps as u64))
        } else {
            String::new()
        }
    }

    pub fn eta_text(&self) -> String {
        match self.eta_secs {
            Some(secs) if self.status == Status::Downloading => format_duration(secs),
            _ => String::new(),
        }
    }
}

// ── The engine ──────────────────────────────────────────────────────

#[derive(Default)]
struct State {
    /// Insertion order is display order; there are never enough downloads for the linear scans
    /// this implies to matter, and a `Vec` keeps "newest last" free.
    items: Vec<Download>,
    controls: HashMap<i32, Arc<AtomicU8>>,
}

/// A handle onto every download in this window. Clone it freely — clones share one state.
#[derive(Clone)]
pub struct Engine {
    state: Arc<Mutex<State>>,
    next_id: Arc<AtomicI32>,
    revision: Arc<AtomicU64>,
    default_dir: PathBuf,
}

impl Engine {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(State::default())),
            next_id: Arc::new(AtomicI32::new(1)),
            revision: Arc::new(AtomicU64::new(0)),
            default_dir: default_download_dir(),
        }
    }

    /// Where downloads land when the caller does not choose.
    pub fn default_dir(&self) -> &Path {
        &self.default_dir
    }

    /// Bumped on every change. The window redraws when this moves and not otherwise.
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Relaxed)
    }

    fn touch(&self) {
        self.revision.fetch_add(1, Ordering::Relaxed);
    }

    /// Every download, in the order they were added.
    pub fn snapshot(&self) -> Vec<Download> {
        self.state.lock().map(|s| s.items.clone()).unwrap_or_default()
    }

    pub fn get(&self, id: i32) -> Option<Download> {
        let state = self.state.lock().ok()?;
        state.items.iter().find(|d| d.id == id).cloned()
    }

    /// Edit one download in place and bump the revision. `false` if there is no such id.
    fn edit(&self, id: i32, f: impl FnOnce(&mut Download)) -> bool {
        let found = match self.state.lock() {
            Ok(mut state) => match state.items.iter_mut().find(|d| d.id == id) {
                Some(item) => {
                    f(item);
                    true
                }
                None => false,
            },
            Err(_) => false,
        };
        if found {
            self.touch();
        }
        found
    }

    fn control_for(&self, id: i32) -> Option<Arc<AtomicU8>> {
        self.state.lock().ok()?.controls.get(&id).cloned()
    }

    /// Counts and combined speed, for the header line.
    pub fn totals(&self) -> Totals {
        let items = self.snapshot();
        Totals {
            active: items.iter().filter(|d| d.status.is_active()).count(),
            paused: items.iter().filter(|d| d.status == Status::Paused).count(),
            completed: items.iter().filter(|d| d.status == Status::Completed).count(),
            failed: items.iter().filter(|d| d.status == Status::Failed).count(),
            speed_bps: items
                .iter()
                .filter(|d| d.status == Status::Downloading)
                .map(|d| d.speed_bps)
                .sum(),
        }
    }

    // ── Commands ────────────────────────────────────────────────────

    /// Queue a URL and start fetching it. Returns the new id.
    ///
    /// Rejects anything that is not `http`/`https` rather than handing it to the transport and
    /// reporting whatever that says: a caller that mistyped a path deserves to be told it was a
    /// path, and a `file://` "download" would be a copy with none of this machinery's meaning.
    pub fn add(
        &self,
        url: &str,
        checksum: &str,
        save_dir: Option<&str>,
    ) -> Result<i32, String> {
        let url = url.trim();
        if url.is_empty() {
            return Err("no URL given".into());
        }
        let lower = url.to_ascii_lowercase();
        if !lower.starts_with("http://") && !lower.starts_with("https://") {
            return Err(format!("`{url}` is not an http(s) URL"));
        }

        let dir = match save_dir.map(str::trim).filter(|s| !s.is_empty()) {
            Some(d) => expand_home(d),
            None => self.default_dir.clone(),
        };
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot use {}: {e}", dir.display()))?;

        let filename = unique_in(&dir, &filename_from_url(url));
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);

        let download = Download {
            id,
            url: url.to_string(),
            filename,
            save_dir: dir,
            status: Status::Queued,
            downloaded: 0,
            total: None,
            speed_bps: 0.0,
            eta_secs: None,
            checksum_expected: checksum.trim().to_ascii_lowercase(),
            checksum_status: "none".into(),
            file_hash: String::new(),
            content_type: String::new(),
            started_at: now_stamp(),
            ended_at: String::new(),
            error: String::new(),
            resume_supported: false,
            selected: false,
        };

        {
            let mut state = self.state.lock().map_err(|_| "engine is poisoned".to_string())?;
            state.items.push(download);
            state.controls.insert(id, Arc::new(AtomicU8::new(control::RUN)));
        }
        self.touch();
        self.spawn(id, 0);
        Ok(id)
    }

    /// Stop writing at the next chunk, keeping the partial file.
    pub fn pause(&self, id: i32) -> Result<(), String> {
        let download = self.get(id).ok_or_else(|| format!("no download {id}"))?;
        if !download.status.is_active() {
            return Err(format!("download {id} is {}, not running", download.status.as_str()));
        }
        if let Some(ctl) = self.control_for(id) {
            ctl.store(control::PAUSE, Ordering::Relaxed);
        }
        Ok(())
    }

    /// Continue a paused or failed download from the bytes already on disk.
    pub fn resume(&self, id: i32) -> Result<(), String> {
        let download = self.get(id).ok_or_else(|| format!("no download {id}"))?;
        match download.status {
            Status::Paused | Status::Failed => {}
            other => return Err(format!("download {id} is {}, nothing to resume", other.as_str())),
        }
        let have = std::fs::metadata(download.path()).map(|m| m.len()).unwrap_or(0);
        if let Some(ctl) = self.control_for(id) {
            ctl.store(control::RUN, Ordering::Relaxed);
        }
        self.edit(id, |d| {
            d.status = Status::Queued;
            d.error.clear();
            d.downloaded = have;
        });
        self.spawn(id, have);
        Ok(())
    }

    /// Stop and discard the partial file.
    ///
    /// The record stays in the list as `failed` with the reason, because a download that vanished
    /// from the window tells the person nothing about why their file is not there.
    pub fn cancel(&self, id: i32) -> Result<(), String> {
        let download = self.get(id).ok_or_else(|| format!("no download {id}"))?;
        if let Some(ctl) = self.control_for(id) {
            ctl.store(control::CANCEL, Ordering::Relaxed);
        }
        // A running worker removes the file itself when it sees the flag; a paused or finished one
        // has no worker left to do it, so do it here.
        if !download.status.is_active() {
            let _ = std::fs::remove_file(download.path());
            self.edit(id, |d| {
                d.status = Status::Failed;
                d.error = "cancelled".into();
                d.ended_at = now_stamp();
                d.speed_bps = 0.0;
                d.eta_secs = None;
            });
        }
        Ok(())
    }

    /// Start a failed download again from nothing.
    pub fn retry(&self, id: i32) -> Result<(), String> {
        let download = self.get(id).ok_or_else(|| format!("no download {id}"))?;
        if download.status.is_active() {
            return Err(format!("download {id} is already running"));
        }
        let _ = std::fs::remove_file(download.path());
        if let Some(ctl) = self.control_for(id) {
            ctl.store(control::RUN, Ordering::Relaxed);
        }
        self.edit(id, |d| {
            d.status = Status::Queued;
            d.downloaded = 0;
            d.error.clear();
            d.file_hash.clear();
            d.checksum_status = "none".into();
            d.ended_at.clear();
            d.started_at = now_stamp();
        });
        self.spawn(id, 0);
        Ok(())
    }

    /// Hash the finished file again and compare it with the expected checksum.
    ///
    /// Worth having separately from the automatic check because the expected hash often arrives
    /// *after* the file — you download the ISO, then find the signature page.
    pub fn verify(&self, id: i32, expected: Option<&str>) -> Result<(), String> {
        let download = self.get(id).ok_or_else(|| format!("no download {id}"))?;
        if download.status != Status::Completed {
            return Err(format!(
                "download {id} is {}; there is nothing finished to verify",
                download.status.as_str()
            ));
        }
        if let Some(expected) = expected {
            let expected = expected.trim().to_ascii_lowercase();
            self.edit(id, |d| d.checksum_expected = expected);
        }
        self.edit(id, |d| d.checksum_status = "verifying".into());

        let engine = self.clone();
        std::thread::Builder::new()
            .name(format!("download-verify-{id}"))
            .spawn(move || engine.hash_and_compare(id))
            .map_err(|e| format!("cannot start verifier: {e}"))?;
        Ok(())
    }

    /// Drop finished downloads from the list. The files stay where they are.
    pub fn clear_completed(&self) -> usize {
        let removed = match self.state.lock() {
            Ok(mut state) => {
                let before = state.items.len();
                let doomed: Vec<i32> = state
                    .items
                    .iter()
                    .filter(|d| d.status == Status::Completed)
                    .map(|d| d.id)
                    .collect();
                state.items.retain(|d| d.status != Status::Completed);
                for id in doomed {
                    state.controls.remove(&id);
                }
                before - state.items.len()
            }
            Err(_) => 0,
        };
        if removed > 0 {
            self.touch();
        }
        removed
    }

    pub fn pause_all(&self) {
        for download in self.snapshot().iter().filter(|d| d.status.is_active()) {
            let _ = self.pause(download.id);
        }
    }

    pub fn resume_all(&self) {
        for download in self.snapshot().iter().filter(|d| d.status == Status::Paused) {
            let _ = self.resume(download.id);
        }
    }

    pub fn set_selected(&self, id: i32, selected: bool) {
        self.edit(id, |d| d.selected = selected);
    }

    pub fn select_all(&self, selected: bool) {
        if let Ok(mut state) = self.state.lock() {
            for item in state.items.iter_mut() {
                item.selected = selected;
            }
        }
        self.touch();
    }

    // ── The worker ──────────────────────────────────────────────────

    fn spawn(&self, id: i32, resume_from: u64) {
        let engine = self.clone();
        let started = std::thread::Builder::new()
            .name(format!("download-{id}"))
            .spawn(move || engine.transfer(id, resume_from));
        if let Err(e) = started {
            self.fail(id, format!("cannot start download thread: {e}"));
        }
    }

    fn fail(&self, id: i32, reason: impl Into<String>) {
        let reason = reason.into();
        self.edit(id, |d| {
            d.status = Status::Failed;
            d.error = reason;
            d.speed_bps = 0.0;
            d.eta_secs = None;
            d.ended_at = now_stamp();
        });
    }

    /// One transfer, start to finish, on its own thread.
    fn transfer(&self, id: i32, resume_from: u64) {
        let Some(download) = self.get(id) else { return };
        let Some(ctl) = self.control_for(id) else { return };
        let path = download.path();

        let mut request = ureq::get(&download.url);
        if resume_from > 0 {
            request = request.set("Range", &format!("bytes={resume_from}-"));
        }
        let response = match request.call() {
            Ok(response) => response,
            Err(ureq::Error::Status(code, _)) => {
                self.fail(id, format!("server said HTTP {code}"));
                return;
            }
            Err(e) => {
                self.fail(id, format!("{e}"));
                return;
            }
        };

        // A ranged request answered 200 means the body is the whole file, not the remainder.
        let appending = resume_from > 0 && response.status() == 206;
        let resume_supported = response
            .header("accept-ranges")
            .map(|v| v.to_ascii_lowercase().contains("bytes"))
            .unwrap_or(response.status() == 206);
        let content_type = response
            .header("content-type")
            .and_then(|v| v.split(';').next())
            .unwrap_or("")
            .trim()
            .to_string();
        let body_len = response.header("content-length").and_then(|v| v.parse::<u64>().ok());
        let start_at = if appending { resume_from } else { 0 };
        // `content-length` is the length of *this* body — the remainder when the server honoured
        // the range, the whole file when it did not. `start_at` is zero in the second case, so one
        // expression covers both.
        let total = body_len.map(|len| start_at + len);

        let file = if appending {
            OpenOptions::new().append(true).open(&path)
        } else {
            File::create(&path)
        };
        let mut file = match file {
            Ok(file) => file,
            Err(e) => {
                self.fail(id, format!("cannot write {}: {e}", path.display()));
                return;
            }
        };

        self.edit(id, |d| {
            d.status = Status::Downloading;
            d.downloaded = start_at;
            d.total = total;
            d.content_type = content_type;
            d.resume_supported = resume_supported;
            d.error.clear();
        });

        let mut reader = response.into_reader();
        let mut buffer = vec![0u8; CHUNK];
        let mut written = start_at;
        let mut window_start = Instant::now();
        let mut window_bytes = 0u64;

        loop {
            match ctl.load(Ordering::Relaxed) {
                control::PAUSE => {
                    let _ = file.flush();
                    self.edit(id, |d| {
                        d.status = Status::Paused;
                        d.speed_bps = 0.0;
                        d.eta_secs = None;
                    });
                    return;
                }
                control::CANCEL => {
                    drop(file);
                    let _ = std::fs::remove_file(&path);
                    self.edit(id, |d| {
                        d.status = Status::Failed;
                        d.error = "cancelled".into();
                        d.speed_bps = 0.0;
                        d.eta_secs = None;
                        d.ended_at = now_stamp();
                    });
                    return;
                }
                _ => {}
            }

            let read = match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => {
                    let _ = file.flush();
                    self.fail(id, format!("transfer broke: {e}"));
                    return;
                }
            };
            if let Err(e) = file.write_all(&buffer[..read]) {
                self.fail(id, format!("cannot write {}: {e}", path.display()));
                return;
            }
            written += read as u64;
            window_bytes += read as u64;

            let elapsed = window_start.elapsed();
            if elapsed >= SPEED_WINDOW {
                let speed = window_bytes as f64 / elapsed.as_secs_f64();
                let eta = total.and_then(|total| {
                    if speed > 0.0 && total > written {
                        Some(((total - written) as f64 / speed) as u64)
                    } else {
                        None
                    }
                });
                self.edit(id, |d| {
                    d.downloaded = written;
                    d.speed_bps = speed;
                    d.eta_secs = eta;
                });
                window_start = Instant::now();
                window_bytes = 0;
            }
        }

        if let Err(e) = file.flush() {
            self.fail(id, format!("cannot finish writing {}: {e}", path.display()));
            return;
        }
        drop(file);

        // A truncated transfer that ended cleanly is still a broken file, and saying "completed"
        // over it would be the confident lie the surface rules warn about.
        if let Some(total) = total {
            if written < total {
                self.fail(
                    id,
                    format!("ended early — {} of {}", format_bytes(written), format_bytes(total)),
                );
                return;
            }
        }

        self.edit(id, |d| {
            d.status = Status::Completed;
            d.downloaded = written;
            d.total = Some(written);
            d.speed_bps = 0.0;
            d.eta_secs = None;
            d.ended_at = now_stamp();
            if !d.checksum_expected.is_empty() {
                d.checksum_status = "verifying".into();
            }
        });
        self.hash_and_compare(id);
    }

    /// Hash the file on disk, record it, and compare it with the expected value if there is one.
    ///
    /// The hash is recorded whether or not anything was expected: an agent that fetched a file
    /// usually needs its digest next, and computing it here means it never has to shell out.
    fn hash_and_compare(&self, id: i32) {
        let Some(download) = self.get(id) else { return };
        let digest = match sha256_file(&download.path()) {
            Ok(digest) => digest,
            Err(e) => {
                self.edit(id, |d| {
                    d.checksum_status = if d.checksum_expected.is_empty() { "none".into() } else { "fail".into() };
                    d.error = format!("cannot hash the file: {e}");
                });
                return;
            }
        };
        self.edit(id, |d| {
            d.file_hash = digest.clone();
            d.checksum_status = if d.checksum_expected.is_empty() {
                "none".into()
            } else if d.checksum_expected == digest {
                "pass".into()
            } else {
                "fail".into()
            };
        });
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

/// The header line's numbers.
#[derive(Clone, Copy, Debug, Default)]
pub struct Totals {
    pub active: usize,
    pub paused: usize,
    pub completed: usize,
    pub failed: usize,
    pub speed_bps: f64,
}

// ── Plain functions ─────────────────────────────────────────────────

/// Which rows the window shows, in the order it shows them.
///
/// Kept out of the window because it is the one piece of display logic with edge cases worth
/// asserting — a filter and a search box that disagree, a sort over a size nobody reported — and a
/// function taking a slice can be tested where one reading Slint properties cannot.
///
/// `filter` is the tab: 0 all, 1 active, 2 completed, 3 failed. `sort` is 0 date, 1 name, 2 size,
/// 3 status, matching the numbers the UI already sends.
pub fn visible_rows(items: &[Download], filter: i32, query: &str, sort: i32) -> Vec<Download> {
    let query = query.trim().to_lowercase();
    let mut rows: Vec<Download> = items
        .iter()
        .filter(|d| match filter {
            1 => d.status.is_active() || d.status == Status::Paused,
            2 => d.status == Status::Completed,
            3 => d.status == Status::Failed,
            _ => true,
        })
        .filter(|d| {
            query.is_empty()
                || d.filename.to_lowercase().contains(&query)
                || d.url.to_lowercase().contains(&query)
        })
        .cloned()
        .collect();

    match sort {
        1 => rows.sort_by(|a, b| a.filename.to_lowercase().cmp(&b.filename.to_lowercase())),
        // Largest first: the reason to sort a download list by size is to find the big one.
        2 => rows.sort_by(|a, b| b.total.unwrap_or(b.downloaded).cmp(&a.total.unwrap_or(a.downloaded))),
        3 => rows.sort_by(|a, b| a.status.as_str().cmp(b.status.as_str())),
        // Newest first, which is what a downloads list is for.
        _ => rows.sort_by(|a, b| b.id.cmp(&a.id)),
    }
    rows
}

/// SHA-256 of a file, streamed so the size of the file does not become the size of a buffer.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; CHUNK];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Where a download goes when nobody says: `~/Downloads`, the same place every other desktop
/// puts it, so a file an agent fetched is where the person would look for it.
pub fn default_download_dir() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    Path::new(&home).join("Downloads")
}

/// `~/x` means what it does in a shell; anything else is taken as given.
pub fn expand_home(dir: &str) -> PathBuf {
    if let Some(rest) = dir.strip_prefix("~/").or_else(|| dir.strip_prefix("~\\")) {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| ".".to_string());
        Path::new(&home).join(rest)
    } else if dir == "~" {
        PathBuf::from(
            std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .unwrap_or_else(|_| ".".to_string()),
        )
    } else {
        PathBuf::from(dir)
    }
}

/// The name to save a URL under.
///
/// The last non-empty path segment, minus query and fragment, with anything that is not obviously
/// safe in a filename replaced. A URL that ends in a slash, or carries a name made entirely of
/// separators, gets `download` rather than an empty name or a dotfile.
pub fn filename_from_url(url: &str) -> String {
    let without_scheme = url.split("://").nth(1).unwrap_or(url);
    let path = without_scheme.split(['?', '#']).next().unwrap_or("");
    // Skip the host: a bare `https://example.com` must not save as `example.com`.
    let last = path.split('/').skip(1).filter(|s| !s.is_empty()).last().unwrap_or("");
    let cleaned: String = last
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+' | '(' | ')' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').to_string();
    if cleaned.is_empty() {
        "download".to_string()
    } else {
        cleaned
    }
}

/// A name that is not already taken in `dir`, by the `name (2).ext` convention.
///
/// Downloading over a file the person already has would be the one irreversible thing this app
/// could do by accident, so it does not.
pub fn unique_in(dir: &Path, name: &str) -> String {
    if !dir.join(name).exists() {
        return name.to_string();
    }
    let (stem, extension) = match name.rfind('.') {
        // A leading dot is the whole name of a dotfile, not a separator.
        Some(index) if index > 0 => (&name[..index], &name[index..]),
        _ => (name, ""),
    };
    for n in 1..10_000 {
        let candidate = format!("{stem} ({n}){extension}");
        if !dir.join(&candidate).exists() {
            return candidate;
        }
    }
    format!("{stem} ({}){extension}", std::process::id())
}

/// `1.4 KB`, `45.2 MB` — one decimal, because two is noise at these magnitudes.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// `45s`, `2m 30s`, `1h 05m` — as long as it is useful and no longer.
pub fn format_duration(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60)
    }
}

fn now_stamp() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_a_file_after_the_last_path_segment() {
        assert_eq!(filename_from_url("https://example.com/dist/yantrik.iso"), "yantrik.iso");
        assert_eq!(filename_from_url("https://example.com/a/b/c.tar.gz?sig=1#x"), "c.tar.gz");
    }

    #[test]
    fn never_saves_a_file_under_the_host_name() {
        // A bare host has no path segment to take; `example.com` would look like a real filename
        // and be wrong every time.
        assert_eq!(filename_from_url("https://example.com"), "download");
        assert_eq!(filename_from_url("https://example.com/"), "download");
    }

    #[test]
    fn strips_what_cannot_go_in_a_filename() {
        assert_eq!(filename_from_url("https://x.dev/a:b*c?q=1"), "a_b_c");
        assert_eq!(filename_from_url("https://x.dev/%2e%2e/etc/passwd"), "passwd");
        assert_eq!(filename_from_url("https://x.dev/..."), "download");
    }

    #[test]
    fn sidesteps_a_name_already_on_disk() {
        let dir = std::env::temp_dir().join(format!("yantrik-dl-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(unique_in(&dir, "a.txt"), "a.txt");

        File::create(dir.join("a.txt")).unwrap();
        assert_eq!(unique_in(&dir, "a.txt"), "a (1).txt");

        File::create(dir.join("a (1).txt")).unwrap();
        assert_eq!(unique_in(&dir, "a.txt"), "a (2).txt");

        File::create(dir.join("noext")).unwrap();
        assert_eq!(unique_in(&dir, "noext"), "noext (1)");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn refuses_what_is_not_a_web_url() {
        let engine = Engine::new();
        assert!(engine.add("", "", None).is_err());
        assert!(engine.add("/etc/passwd", "", None).unwrap_err().contains("not an http"));
        assert!(engine.add("file:///etc/passwd", "", None).is_err());
        assert!(engine.add("ftp://example.com/x", "", None).is_err());
    }

    #[test]
    fn names_the_download_a_command_could_not_find() {
        let engine = Engine::new();
        assert!(engine.pause(99).unwrap_err().contains("no download 99"));
        assert!(engine.resume(99).is_err());
        assert!(engine.cancel(99).is_err());
        assert!(engine.retry(99).is_err());
        assert!(engine.verify(99, None).is_err());
    }

    #[test]
    fn formats_sizes_and_times_the_way_the_window_shows_them() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(999), "999 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1024 * 1024 * 3 / 2), "1.5 MB");
        assert_eq!(format_duration(45), "45s");
        assert_eq!(format_duration(150), "2m 30s");
        assert_eq!(format_duration(3960), "1h 06m");
    }

    #[test]
    fn reports_no_percentage_when_the_server_gave_no_length() {
        let mut download = Download {
            id: 1,
            url: "https://x.dev/a".into(),
            filename: "a".into(),
            save_dir: PathBuf::from("/tmp"),
            status: Status::Downloading,
            downloaded: 4096,
            total: None,
            speed_bps: 1024.0,
            eta_secs: None,
            checksum_expected: String::new(),
            checksum_status: "none".into(),
            file_hash: String::new(),
            content_type: String::new(),
            started_at: String::new(),
            ended_at: String::new(),
            error: String::new(),
            resume_supported: false,
            selected: false,
        };
        assert_eq!(download.progress(), 0.0);
        assert_eq!(download.size_text(), "4.0 KB");
        assert_eq!(download.eta_text(), "");

        download.total = Some(8192);
        assert_eq!(download.progress(), 0.5);
        assert_eq!(download.size_text(), "4.0 KB / 8.0 KB");
    }

    fn row(id: i32, filename: &str, status: Status, total: Option<u64>) -> Download {
        Download {
            id,
            url: format!("https://example.com/{filename}"),
            filename: filename.into(),
            save_dir: PathBuf::from("/tmp"),
            status,
            downloaded: total.unwrap_or(0),
            total,
            speed_bps: 0.0,
            eta_secs: None,
            checksum_expected: String::new(),
            checksum_status: "none".into(),
            file_hash: String::new(),
            content_type: String::new(),
            started_at: String::new(),
            ended_at: String::new(),
            error: String::new(),
            resume_supported: false,
            selected: false,
        }
    }

    fn names(rows: &[Download]) -> Vec<&str> {
        rows.iter().map(|d| d.filename.as_str()).collect()
    }

    #[test]
    fn shows_newest_first_and_filters_by_tab() {
        let items = vec![
            row(1, "old.iso", Status::Completed, Some(10)),
            row(2, "running.zip", Status::Downloading, Some(20)),
            row(3, "broken.tar", Status::Failed, None),
            row(4, "held.bin", Status::Paused, Some(5)),
        ];
        assert_eq!(names(&visible_rows(&items, 0, "", 0)), ["held.bin", "broken.tar", "running.zip", "old.iso"]);
        // "Active" includes paused: a download the person stopped is still one they are dealing
        // with, and hiding it there is how a paused transfer gets forgotten.
        assert_eq!(names(&visible_rows(&items, 1, "", 0)), ["held.bin", "running.zip"]);
        assert_eq!(names(&visible_rows(&items, 2, "", 0)), ["old.iso"]);
        assert_eq!(names(&visible_rows(&items, 3, "", 0)), ["broken.tar"]);
    }

    #[test]
    fn searches_the_name_and_the_url_together() {
        let items = vec![
            row(1, "yantrik.iso", Status::Completed, Some(10)),
            row(2, "notes.zip", Status::Completed, Some(20)),
        ];
        assert_eq!(names(&visible_rows(&items, 0, "YANTRIK", 0)), ["yantrik.iso"]);
        assert_eq!(names(&visible_rows(&items, 0, "example.com", 0)).len(), 2);
        assert!(visible_rows(&items, 0, "nothing like it", 0).is_empty());
    }

    #[test]
    fn sorts_by_name_size_and_status() {
        let items = vec![
            row(1, "b.iso", Status::Downloading, Some(10)),
            row(2, "a.iso", Status::Completed, Some(900)),
            row(3, "c.iso", Status::Failed, None),
        ];
        assert_eq!(names(&visible_rows(&items, 0, "", 1)), ["a.iso", "b.iso", "c.iso"]);
        // The one with no reported size sorts last rather than being dropped or crashing.
        assert_eq!(names(&visible_rows(&items, 0, "", 2)), ["a.iso", "b.iso", "c.iso"]);
        assert_eq!(names(&visible_rows(&items, 0, "", 3)), ["a.iso", "b.iso", "c.iso"]);
    }

    #[test]
    fn hashes_a_file_the_way_sha256sum_does() {
        let path = std::env::temp_dir().join(format!("yantrik-dl-hash-{}", std::process::id()));
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(
            sha256_file(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        std::fs::remove_file(&path).ok();
    }
}
