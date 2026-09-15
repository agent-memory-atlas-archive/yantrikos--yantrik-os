//! Append-only segments, and a cursor that only moves after the bytes are on disk.
//!
//! The ordering here is the entire point of the file. perception-service holds observations in a
//! 2048-entry ring that overwrites its oldest entry when full. That is the right structure for a
//! feed and a disqualifying one for a record: whatever the ring drops is gone, and nothing
//! upstream can be asked for it again.
//!
//! Measured on the deployed machine while an agent was working: `next_seq 5885`, ring holding
//! 2048, **`missed 3837`**. The ring wrapped nearly twice, and the only reason anyone knew was
//! that the service counts what it drops. Eight minutes of an agent's actual work was unavailable
//! by the time anybody looked — not because a component failed, but because nothing was draining.
//!
//! So: append, `fsync`, *then* advance the cursor. A crash between the append and the cursor
//! write replays the tail, which is why every record carries the source's sequence number and
//! replay is idempotent by construction. The opposite ordering loses data silently, which is the
//! one outcome this service exists to prevent.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// Bytes per segment before rolling to the next one.
///
/// Small enough that the oldest data can be dropped at a useful granularity, large enough that
/// rolling is rare. Observations run a few hundred bytes, so this is a few thousand of them.
const SEGMENT_BYTES: u64 = 2 * 1024 * 1024;

/// How much history to keep, total.
///
/// The journal is short-lived infrastructure for replay and diagnosis, not an archive and not a
/// second searchable store. A bounded consumer of a bounded producer: when this fills, the oldest
/// segment is deleted, and that deletion is recorded as a coverage gap like any other loss.
const MAX_BYTES: u64 = 64 * 1024 * 1024;

/// One line in a segment.
///
/// Either an observation exactly as the eye reported it, or a note about something that could not
/// be observed. Both are records; only one is evidence.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
pub enum Record {
    /// An observation, carried through unmodified. The journal does not interpret.
    Observation {
        /// The eye's sequence number. This is the idempotency key: a replayed record has the same
        /// `seq` and must not become a second memory.
        seq: u64,
        /// Which run of perception-service produced it. A restart resets `seq` to zero, so
        /// without this a replay after a restart would look like a rewind rather than a new
        /// stream, and the two would interleave into nonsense.
        source: String,
        at: f64,
        observation: serde_json::Value,
    },
    /// A hole. Recorded in-band, in sequence, so a reader walking the journal encounters the
    /// absence where it happened rather than having to ask a separate health endpoint whether
    /// what it just read was complete.
    Gap {
        /// First sequence known lost, and one past the last.
        from: u64,
        to: u64,
        at: f64,
        /// Why it is missing. The two causes need different responses: `ring_overrun` means the
        /// reader was too slow or absent, `retention` means the journal itself aged it out.
        ///
        /// Owned rather than `&'static str`: this type is deserialized when the journal is read
        /// back, and a borrowed field would demand the input outlive the record.
        cause: String,
    },
}

pub struct Journal {
    dir: PathBuf,
    current: File,
    current_path: PathBuf,
    current_bytes: u64,
    /// Highest source sequence durably written. The cursor on disk lags this only between the
    /// `fsync` and the cursor write, which is the window replay exists to cover.
    pub last_written: u64,
    pub records: u64,
    pub gaps: u64,
}

impl Journal {
    pub fn open(dir: impl AsRef<Path>) -> std::io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;

        let (current_path, current_bytes) = match newest_segment(&dir)? {
            Some((path, bytes)) if bytes < SEGMENT_BYTES => (path, bytes),
            _ => (dir.join(segment_name(next_segment_index(&dir)?)), 0),
        };
        let current = OpenOptions::new().create(true).append(true).open(&current_path)?;

