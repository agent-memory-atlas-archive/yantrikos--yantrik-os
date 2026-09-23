//! A job's output as the view keeps it: the first half of the budget, the last half, and a count
//! of what fell between.
//!
//! The design's cap for one call's output (decision 2, "a call's retained output is at most 2 MiB,
//! and past that the card keeps the head and the tail with a marker saying how much was
//! dropped"), applied to the bytes of a command's terminal. The head says how it started — the
//! command line, the first error — and the tail says how it ended; a build log's middle is what a
//! person scrolls past.

use std::collections::VecDeque;

/// The line the view shows where output was dropped.
///
/// Starts with CAN (0x18), which aborts an escape sequence the head may have been cut in the middle
/// of, then resets the attributes, so the marker is readable whatever state the cut left the
/// emulator in.
pub fn dropped_marker(dropped: u64) -> String {
    format!("\x18\x1b[0m\r\n[… {dropped} bytes of output dropped here; the first and last parts are kept …]\r\n")
}

pub(crate) struct Retained {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    head_cap: usize,
    tail_cap: usize,
    dropped: u64,
    total: u64,
}

impl Retained {
    /// Keep at most `budget` bytes: half from the start, half from the end.
    pub(crate) fn new(budget: usize) -> Self {
        let head_cap = budget / 2;
        Retained {
            head: Vec::new(),
            tail: VecDeque::new(),
            head_cap,
            tail_cap: budget - head_cap,
            dropped: 0,
            total: 0,
        }
    }

    pub(crate) fn push(&mut self, bytes: &[u8]) {
        self.total += bytes.len() as u64;
        let take = (self.head_cap - self.head.len()).min(bytes.len());
        self.head.extend_from_slice(&bytes[..take]);
        let rest = &bytes[take..];
        if rest.is_empty() {
            return;
        }
        if rest.len() >= self.tail_cap {
            self.dropped += (self.tail.len() + rest.len() - self.tail_cap) as u64;
            self.tail.clear();
            self.tail.extend(&rest[rest.len() - self.tail_cap..]);
            return;
        }
        let over = (self.tail.len() + rest.len()).saturating_sub(self.tail_cap);
        self.tail.drain(..over);
        self.dropped += over as u64;
        self.tail.extend(rest);
    }

    /// Bytes that were written and are no longer kept.
    pub(crate) fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Every byte the command ever wrote, kept or not.
    pub(crate) fn total(&self) -> u64 {
        self.total
    }

    /// What is kept, in order, with the marker where the middle went.
    pub(crate) fn bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.head.len() + self.tail.len() + 96);
        out.extend_from_slice(&self.head);
        if self.dropped > 0 {
            out.extend_from_slice(dropped_marker(self.dropped).as_bytes());
        }
        out.extend(self.tail.iter());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: usize = 1024 * 1024;

    #[test]
    fn output_under_the_budget_is_kept_whole_and_unmarked() {
        let mut kept = Retained::new(2 * MIB);
        kept.push(b"hello ");
        kept.push(b"world");
        assert_eq!(kept.bytes(), b"hello world");
        assert_eq!((kept.dropped(), kept.total()), (0, 11));
    }

    #[test]
    fn past_the_budget_the_head_and_the_tail_stay_and_the_middle_is_counted() {
        let mut kept = Retained::new(2 * MIB);
        // Three MiB in uneven pieces, each byte saying where it was, so a wrong splice shows.
        let all: Vec<u8> = (0..3 * MIB).map(|i| (i / 4096 % 251) as u8).collect();
        for piece in all.chunks(7919) {
            kept.push(piece);
        }
        assert_eq!(kept.total(), (3 * MIB) as u64);
        assert_eq!(kept.dropped(), MIB as u64, "a third of it fell out of the middle");

        let bytes = kept.bytes();
        let marker = dropped_marker(MIB as u64);
        assert_eq!(bytes.len(), 2 * MIB + marker.len());
        assert_eq!(&bytes[..MIB], &all[..MIB], "the head is the first MiB");
        assert_eq!(&bytes[MIB..MIB + marker.len()], marker.as_bytes());
        assert_eq!(&bytes[MIB + marker.len()..], &all[2 * MIB..], "the tail is the last MiB");
        assert!(marker.contains("1048576 bytes"), "the marker says how much: {marker}");
    }

    #[test]
    fn one_write_larger_than_the_whole_budget_keeps_its_own_end() {
        let mut kept = Retained::new(10);
        kept.push(b"0123456789abcdefghij");
        // Head 0..5, tail the last five, ten dropped between.
        assert_eq!(kept.dropped(), 10);
        let bytes = kept.bytes();
        assert!(bytes.starts_with(b"01234"));
        assert!(bytes.ends_with(b"fghij"));
    }
}
