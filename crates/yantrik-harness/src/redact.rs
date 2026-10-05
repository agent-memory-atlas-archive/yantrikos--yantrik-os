//! Finding forgotten words by their digest (the `redact` event).
//!
//! When the person asks a mind to forget something and answers *Erase* to its Keep/Erase
//! question, the mind erases its own memory. The shell keeps copies of its own — the agent's pane
//! transcript and the run store — and a forget that left those would not be one. So the mind tells
//! the shell what to erase, without ever sending the words: each needle travels as the SHA-256 of
//! its NFC form and its length in Unicode scalar values, and the shell slides a window of that
//! length over its own text, hashing as it goes.
//!
//! This module is that matching, and nothing else: no store, no rule about who may ask. The
//! acceptance rule is the run store's (`run_store`, `RunStore::redact`), the host's
//! (`Host`'s `redact` event) and the shell's transcript (`yantrik-ui`'s agent store).
//!
//! # How text is matched
//!
//! - Case-sensitive, over NFC. The text is normalised before windows are taken, so "café" typed
//!   composed and "café" stored decomposed are the same words. A piece of text with no match is
//!   handed back untouched; one with a match comes back in NFC, with the marker where the words
//!   were.
//! - **Pieces are joined before matching.** A reply arrives as chunks, and a name split across two
//!   of them ("Pri" + "ya") is still a name. [`redact_pieces`] matches over the joined pieces and
//!   hands back the same number of pieces, so a store can write each one back where it was: the
//!   piece the match began in carries the marker, and the rest of the match is taken out of the
//!   pieces it ran into.
//! - Left to right, longest needle first at each place, and a match is never matched again.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use unicode_normalization::char::canonical_combining_class;
use unicode_normalization::UnicodeNormalization;

/// What is left where the words were.
pub const MARKER: &str = "[erased at your request]";

/// The most needles one `redact` may carry.
pub const MAX_NEEDLES: usize = 16;

/// The longest one needle may be, in Unicode scalar values.
pub const MAX_NEEDLE_CHARS: usize = 4096;

/// Words to erase, as their digest: never the words themselves.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Needle {
    /// SHA-256 of the needle's NFC form as UTF-8, as 64 lowercase hex digits.
    pub sha256: String,
    /// Its length in Unicode scalar values, after NFC.
    pub len: usize,
}

impl Needle {
    /// The needle for `text`, as a harness computes it before sending. Here for tests and for the
    /// shell's own callers; a mind computes its own (`turn.redact` in the Python library).
    pub fn of(text: &str) -> Needle {
        let nfc: String = text.nfc().collect();
        Needle { sha256: digest_hex(&nfc), len: nfc.chars().count() }
    }

    fn bytes(&self) -> Option<[u8; 32]> {
        if self.sha256.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, pair) in self.sha256.as_bytes().chunks(2).enumerate() {
            let hex = std::str::from_utf8(pair).ok()?;
            out[i] = u8::from_str_radix(hex, 16).ok()?;
        }
        Some(out)
    }
}

/// Whether `needles` may be acted on: between one and [`MAX_NEEDLES`] of them, each a 64-digit
/// lowercase hex digest and a length from 1 to [`MAX_NEEDLE_CHARS`]. The error says which is wrong
/// and never repeats a digest.
pub fn validate(needles: &[Needle]) -> Result<(), String> {
    if needles.is_empty() {
        return Err("a `redact` needs at least one needle".to_string());
    }
    if needles.len() > MAX_NEEDLES {
        return Err(format!("a `redact` carries at most {MAX_NEEDLES} needles, not {}", needles.len()));
    }
    for (i, needle) in needles.iter().enumerate() {
        let lower_hex = needle.sha256.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if needle.sha256.len() != 64 || !lower_hex {
            return Err(format!("needle {i}: `sha256` must be 64 lowercase hex digits"));
        }
        if !(1..=MAX_NEEDLE_CHARS).contains(&needle.len) {
            return Err(format!("needle {i}: `len` must be from 1 to {MAX_NEEDLE_CHARS}"));
        }
    }
    Ok(())
}

