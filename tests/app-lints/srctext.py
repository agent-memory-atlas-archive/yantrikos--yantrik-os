#!/usr/bin/env python3
"""srctext.py -- masking source text so brackets can be counted honestly.

Both lints have to match braces and parentheses across many lines. A brace
inside a string literal or a comment is not a brace, so every scan here runs
over a *masked* copy of the file: the interior of every string, character
literal and comment is replaced by spaces of the same length, and every other
byte is left where it was. Offsets and line numbers therefore still refer to
the real file, and `text.count("\\n", 0, i)` is still the line number.

Nothing here understands Rust or Slint. It understands quotes, escapes, raw
strings and comment delimiters, which is all that bracket counting needs.
"""

__all__ = ["mask_rust", "mask_slint", "line_of", "depth_map", "match_bracket", "skip_ws"]

OPENERS = "([{"
CLOSERS = ")]}"
PAIRS = {")": "(", "]": "[", "}": "{"}


def _blanker(src):
    """Return (out_list, blank_fn). blank_fn(a, b) spaces out src[a:b], keeping newlines."""
    out = list(src)

    def blank(a, b):
        for k in range(max(a, 0), min(b, len(src))):
            if src[k] != "\n":
                out[k] = " "

    return out, blank


def _mask_comments(src, out, blank, i, n):
    """Handle a comment starting at i. Returns the new index, or None if not a comment."""
    if src.startswith("//", i):
        j = src.find("\n", i)
        j = n if j < 0 else j
        blank(i, j)
        return j
    if src.startswith("/*", i):
        # Rust block comments nest; Slint's do not, but counting depth is harmless
        # for Slint because an unnested comment closes at depth 1 either way.
        depth, j = 1, i + 2
        while j < n and depth:
            if src.startswith("/*", j):
                depth += 1
                j += 2
            elif src.startswith("*/", j):
                depth -= 1
                j += 2
            else:
                j += 1
        blank(i, j)
        return j
    return None


def _mask_dq_string(src, blank, i, n):
    """Blank the interior of a normal "..." string starting at i. Returns index past it."""
    j = i + 1
    while j < n:
        c = src[j]
        if c == "\\":
            j += 2
            continue
        if c == '"':
            j += 1
            break
        j += 1
    blank(i + 1, j - 1)
    return j


def mask_rust(src):
    """Blank out Rust comments, string literals, raw strings and char literals.

    A lifetime (`'a`, `'static`) is left alone: it is not a literal and it never
    carries a bracket. A char literal (`'x'`, `'\\n'`) is blanked, because `'('`
    would otherwise unbalance a scan.
    """
    out, blank = _blanker(src)
    i, n = 0, len(src)
    while i < n:
        c = src[i]
        if c == "/":
            j = _mask_comments(src, out, blank, i, n)
            if j is not None:
                i = j
                continue
            i += 1
        elif c == "r" and i + 1 < n and src[i + 1] in '"#':
            prev = src[i - 1] if i else ""
            if prev.isalnum() or prev == "_":
                i += 1  # part of an identifier such as `for` or `char`
                continue
            j = i + 1
            hashes = 0
            while j < n and src[j] == "#":
                hashes += 1
                j += 1
            if j < n and src[j] == '"':
                closing = '"' + "#" * hashes
                k = src.find(closing, j + 1)
                k = n if k < 0 else k + len(closing)
                blank(j + 1, k - len(closing))
                i = k
            else:
                i += 1
        elif c == '"':
            i = _mask_dq_string(src, blank, i, n)
        elif c == "'":
            if src.startswith("'\\", i):  # escaped char literal
                j = i + 2
                while j < n and src[j] != "'":
                    j += 1
                blank(i + 1, j)
                i = min(j + 1, n)
            elif i + 2 < n and src[i + 2] == "'":  # plain char literal
                blank(i + 1, i + 2)
                i += 3
            else:
                i += 1  # a lifetime
        else:
            i += 1
    return "".join(out)


def mask_slint(src):
    """Blank out Slint comments and string literals.

    Slint strings are double-quoted with backslash escapes, and `\\{expr}`
    interpolation lives inside them. Blanking the whole interior takes the
    interpolation with it, which is correct here: the lints never read inside a
    string, and an unmatched brace in one would break the scan.
    """
    out, blank = _blanker(src)
    i, n = 0, len(src)
    while i < n:
        c = src[i]
        if c == "/":
            j = _mask_comments(src, out, blank, i, n)
            if j is not None:
                i = j
                continue
            i += 1
        elif c == '"':
            i = _mask_dq_string(src, blank, i, n)
        else:
            i += 1
    return "".join(out)


def line_of(text, index):
    """1-based line number of a character offset."""
    return text.count("\n", 0, index) + 1


def depth_map(text):
    """Bracket depth before each character. Counts (), [] and {} together.

    Run it over masked text only. The value at index i is the number of
    unclosed brackets to the left of i.
    """
    depths = [0] * (len(text) + 1)
    d = 0
    for i, c in enumerate(text):
        depths[i] = d
        if c in OPENERS:
            d += 1
        elif c in CLOSERS:
            d = max(d - 1, 0)
    depths[len(text)] = d
    return depths


def match_bracket(text, open_index):
    """Index just past the bracket closing the one at open_index, or len(text).

    Expects masked text. Mismatched kinds are tolerated rather than raised: a
    lint that cannot parse a file should report nothing about it, not crash the
    run for every other app.
    """
    opener = text[open_index]
    if opener not in OPENERS:
        raise ValueError("not an opening bracket at %d: %r" % (open_index, opener))
    depth = 0
    for i in range(open_index, len(text)):
        c = text[i]
        if c in OPENERS:
            depth += 1
        elif c in CLOSERS:
            depth -= 1
            if depth == 0:
                return i + 1
    return len(text)


def skip_ws(text, i):
    """First index at or after i that is not whitespace."""
    n = len(text)
    while i < n and text[i].isspace():
        i += 1
    return i