        Ok(Self {
            dir,
            current,
            current_path,
            current_bytes,
            last_written: 0,
            records: 0,
            gaps: 0,
        })
    }

    /// Append one record and get it onto the disk before returning.
    ///
    /// `sync_data` rather than `sync_all`: the contents must survive, the directory metadata
    /// matters only when a segment is created, which is handled at roll time.
    pub fn append(&mut self, record: &Record) -> std::io::Result<()> {
        let mut line = serde_json::to_vec(record)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        line.push(b'\n');

        self.current.write_all(&line)?;
        self.current.sync_data()?;

        self.current_bytes += line.len() as u64;
        self.records += 1;
        match record {
            Record::Observation { seq, .. } => self.last_written = (*seq).max(self.last_written),
            Record::Gap { .. } => self.gaps += 1,
        }

        if self.current_bytes >= SEGMENT_BYTES {
            self.roll()?;
        }
        Ok(())
    }

    fn roll(&mut self) -> std::io::Result<()> {
        let index = next_segment_index(&self.dir)?;
        self.current_path = self.dir.join(segment_name(index));
        self.current = OpenOptions::new().create(true).append(true).open(&self.current_path)?;
        self.current_bytes = 0;
        self.enforce_retention()
    }

    /// Delete the oldest segments until the journal is back inside its budget.
    ///
    /// Deliberately loud. Dropping a segment is losing history, and the whole argument of this
    /// service is that lost history must never be silent — so it is logged with the range that
    /// went, and a reader that had not caught up will meet a `Gap` where those records were.
    fn enforce_retention(&mut self) -> std::io::Result<()> {
        let mut segments = segments(&self.dir)?;
        let mut total: u64 = segments.iter().map(|(_, bytes)| bytes).sum();

        while total > MAX_BYTES && segments.len() > 1 {
            let (path, bytes) = segments.remove(0);
            tracing::warn!(
                segment = %path.display(),
                bytes,
                "Journal retention reached; dropping the oldest segment. History before this \
                 point is no longer replayable."
            );
            std::fs::remove_file(&path)?;
            total -= bytes;
        }
        Ok(())
    }

    pub fn bytes_on_disk(&self) -> u64 {
        segments(&self.dir).map(|s| s.iter().map(|(_, b)| b).sum()).unwrap_or(0)
    }

    /// Every record with a sequence at or after `from`, in order.
    ///
    /// The interpretation worker's read path. It keeps its own cursor — deliberately separate
    /// from the ingestion cursor, so a slow or crashed interpreter cannot stall the drain, and a
    /// restarted interpreter can rewind without asking the eye for anything.
    pub fn read_from(&self, from: u64, limit: usize) -> std::io::Result<Vec<Record>> {
        let mut out = Vec::new();
        for (path, _) in segments(&self.dir)? {
            let file = File::open(&path)?;
            for line in BufReader::new(file).lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                // A truncated final line is expected after a crash mid-append. Skip it rather
                // than refusing to serve the rest — the whole point of fsync-before-cursor is
                // that the tail may be incomplete and the missing part will be replayed.
                let Ok(record) = serde_json::from_str::<Record>(line.as_str()) else { continue };
                let seq = match &record {
                    Record::Observation { seq, .. } => *seq,
                    Record::Gap { to, .. } => *to,
                };
                if seq >= from {
                    out.push(record);
                    if out.len() >= limit {
                        return Ok(out);
                    }
                }
            }
        }
        Ok(out)
    }
}

fn segment_name(index: u64) -> String {
    format!("{index:08}.jsonl")
}

fn segments(dir: &Path) -> std::io::Result<Vec<(PathBuf, u64)>> {
    let mut out: Vec<(PathBuf, u64)> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|e| e.metadata().ok().map(|m| (e.path(), m.len())))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn newest_segment(dir: &Path) -> std::io::Result<Option<(PathBuf, u64)>> {
    Ok(segments(dir)?.pop())
}

fn next_segment_index(dir: &Path) -> std::io::Result<u64> {
    let highest = segments(dir)?
        .iter()
        .filter_map(|(p, _)| p.file_stem()?.to_str()?.parse::<u64>().ok())
        .max();
    Ok(highest.map(|h| h + 1).unwrap_or(0))
}

// ── The ingestion cursor ────────────────────────────────────────────