/// Erase every needle from one text. `None` when nothing matched, so the caller leaves it alone.
pub fn redact(text: &str, needles: &[Needle]) -> Option<(String, usize)> {
    let (mut pieces, n) = redact_pieces(&[text], needles)?;
    Some((pieces.remove(0), n))
}

/// Erase every needle from `pieces`, matched as one text (see the module docs). `None` when
/// nothing matched. Otherwise the same number of pieces, and how many places were erased; a piece
/// no match reached is handed back as it was, less any combining characters it began with (those
/// go with the piece before, whose character they belong to).
pub fn redact_pieces(pieces: &[&str], needles: &[Needle]) -> Option<(Vec<String>, usize)> {
    let digests: HashSet<[u8; 32]> = needles.iter().filter_map(Needle::bytes).collect();
    let mut lengths: Vec<usize> = needles.iter().map(|n| n.len).filter(|&l| l > 0).collect();
    lengths.sort_unstable_by(|a, b| b.cmp(a));
    lengths.dedup();
    if digests.is_empty() || lengths.is_empty() {
        return None;
    }

    // Each piece normalised on its own, after moving any characters at its start that compose
    // with what came before (combining marks, Hangul vowel and final jamo) into the piece before:
    // so normalising the pieces one by one is normalising the whole, and a match can still be
    // written back piece by piece.
    let balanced = rebalance(pieces);
    let mut chars: Vec<char> = Vec::new();
    let mut owner: Vec<usize> = Vec::new();
    for (i, piece) in balanced.iter().enumerate() {
        for c in piece.nfc() {
            chars.push(c);
            owner.push(i);
        }
    }

    let mut matches: Vec<(usize, usize)> = Vec::new();
    let mut at = 0;
    let mut window = String::new();
    'scan: while at < chars.len() {
        for &len in &lengths {
            if at + len > chars.len() {
                continue;
            }
            window.clear();
            window.extend(chars[at..at + len].iter().copied().nfc());
            let digest: [u8; 32] = Sha256::digest(window.as_bytes()).into();
            if digests.contains(&digest) {
                matches.push((at, at + len));
                at += len;
                continue 'scan;
            }
        }
        at += 1;
    }
    if matches.is_empty() {
        return None;
    }

    let mut touched = vec![false; balanced.len()];
    for &(start, end) in &matches {
        for &piece in &owner[start..end] {
            touched[piece] = true;
        }
    }
    let mut out: Vec<String> = vec![String::new(); balanced.len()];
    let mut next = matches.iter().peekable();
    let mut i = 0;
    while i < chars.len() {
        let piece = owner[i];
        if let Some(&&(start, end)) = next.peek() {
            if i == start {
                out[piece].push_str(MARKER);
                i = end;
                next.next();
                continue;
            }
        }
        if touched[piece] {
            out[piece].push(chars[i]);
        }
        i += 1;
    }
    for (i, piece) in balanced.iter().enumerate() {
        if !touched[i] {
            out[i] = piece.clone();
        }
    }
    Some((out, matches.len()))
}

/// Erase every needle from each string inside a JSON value, in place: object values and array
/// items, never object keys. How many places were erased.
pub fn redact_json(value: &mut serde_json::Value, needles: &[Needle]) -> usize {
    match value {
        serde_json::Value::String(s) => match redact(s, needles) {
            Some((text, n)) => {
                *s = text;
                n
            }
            None => 0,
        },
        serde_json::Value::Array(items) => items.iter_mut().map(|v| redact_json(v, needles)).sum(),
        serde_json::Value::Object(map) => map.values_mut().map(|v| redact_json(v, needles)).sum(),
        _ => 0,
    }
}

fn digest_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// Whether `c` can compose with the character before it, so a piece must not begin with it.
fn joins_previous(c: char) -> bool {
    canonical_combining_class(c) != 0 || ('\u{1161}'..='\u{11C2}').contains(&c)
}

