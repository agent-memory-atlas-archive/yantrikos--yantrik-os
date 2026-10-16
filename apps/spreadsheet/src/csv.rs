//! CSV, read and written the way RFC 4180 and every spreadsheet do it.
//!
//! The first version split the text on `\n` before it looked at quotes, so a cell with a line
//! break in it was written correctly and read back as broken rows; it did not quote `\r` and
//! kept Excel's byte-order mark as part of the first cell (review of #727, finding 2). This reads
//! character by character with the quote state in hand, so a quoted field may hold commas,
//! doubled quotes, `\n`, `\r` and `\r\n`, and the mark is dropped.
//!
//! A blank line in the middle of a file is a row of one empty cell, not nothing: skipping it
//! moved every row below it up by one, and with it every reference into them.

/// Rows of fields, or a sentence saying where the file stops making sense.
pub fn parse(text: &str) -> Result<Vec<Vec<String>>, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut rows = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    // Whether anything of the current row has been read, so a final newline ends the last row
    // instead of starting an empty one.
    let mut started = false;
    let mut line = 1usize;
    let mut quote_line = 0usize;
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = false,
                other => {
                    if other == '\n' {
                        line += 1;
                    }
                    field.push(other);
                }
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => {
                quoted = true;
                quote_line = line;
                started = true;
            }
            ',' => {
                row.push(std::mem::take(&mut field));
                started = true;
            }
            '\r' | '\n' => {
                if c == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                line += 1;
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
                started = false;
            }
            other => {
                field.push(other);
                started = true;
            }
        }
    }
    if quoted {
        return Err(format!(
            "the file ends inside a quoted cell that starts on line {quote_line} — a closing \" is missing"
        ));
    }
    if started {
        row.push(field);
        rows.push(row);
    }
    Ok(rows)
}

/// Rows as CSV text: the inverse of [`parse`]. Lines end in `\r\n`, as RFC 4180 has them.
pub fn write(rows: &[Vec<String>]) -> String {
    let mut out = String::new();
    for row in rows {
        for (c, field) in row.iter().enumerate() {
            if c > 0 {
                out.push(',');
            }
            if field.contains([',', '"', '\n', '\r']) {
                out.push('"');
                out.push_str(&field.replace('"', "\"\""));
                out.push('"');
            } else {
                out.push_str(field);
            }
        }
        out.push_str("\r\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(r: &[&[&str]]) -> Vec<Vec<String>> {
        r.iter().map(|row| row.iter().map(|s| s.to_string()).collect()).collect()
    }

    #[test]
    fn quoted_commas_quotes_and_line_breaks_survive_a_round_trip() {
        let data = rows(&[
            &["name", "note", "amount"],
            &["Smith, Jo", "said \"hi\"", "3"],
            &["multi", "line one\nline two", "4"],
            &["cr", "old\rmac", "5"],
            &["crlf", "a\r\nb", "6"],
            &["", "", ""],
        ]);
        let text = write(&data);
        assert_eq!(parse(&text).unwrap(), data);
    }

    #[test]
    fn excel_byte_order_mark_and_line_endings_are_read() {
        let text = "\u{feff}a,b\r\n1,2\r\n";
        assert_eq!(parse(text).unwrap(), rows(&[&["a", "b"], &["1", "2"]]));
        assert_eq!(parse("a,b\r1,2").unwrap(), rows(&[&["a", "b"], &["1", "2"]]));
        assert_eq!(parse("a,b\n1,2").unwrap(), rows(&[&["a", "b"], &["1", "2"]]));
    }

    #[test]
    fn a_blank_line_keeps_its_place() {
        assert_eq!(parse("a\n\nb\n").unwrap(), rows(&[&["a"], &[""], &["b"]]));
    }

    #[test]
    fn empty_text_is_no_rows_and_a_trailing_comma_is_an_empty_cell() {
        assert!(parse("").unwrap().is_empty());
        assert_eq!(parse("a,\n").unwrap(), rows(&[&["a", ""]]));
    }

    #[test]
    fn an_unclosed_quote_says_where_it_began() {
        let e = parse("a,b\n\"never\nclosed").unwrap_err();
        assert!(e.contains("line 2"), "{e}");
    }
}
