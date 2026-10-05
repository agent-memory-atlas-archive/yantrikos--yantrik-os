//! Finding forgotten words by their digest (the `redact` event).
//!
//! When the person asks a mind to forget something and answers *Erase* to its Keep/Erase
//! question, the mind erases its own memory. The shell keeps copies of its own — the agent's pane
//! transcript and the run store — and a forget that left those would not be one. So the mind tells
//! the shell what to erase, without ever sending the words: each needle travels as the SHA-256 of
//! its canonical form and its length in Unicode scalar values, and the shell slides a window of
//! that length over the canonical form of its own text, hashing as it goes.
//!
//! This module is that matching, and nothing else: no store, no rule about who may ask. The
//! acceptance rule is the run store's (`run_store`, `RunStore::redact`), the host's
//! (`Host`'s `redact` event) and the shell's transcript (`yantrik-ui`'s agent store).
//!
//! # The canonical form
//!
//! NFC first, then Unicode default lowercasing (`str::to_lowercase`; in Python
//! `unicodedata.normalize('NFC', t).lower()`), as UTF-8. The needle's `sha256` is over that, and
//! its `len` is the canonical form's length in scalar values — after lowercasing, which can change
//! it ('İ' U+0130 lowercases to 'i' + U+0307, two scalars). The shell puts each text it holds in
//! the same form before taking windows. Nothing else is folded: 'ß' is not 'ss'.
//!
//! # How text is matched
//!
//! - Case-insensitive and over NFC, as above: "Priya", "PRIYA" and "priya" are one needle, and so
//!   are "café" typed composed and stored decomposed.
//! - **What is replaced is the original.** Every scalar of the canonical form knows the bytes of
//!   the stored text it came from, and a match replaces the stored span that produced it. Where a
//!   window begins or ends inside what one original character became (the 'i' of a lowercased
//!   'İ', the 'é' two stored characters composed into), the span widens to the whole of it.
//!   Everything outside the spans is handed back byte for byte: its case, its normalisation.
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
use unicode_normalization::{is_nfc_quick, IsNormalized, UnicodeNormalization};

/// What is left where the words were.
pub const MARKER: &str = "[erased at your request]";

/// The most needles one `redact` may carry.
pub const MAX_NEEDLES: usize = 16;

/// The longest one needle may be, in Unicode scalar values.
pub const MAX_NEEDLE_CHARS: usize = 4096;

/// Words to erase, as their digest: never the words themselves.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Needle {
    /// SHA-256 of the needle's canonical form ([`canonical`]) as UTF-8, as 64 lowercase hex digits.
    pub sha256: String,
    /// The canonical form's length in Unicode scalar values (after lowercasing).
    pub len: usize,
}

