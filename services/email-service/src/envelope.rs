//! Turning an IMAP ENVELOPE's bytes into text a person can read.
//!
//! The list view is built from `FETCH ENVELOPE`, and for a long time what the envelope held was
//! shown as it arrived: `String::from_utf8_lossy` and nothing else. Against a demo fixture that
//! looks fine. The first real Gmail inbox on a real machine showed what it leaves out:
//!
//! - **IMAP quoting.** A subject is a quoted string on the wire, and inside one a `"` is sent as
//!   `\"`. `imap-proto` hands the bytes back still escaped, so a Reddit digest titled
//!   `"Grandpa gave me this"` was listed as `\"Grandpa gave me this\"`.
//! - **RFC 2047.** Any header with a character outside ASCII is sent as an encoded word,
//!   `=?UTF-8?B?…?=`. Undecoded, every subject or sender name not written in plain English is a
//!   line of base64.
//! - **The date.** `Sun, 20 Sep 2026 03:47:42 +0000` is the sender's clock in the sender's zone,
//!   in a format meant for parsers, and it was the widest thing in the row.
//!
//! No sockets and no state, so `tests/email-core` compiles this file as it stands.

/// A header value from an envelope — a subject or a display name — as text.
pub fn text(raw: &[u8]) -> String {
    let unquoted = unescape_quoted(&String::from_utf8_lossy(raw));
    // mailparse decodes encoded words as part of parsing a header, so it is handed one. A value
    // it cannot parse is shown as it came rather than dropped: a strange subject beats none.
    let as_header = format!("X: {unquoted}");
    match mailparse::parse_header(as_header.as_bytes()) {
        Ok((header, _)) => header.get_value().trim().to_string(),
        Err(_) => unquoted.trim().to_string(),
    }
}

/// Undo the two escapes an IMAP quoted string has: `\"` and `\\`.
fn unescape_quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some(next @ ('"' | '\\')) => out.push(next),
                // Not an escape IMAP defines, so not ours to remove.
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// An envelope's date, in this machine's zone, short enough to sit in a list row and sortable
/// as text: `2026-09-20 03:47`. A date that does not parse is passed through unchanged — it is
/// still the only thing known about when the message was sent.
pub fn date(raw: &[u8]) -> String {
    let original = String::from_utf8_lossy(raw).trim().to_string();
    match mailparse::dateparse(&original) {
        // `dateparse` is lenient to a fault: handed "sometime last week" it answers Ok(0), and
        // the row said 1969-12-31 18:00. Nothing in a mailbox was sent before 1970, so a zero or
        // negative answer is the parser not having found a date, and is treated as that.
        Ok(unix) if unix > 0 => local_minute(unix).unwrap_or(original),
        _ => original,
    }
}

fn local_minute(unix: i64) -> Option<String> {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_opt(unix, 0)
        .single()
        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
}

/// The same instant in UTC — what the tests can assert on a machine in any zone.
#[cfg(test)]
fn utc_minute(unix: i64) -> String {
    use chrono::TimeZone;
    chrono::Utc.timestamp_opt(unix, 0).single().unwrap().format("%Y-%m-%d %H:%M").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quoted_subject_loses_its_imap_escapes() {
        assert_eq!(text(br#"\"Grandpa gave me this for my new apartment\""#),
                   r#""Grandpa gave me this for my new apartment""#);
    }

    #[test]
    fn a_backslash_that_is_not_an_escape_is_kept() {
        assert_eq!(text(br"C:\\Users and a \n that is just text"), r"C:\Users and a \n that is just text");
    }

    #[test]
    fn an_encoded_word_is_decoded() {
        assert_eq!(text(b"=?UTF-8?B?4KSo4KSu4KS44KWN4KSk4KWH?="), "नमस्ते");
        assert_eq!(text(b"=?ISO-8859-1?Q?Caf=E9?= menu"), "Café menu");
    }

    #[test]
    fn plain_text_is_left_alone() {
        assert_eq!(text(b"Security alert"), "Security alert");
        assert_eq!(text(b""), "");
    }

    #[test]
    fn a_date_is_parsed_whatever_zone_it_was_sent_from() {
        let a = mailparse::dateparse("Sun, 20 Sep 2026 03:47:42 +0000").unwrap();
        let b = mailparse::dateparse("Sat, 19 Sep 2026 20:47:42 -0700").unwrap();
        assert_eq!(a, b);
        assert_eq!(utc_minute(a), "2026-09-20 03:47");
    }

    #[test]
    fn the_short_date_is_sixteen_characters_and_sorts_as_text() {
        let earlier = date(b"Thu, 17 Sep 2026 20:39:52 -0700");
        let later = date(b"Sun, 20 Sep 2026 03:47:42 +0000");
        assert_eq!(earlier.len(), 16);
        assert!(earlier < later, "{earlier} should sort before {later}");
    }

    #[test]
    fn a_date_that_does_not_parse_is_shown_as_it_came() {
        assert_eq!(date(b"sometime last week"), "sometime last week");
    }
}