/// The pieces, with whatever begins each one and composes with the piece before moved into that
/// piece. The text, joined, is unchanged.
fn rebalance(pieces: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = pieces.iter().map(|p| p.to_string()).collect();
    for i in 1..out.len() {
        let cut = out[i].char_indices().find(|&(_, c)| !joins_previous(c)).map(|(at, _)| at).unwrap_or(out[i].len());
        if cut > 0 {
            let head: String = out[i].drain(..cut).collect();
            // Into the nearest piece before that has anything to compose with.
            let into = (0..i).rev().find(|&j| !out[j].is_empty()).unwrap_or(i - 1);
            out[into].push_str(&head);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_needle_is_found_and_marked_and_nothing_else_moves() {
        let (text, n) = redact("My sister Priya lives in Pune. Priya!", &[Needle::of("Priya")]).unwrap();
        assert_eq!(n, 2);
        assert_eq!(text, format!("My sister {MARKER} lives in Pune. {MARKER}!"));
        assert_eq!(redact("nobody named here", &[Needle::of("Priya")]), None);
    }

    #[test]
    fn matching_is_case_sensitive() {
        assert_eq!(redact("priya", &[Needle::of("Priya")]), None);
    }

    #[test]
    fn composed_and_decomposed_are_the_same_words() {
        let composed = "caf\u{e9}";
        let decomposed = "cafe\u{301}";
        assert_eq!(Needle::of(composed), Needle::of(decomposed));
        assert_eq!(Needle::of(composed).len, 4);
        let (text, n) = redact(&format!("at the {decomposed} on Elm"), &[Needle::of(composed)]).unwrap();
        assert_eq!((text.as_str(), n), (format!("at the {MARKER} on Elm").as_str(), 1));
        let (text, _) = redact(&format!("at the {composed}"), &[Needle::of(decomposed)]).unwrap();
        assert_eq!(text, format!("at the {MARKER}"));
    }

    #[test]
    fn a_needle_split_across_pieces_is_found_and_written_back_piece_by_piece() {
        let (pieces, n) = redact_pieces(&["Her name is Pri", "ya, and she", " lives here."], &[Needle::of("Priya")]).unwrap();
        assert_eq!(n, 1);
        assert_eq!(pieces, vec![format!("Her name is {MARKER}"), ", and she".to_string(), " lives here.".to_string()]);
        assert_eq!(pieces.concat(), format!("Her name is {MARKER}, and she lives here."));
    }

    #[test]
    fn a_combining_mark_at_the_start_of_a_piece_still_composes() {
        let (pieces, n) = redact_pieces(&["the cafe", "\u{301} closed"], &[Needle::of("caf\u{e9}")]).unwrap();
        assert_eq!(n, 1);
        assert_eq!(pieces.concat(), format!("the {MARKER} closed"));
        assert_eq!(pieces.len(), 2);
    }

    #[test]
    fn the_longest_needle_wins_where_two_begin() {
        let needles = [Needle::of("12 Elm"), Needle::of("12 Elm Street")];
        let (text, n) = redact("at 12 Elm Street now", &needles).unwrap();
        assert_eq!((text, n), (format!("at {MARKER} now"), 1));
    }

    #[test]
    fn strings_inside_json_are_erased_and_keys_are_not() {
        let mut v = serde_json::json!({"Priya": "call Priya", "n": 3, "list": ["Priya", "x"]});
        assert_eq!(redact_json(&mut v, &[Needle::of("Priya")]), 2);
        assert_eq!(v, serde_json::json!({"Priya": format!("call {MARKER}"), "n": 3, "list": [MARKER, "x"]}));
    }

    #[test]
    fn needles_are_checked_before_anything_is_touched() {
        assert!(validate(&[Needle::of("x")]).is_ok());
        assert!(validate(&[]).is_err());
        assert!(validate(&vec![Needle::of("x"); MAX_NEEDLES + 1]).is_err());
        let upper = Needle { sha256: Needle::of("x").sha256.to_uppercase(), len: 1 };
        assert!(validate(&[upper]).is_err());
        assert!(validate(&[Needle { sha256: "ab".into(), len: 1 }]).is_err());
        assert!(validate(&[Needle { sha256: Needle::of("x").sha256, len: 0 }]).is_err());
        assert!(validate(&[Needle { sha256: Needle::of("x").sha256, len: MAX_NEEDLE_CHARS + 1 }]).is_err());
        // The refusal never repeats the digest it refused.
        let bad = Needle { sha256: format!("{}zz", &Needle::of("x").sha256[..62]), len: 1 };
        assert!(!validate(&[bad.clone()]).unwrap_err().contains(&bad.sha256));
    }
}