impl Needle {
    /// The needle for `text`, as a harness computes it before sending. Here for tests and for the
    /// shell's own callers; a mind computes its own (`turn.redact` in the Python library).
    pub fn of(text: &str) -> Needle {
        let canon = canonical(text);
        Needle { sha256: digest_hex(&canon), len: canon.chars().count() }
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

/// The form needles are hashed in and text is matched in: NFC, then Unicode default lowercasing.
pub fn canonical(text: &str) -> String {
    text.nfc().collect::<String>().to_lowercase()
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
/// no match reached is handed back exactly as it was.
pub fn redact_pieces(pieces: &[&str], needles: &[Needle]) -> Option<(Vec<String>, usize)> {
    let digests: HashSet<[u8; 32]> = needles.iter().filter_map(Needle::bytes).collect();
    let mut lengths: Vec<usize> = needles.iter().map(|n| n.len).filter(|&l| l > 0).collect();
    lengths.sort_unstable_by(|a, b| b.cmp(a));
    lengths.dedup();
    if digests.is_empty() || lengths.is_empty() {
        return None;
    }

    let joined: String = pieces.concat();
    let canon = Canonical::of(&joined);
    let count = canon.origin.len();

    // The matches, as byte spans of `joined`: sorted, never overlapping.
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut at = 0;
    'scan: while at < count {
        for &len in &lengths {
            if at + len > count {
                continue;
            }
            let window = &canon.text.as_bytes()[canon.offsets[at]..canon.offsets[at + len]];
            let digest: [u8; 32] = Sha256::digest(window).into();
            if digests.contains(&digest) {
                let span = (canon.origin[at].0, canon.origin[at + len - 1].1);
                spans.push(span);
                // On past everything the span took, including the rest of a character it widened
                // into: what is erased is not matched again.
                at += len;
                while at < count && canon.origin[at].0 < span.1 {
                    at += 1;
                }
                continue 'scan;
            }
        }
        at += 1;
    }
    if spans.is_empty() {
        return None;
    }

    let mut out: Vec<String> = Vec::with_capacity(pieces.len());
    let mut first = 0; // the first span that may still reach the current piece
    let mut start = 0;
    for piece in pieces {
        let end = start + piece.len();
        while first < spans.len() && spans[first].1 <= start {
            first += 1;
        }
        let mut kept = String::new();
        let mut cursor = start;
        for &(from, to) in spans[first..].iter().take_while(|&&(from, _)| from < end) {
            if from >= start {
                kept.push_str(&joined[cursor..from]);
                kept.push_str(MARKER);
            }
            cursor = cursor.max(to.min(end));
        }
        kept.push_str(&joined[cursor..end]);
        out.push(kept);
        start = end;
    }
    Some((out, spans.len()))
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

/// A text in canonical form, each scalar knowing where in the original it came from.
struct Canonical {
    /// `canonical(original)`, exactly.
    text: String,
    /// The byte offset in `text` of each scalar, and `text.len()` after the last.
    offsets: Vec<usize>,
    /// For each scalar, the byte range of the original it came from: the original character, or
    /// all the original characters NFC made it from.
    origin: Vec<(usize, usize)>,
}

impl Canonical {
    fn of(original: &str) -> Canonical {
        // NFC, one segment at a time: a segment begins at a character nothing before it can
        // compose or reorder with, so NFC of the segments one by one is NFC of the whole. Where a
        // segment comes out as it went in, each character is its own origin; where NFC changed it,
        // every character it became comes from the whole segment.
        let mut nfc = String::with_capacity(original.len());
        let mut nfc_origin: Vec<(usize, usize)> = Vec::with_capacity(original.len());
        for (from, to) in segments(original) {
            let segment = &original[from..to];
            let before = nfc.len();
            nfc.extend(segment.nfc());
            if &nfc[before..] == segment {
                nfc_origin.extend(segment.char_indices().map(|(i, c)| (from + i, from + i + c.len_utf8())));
            } else {
                let made = nfc[before..].chars().count();
                nfc_origin.extend(std::iter::repeat((from, to)).take(made));
            }
        }

        // Then lowercase the whole (so a final sigma is decided as `str::to_lowercase` decides
        // it), and give each lowercase scalar the origin of the NFC character it came from. Each
        // character lowercases to as many scalars on its own as it does in the whole; the final
        // sigma is the one context-dependent mapping, and it is one scalar either way.
        let text = nfc.to_lowercase();
        let mut origin: Vec<(usize, usize)> = Vec::with_capacity(nfc_origin.len());
        for (c, from) in nfc.chars().zip(nfc_origin) {
            origin.extend(std::iter::repeat(from).take(c.to_lowercase().count()));
        }
        let offsets: Vec<usize> = text.char_indices().map(|(i, _)| i).chain(std::iter::once(text.len())).collect();
        debug_assert_eq!(origin.len() + 1, offsets.len(), "every canonical scalar has an origin");
        // Never index past the text, even if a future lowercasing broke the count above.
        origin.truncate(offsets.len() - 1);
        Canonical { text, offsets, origin }
    }
}

/// The byte ranges of `text`, cut where nothing before can compose or reorder with what follows.
fn segments(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut from = 0;
    for (i, c) in text.char_indices() {
        if i > from && starts_segment(c) {
            out.push((from, i));
            from = i;
        }
    }
    if from < text.len() {
        out.push((from, text.len()));
    }
    out
}

/// Whether NFC leaves everything before `c` alone: `c`, and the first character of its canonical
/// decomposition, are starters that never compose with a character before them (NFC_Quick_Check is
/// not Maybe).
fn starts_segment(c: char) -> bool {
    if c.is_ascii() {
        return true;
    }
    let first = std::iter::once(c).nfd().next().unwrap_or(c);
    canonical_combining_class(c) == 0
        && canonical_combining_class(first) == 0
        && is_nfc_quick(std::iter::once(first)) != IsNormalized::Maybe
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
    fn matching_is_case_insensitive_and_the_text_around_keeps_its_case() {
        let needle = Needle::of("throwaway-erase2");
        assert_eq!(needle, Needle::of("THROWAWAY-ERASE2"));
        let (text, n) =
            redact("Code THROWAWAY-ERASE2, then Throwaway-Erase2 and throwaway-erase2 — DONE.", &[needle]).unwrap();
        assert_eq!(n, 3);
        assert_eq!(text, format!("Code {MARKER}, then {MARKER} and {MARKER} — DONE."));
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
    fn composed_and_decomposed_in_either_case_are_the_same_words() {
        let forms = ["caf\u{e9}", "cafe\u{301}", "CAF\u{c9}", "CAFE\u{301}", "Caf\u{e9}"];
        for needle in &forms[..4] {
            assert_eq!(Needle::of(needle), Needle::of("caf\u{e9}"), "{needle:?}");
            for stored in forms {
                let (text, n) = redact(&format!("At the {stored}, Bob."), &[Needle::of(needle)]).unwrap();
                assert_eq!((text, n), (format!("At the {MARKER}, Bob."), 1), "{needle:?} in {stored:?}");
            }
        }
        // Untouched text keeps its own normalisation, decomposed or not.
        let (text, _) = redact("CAFE\u{301} and Cafe\u{301} Lune", &[Needle::of("CAF\u{c9} LUNE")]).unwrap();
        assert_eq!(text, format!("CAFE\u{301} and {MARKER}"));
    }

    #[test]
    fn a_dotted_capital_i_inside_a_match_is_erased_whole() {
        let needle = Needle::of("\u{130}stanbul");
        assert_eq!(needle.len, 9, "'İ' lowercases to two scalars");
        let (text, n) = redact("Fly to \u{130}STANBUL, then \u{130}stanbul.", &[needle.clone()]).unwrap();
        assert_eq!((text, n), (format!("Fly to {MARKER}, then {MARKER}."), 2));
        // Stored decomposed, 'I' + U+0307 composes to 'İ' first: still the same words.
        let (text, _) = redact("Visit I\u{307}stanbul now", &[needle]).unwrap();
        assert_eq!(text, format!("Visit {MARKER} now"));
    }

    #[test]
    fn a_dotted_capital_i_next_to_a_match_is_not_touched() {
        let (text, n) = redact("\u{130}PRIYA\u{130} x \u{130}priya", &[Needle::of("Priya")]).unwrap();
        assert_eq!((text, n), (format!("\u{130}{MARKER}\u{130} x \u{130}{MARKER}"), 2));
    }

    #[test]
    fn a_window_that_ends_or_begins_inside_a_dotted_capital_i_takes_all_of_it_and_no_more() {
        // "abci" matches the first scalar of the 'İ' in "ABCİ": the span widens to the whole 'İ',
        // and the next character is left as it was.
        let (text, n) = redact("xABC\u{130}y ABC\u{130}", &[Needle::of("abci")]).unwrap();
        assert_eq!((text, n), (format!("x{MARKER}y {MARKER}"), 2));
        // A needle that begins with the dot above matches the second scalar: widened back to the
        // 'İ', and nothing before it.
        let (text, n) = redact("x\u{130}STANBUL", &[Needle::of("\u{307}stan")]).unwrap();
        assert_eq!((text, n), (format!("x{MARKER}BUL"), 1));
        // The dot left over after a widened match is not matched again.
        let (text, n) = redact("ABC\u{130}\u{307}x", &[Needle::of("abci"), Needle::of("\u{307}")]).unwrap();
        assert_eq!((text, n), (format!("{MARKER}{MARKER}x"), 2), "the second dot is its own character");
    }

    #[test]
    fn a_needle_split_across_pieces_is_found_and_written_back_piece_by_piece() {
        let (pieces, n) = redact_pieces(&["Her name is Pri", "ya, and she", " lives here."], &[Needle::of("Priya")]).unwrap();
        assert_eq!(n, 1);
        assert_eq!(pieces, vec![format!("Her name is {MARKER}"), ", and she".to_string(), " lives here.".to_string()]);
        assert_eq!(pieces.concat(), format!("Her name is {MARKER}, and she lives here."));
        let (pieces, _) = redact_pieces(&["Her name is PRI", "ya, and she"], &[Needle::of("priya")]).unwrap();
        assert_eq!(pieces, vec![format!("Her name is {MARKER}"), ", and she".to_string()]);
    }

    #[test]
    fn a_combining_mark_at_the_start_of_a_piece_still_composes() {
        let (pieces, n) = redact_pieces(&["the CAFE", "\u{301} closed"], &[Needle::of("caf\u{e9}")]).unwrap();
        assert_eq!(n, 1);
        assert_eq!(pieces.concat(), format!("the {MARKER} closed"));
        assert_eq!(pieces.len(), 2);
    }

    #[test]
    fn a_piece_no_match_reached_comes_back_as_it_was() {
        let (pieces, _) = redact_pieces(&["Cafe\u{301} ", "Priya", " Cafe\u{301}"], &[Needle::of("priya")]).unwrap();
        assert_eq!(pieces, vec!["Cafe\u{301} ".to_string(), MARKER.to_string(), " Cafe\u{301}".to_string()]);
    }

    #[test]
    fn the_longest_needle_wins_where_two_begin() {
        let needles = [Needle::of("12 Elm"), Needle::of("12 Elm Street")];
        let (text, n) = redact("at 12 ELM STREET now", &needles).unwrap();
        assert_eq!((text, n), (format!("at {MARKER} now"), 1));
    }

    #[test]
    fn strings_inside_json_are_erased_and_keys_are_not() {
        let mut v = serde_json::json!({"Priya": "call PRIYA", "n": 3, "list": ["Priya", "x"]});
        assert_eq!(redact_json(&mut v, &[Needle::of("Priya")]), 2);
        assert_eq!(v, serde_json::json!({"Priya": format!("call {MARKER}"), "n": 3, "list": [MARKER, "x"]}));
    }

    /// The needles both sides must compute, byte for byte: `harnesses/tests/fixtures/
    /// redact_needles.json`, asserted here and by the Python harness library's tests.
    #[test]
    fn needles_match_the_shared_fixtures() {
        let fixtures: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../../../harnesses/tests/fixtures/redact_needles.json")).unwrap();
        let texts: Vec<&str> = fixtures.iter().map(|f| f["text"].as_str().unwrap()).collect();
        assert_eq!(texts, ["Stra\u{df}e", "\u{130}stanbul", "\u{c9}", "E\u{301}", "throwaway-erase2"]);
        for f in &fixtures {
            let text = f["text"].as_str().unwrap();
            let needle = Needle::of(text);
            assert_eq!(needle.sha256, f["sha256"].as_str().unwrap(), "sha256 of {text:?}");
            assert_eq!(needle.len as u64, f["len"].as_u64().unwrap(), "len of {text:?}");
            assert!(validate(&[needle.clone()]).is_ok());
            // And the shell finds each one in its own text.
            assert_eq!(redact(&format!("<{text}>"), &[needle]), Some((format!("<{MARKER}>"), 1)), "{text:?}");
        }
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