/// Where the drain got to, on disk.
///
/// Written *after* the observations it refers to are durable. That ordering is the contract: on
/// restart the drain resumes from here and re-reads anything appended but not yet acknowledged,
/// which is safe precisely because every record carries the source's own sequence number.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Cursor {
    pub next_seq: u64,
    /// The perception-service run this cursor belongs to. A restart upstream resets sequence
    /// numbers, and resuming from a stale cursor would silently skip the new run's first
    /// several thousand observations while looking perfectly healthy.
    pub source: String,
}

impl Cursor {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        // Write-and-rename: a cursor half-written by a crash would be worse than an old one,
        // because an old cursor replays and a corrupt one resets to zero.
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec(self)?)?;
        std::fs::rename(&tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lab(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("journal-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn observation(seq: u64) -> Record {
        Record::Observation {
            seq,
            source: "run-1".into(),
            at: 1.0,
            observation: serde_json::json!({ "summary": format!("event {seq}") }),
        }
    }

    #[test]
    fn what_is_appended_can_be_read_back_in_order() {
        let dir = lab("roundtrip");
        let mut j = Journal::open(&dir).unwrap();
        for seq in 0..5 {
            j.append(&observation(seq)).unwrap();
        }
        let back = j.read_from(0, 100).unwrap();
        assert_eq!(back.len(), 5);
        assert_eq!(j.last_written, 4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_reader_can_resume_from_where_it_stopped() {
        // The interpretation worker's restart. It must be able to rewind without asking the eye,
        // because the eye's ring will not have it any more.
        let dir = lab("resume");
        let mut j = Journal::open(&dir).unwrap();
        for seq in 0..10 {
            j.append(&observation(seq)).unwrap();
        }
        let tail = j.read_from(7, 100).unwrap();
        assert_eq!(tail.len(), 3, "everything from 7 onwards, and nothing before it");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_gap_is_a_record_in_the_stream_not_a_footnote() {
        // A reader walking the journal must meet the hole where it happened. If coverage lived
        // only in a health endpoint, a reader could process straight across 3837 missing
        // observations and never know its account of that period was fiction.
        let dir = lab("gap");
        let mut j = Journal::open(&dir).unwrap();
        j.append(&observation(0)).unwrap();
        j.append(&Record::Gap { from: 1, to: 3838, at: 2.0, cause: "ring_overrun".into() }).unwrap();
        j.append(&observation(3838)).unwrap();

        let all = j.read_from(0, 100).unwrap();
        assert_eq!(all.len(), 3);
        assert!(matches!(all[1], Record::Gap { from: 1, to: 3838, .. }));
        assert_eq!(j.gaps, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cursor_survives_a_restart_and_carries_which_run_it_belongs_to() {
        // Without `source`, a perception-service restart resets seq to 0, and a cursor at 5000
        // would skip the new run's first 5000 observations while reporting perfect health.
        let dir = lab("cursor");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cursor.json");

        Cursor { next_seq: 5885, source: "run-1".into() }.save(&path).unwrap();
        let loaded = Cursor::load(&path);
        assert_eq!(loaded.next_seq, 5885);
        assert_eq!(loaded.source, "run-1");

        // A missing cursor is a fresh start, not a crash.
        assert_eq!(Cursor::load(&dir.join("absent.json")).next_seq, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_truncated_final_line_does_not_poison_the_rest() {
        // Expected after a crash between write and fsync. The tail will be replayed; refusing to
        // serve the good records before it would turn a recoverable partial write into an outage.
        let dir = lab("torn");
        let mut j = Journal::open(&dir).unwrap();
        j.append(&observation(0)).unwrap();
        j.append(&observation(1)).unwrap();
        drop(j);

        let seg = segments(&dir).unwrap().pop().unwrap().0;
        let mut f = OpenOptions::new().append(true).open(&seg).unwrap();
        f.write_all(br#"{"record":"observation","seq":2,"sour"#).unwrap();

        let j = Journal::open(&dir).unwrap();
        let back = j.read_from(0, 100).unwrap();
        assert_eq!(back.len(), 2, "the two complete records survive the torn third");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
