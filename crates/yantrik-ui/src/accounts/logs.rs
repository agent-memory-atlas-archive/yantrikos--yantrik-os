//! Reading a vendor program's own session logs, a little at a time.
//!
//! Claude Code and Codex each write a JSON line per event into files under their directory, and
//! those lines carry what the panel counts: tokens, and — for Codex — the plan's own meters. The
//! files grow to tens of megabytes, so they are read the way `tail -f` reads: each file from where
//! the last read stopped, whole lines only, and only files written since the day began.
//!
//! Bounded on every side: how deep a directory is walked, how many files are looked at, how long a
//! line may be before it is skipped unread. A line is only handed on when it contains a word the
//! caller names, so the tool results and file contents that make up most of these logs are never
//! parsed at all.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// How deep under a vendor's log directory files are looked for.
pub const DEPTH: usize = 5;
/// The most files one read looks at.
pub const MOST_FILES: usize = 400;
/// A line longer than this is a tool result or a pasted file, never a count: skipped unparsed.
pub const LONGEST_LINE: usize = 256 * 1024;

/// Every `.jsonl` under `root`, at most `DEPTH` down, written at or after `since`, newest first.
/// Never through a link.
pub fn written_since(root: &Path, since: SystemTime) -> Vec<(PathBuf, SystemTime)> {
    let mut out = Vec::new();
    walk(root, since, DEPTH, &mut out);
    out.sort_by(|a, b| b.1.cmp(&a.1));
    out.truncate(MOST_FILES);
    out
}

fn walk(dir: &Path, since: SystemTime, depth: usize, out: &mut Vec<(PathBuf, SystemTime)>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let Ok(ft) = e.file_type() else { continue };
        let path = e.path();
        if ft.is_dir() {
            if depth > 0 {
                walk(&path, since, depth - 1, out);
            }
        } else if ft.is_file() && path.extension().is_some_and(|x| x == "jsonl") {
            if let Ok(m) = e.metadata().and_then(|m| m.modified()) {
                if m >= since {
                    out.push((path, m));
                }
            }
            if out.len() >= MOST_FILES * 4 {
                return;
            }
        }
    }
}

/// Where each file was read to.
#[derive(Default)]
pub struct Tails {
    at: HashMap<PathBuf, u64>,
}

impl Tails {
    /// Hand every whole line written to `path` since the last call, that contains `needle`, to
    /// `each`. A file that is shorter than where it was read to was replaced: read from the start.
    pub fn read(&mut self, path: &Path, needle: &str, mut each: impl FnMut(&str)) {
        let Ok(mut f) = std::fs::File::open(path) else { return };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        let mut at = self.at.get(path).copied().unwrap_or(0);
        if at > len {
            at = 0;
        }
        if at == len || f.seek(SeekFrom::Start(at)).is_err() {
            self.at.insert(path.to_path_buf(), at);
            return;
        }
        let mut reader = BufReader::with_capacity(64 * 1024, f);
        let mut line = Vec::new();
        loop {
            line.clear();
            let n = match read_line_bounded(&mut reader, &mut line) {
                Ok(n) => n,
                Err(_) => break,
            };
            if n == 0 {
                break;
            }
            if line.last() != Some(&b'\n') {
                // A line still being written: read it whole next time.
                break;
            }
            at += n as u64;
            if line.len() <= LONGEST_LINE {
                if let Ok(text) = std::str::from_utf8(&line) {
                    if text.contains(needle) {
                        each(text.trim_end());
                    }
                }
            }
        }
        self.at.insert(path.to_path_buf(), at);
    }

    pub fn clear(&mut self) {
        self.at.clear();
    }
}

/// One line, keeping at most `LONGEST_LINE + 1` bytes of it but consuming all of it. Answers with
/// how many bytes were consumed; a line that was cut short is longer than `LONGEST_LINE`.
fn read_line_bounded(reader: &mut impl BufRead, buf: &mut Vec<u8>) -> std::io::Result<usize> {
    let mut consumed = 0;
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            return Ok(consumed);
        }
        let (take, done) = match chunk.iter().position(|&b| b == b'\n') {
            Some(i) => (i + 1, true),
            None => (chunk.len(), false),
        };
        let room = (LONGEST_LINE + 1).saturating_sub(buf.len());
        buf.extend_from_slice(&chunk[..take.min(room)]);
        if done && take > room {
            // Keep the newline, so the caller still sees a finished line (one it will skip).
            buf.push(b'\n');
        }
        reader.consume(take);
        consumed += take;
        if done {
            return Ok(consumed);
        }
    }
}

/// An RFC 3339 time, as Unix seconds.
pub fn unix_of(ts: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(ts).ok().map(|t| t.timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("yantrik-logs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_file_is_read_from_where_it_stopped_whole_lines_only() {
        let d = tmp("tail");
        let p = d.join("s.jsonl");
        std::fs::write(&p, "{\"usage\":1}\n{\"other\":2}\n{\"usage\":3").unwrap();
        let mut t = Tails::default();
        let mut got = Vec::new();
        t.read(&p, "usage", |l| got.push(l.to_string()));
        assert_eq!(got, ["{\"usage\":1}"], "the unfinished line waits");
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(b"}\n{\"usage\":4}\n").unwrap();
        got.clear();
        t.read(&p, "usage", |l| got.push(l.to_string()));
        assert_eq!(got, ["{\"usage\":3}", "{\"usage\":4}"]);
        got.clear();
        t.read(&p, "usage", |l| got.push(l.to_string()));
        assert!(got.is_empty(), "nothing is read twice");
        // Replaced by a shorter file: read again from the start.
        std::fs::write(&p, "{\"usage\":9}\n").unwrap();
        t.read(&p, "usage", |l| got.push(l.to_string()));
        assert_eq!(got, ["{\"usage\":9}"]);
    }

    #[test]
    fn a_huge_line_is_consumed_and_skipped_and_the_next_one_still_read() {
        let d = tmp("huge");
        let p = d.join("s.jsonl");
        let big = format!("{{\"usage\":\"{}\"}}\n", "x".repeat(LONGEST_LINE * 2));
        std::fs::write(&p, format!("{big}{{\"usage\":2}}\n")).unwrap();
        let mut got = Vec::new();
        Tails::default().read(&p, "usage", |l| got.push(l.to_string()));
        assert_eq!(got, ["{\"usage\":2}"]);
    }

    #[test]
    fn only_logs_written_since_are_found_and_never_through_a_link() {
        let d = tmp("walk");
        std::fs::create_dir_all(d.join("a/b")).unwrap();
        std::fs::write(d.join("a/b/new.jsonl"), "x\n").unwrap();
        std::fs::write(d.join("a/notes.txt"), "x\n").unwrap();
        let later = SystemTime::now() + std::time::Duration::from_secs(3600);
        assert_eq!(written_since(&d, SystemTime::UNIX_EPOCH).len(), 1);
        assert!(written_since(&d, later).is_empty());
        #[cfg(unix)]
        {
            let outside = tmp("walk-outside");
            std::fs::write(outside.join("secret.jsonl"), "x\n").unwrap();
            std::os::unix::fs::symlink(&outside, d.join("link")).unwrap();
            assert_eq!(written_since(&d, SystemTime::UNIX_EPOCH).len(), 1, "the link is not followed");
        }
    }
}
