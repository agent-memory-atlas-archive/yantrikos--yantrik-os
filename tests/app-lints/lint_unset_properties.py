#!/usr/bin/env python3
"""lint_unset_properties.py -- the `in` property nothing ever writes.

A Slint window declares `in property <string> doc-file-path;`. Slint gives it
the type's default. The Rust side never calls the generated `set_doc_file_path`.
The default is then drawn on screen, and read by everyone as a measurement.

That is Document Editor's Save, which always takes its early return because the
path is empty. It is Network Manager's "Firewall: Off", in warning colour,
which is a hardcoded `false` the app never looked up. It is Spreadsheet's
`cell-grid`, never populated, so `row_count() == 0` and cell-click, cell-edit
and the formula bar all fail their guards behind a perfect blank 50x26 grid.

The check: for every `in` and `in-out` property declared on an app's exported
window, assert that something under that app's `src/` calls the generated
setter. Slint's `foo-bar` becomes Rust's `set_foo_bar`.

Two severities, because the two kinds of property do not mean the same thing:

  * an `in` property is written by Rust by definition. Never written is a
    failure.
  * an `in-out` property may be driven entirely by the UI -- a text field the
    user types into, a panel the user opens. Never written from Rust is worth
    seeing but is not by itself a fault, so it is reported separately and does
    not fail the run.

Run standalone:  python3 lint_unset_properties.py [--app NAME]
"""

import argparse
import json
import re
import sys

from appscan import discover_apps, repo_root
from srctext import depth_map, line_of, mask_rust, mask_slint, match_bracket, skip_ws

LINT = "unset-property"

# `export component NetworkManagerApp inherits Window {`
COMPONENT_RE = re.compile(
    r"\bexport\s+component\s+(?P<name>[A-Za-z_][\w-]*)"
    r"(?:\s+inherits\s+(?P<base>[A-Za-z_][\w-]*))?\s*\{"
)
# The start of a property declaration. `in-out` first so `in` cannot win the
# prefix; \b on the tail keeps `input` and `internal` out.
PROPERTY_RE = re.compile(r"(?<![\w-])(?P<kind>in-out|in|out|private)\s+property\b")
NAME_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_-]*")


class Property:
    def __init__(self, name, kind, line, default=None, alias=None, type_name=None):
        self.name = name
        self.kind = kind          # "in" or "in-out"
        self.line = line
        self.default = default    # source text of the default, or None
        self.alias = alias        # `<=>` target, or None
        self.type_name = type_name

    @property
    def setter(self):
        """Slint normalises - and _ to the same identifier; Rust gets underscores."""
        return "set_" + self.name.replace("-", "_")

    def __repr__(self):
        return "Property(%r, %s, line=%d)" % (self.name, self.kind, self.line)


def parse_window_properties(slint_text):
    """Every `in`/`in-out` property declared directly on the exported component.

    Only the exported component's own body counts: a property declared inside a
    child element belongs to that child and has no generated setter on the
    window. Depth is measured over masked text, so a brace in a string or a
    comment cannot move it.
    """
    masked = mask_slint(slint_text)
    match = COMPONENT_RE.search(masked)
    if not match:
        return []
    body_open = masked.index("{", match.start())
    body_end = match_bracket(masked, body_open)
    body_start = body_open + 1

    depths = depth_map(masked)
    base_depth = depths[body_start]

    props = []
    for decl in PROPERTY_RE.finditer(masked, body_start, body_end):
        if depths[decl.start()] != base_depth:
            continue  # inside a child element, not on the window
        kind = decl.group("kind")
        parsed = _parse_declaration(masked, decl.end(), body_end)
        if parsed is None:
            continue
        name, type_name, default, alias = parsed
        if kind in ("in", "in-out"):
            props.append(
                Property(name, kind, line_of(masked, decl.start()), default, alias, type_name)
            )
    return props


def _parse_declaration(text, pos, limit):
    """Read `<TYPE> name [: default | <=> target] ;` starting after the `property`.

    Returns (name, type_name, default_text, alias_text) or None when the text
    does not look like a declaration at all. Handles the declaration spanning
    any number of lines, because it reads characters rather than lines.
    """
    pos = skip_ws(text, pos)
    type_name = None
    if pos < limit and text[pos] == "<" and not text.startswith("<=>", pos):
        depth, j = 0, pos
        while j < limit:
            if text[j] == "<":
                depth += 1
            elif text[j] == ">":
                depth -= 1
                if depth == 0:
                    j += 1
                    break
            j += 1
        type_name = text[pos + 1 : j - 1].strip()
        pos = skip_ws(text, j)

    name_match = NAME_RE.match(text, pos)
    if not name_match:
        return None
    name = name_match.group(0)
    pos = skip_ws(text, name_match.end())

    default = alias = None
    if text.startswith("<=>", pos):
        alias = _read_to_semicolon(text, pos + 3, limit).strip()
    elif pos < limit and text[pos] == ":":
        default = _read_to_semicolon(text, pos + 1, limit).strip()
    # Anything else (`;`, or a `{` opening a callback-style body) leaves both None.
    return name, type_name, default, alias


def _read_to_semicolon(text, pos, limit):
    """Text up to the `;` that ends the statement, ignoring any inside brackets."""
    depth = 0
    for i in range(pos, limit):
        c = text[i]
        if c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
            if depth < 0:
                return text[pos:i]
        elif c == ";" and depth == 0:
            return text[pos:i]
    return text[pos:limit]


def setters_called(rust_sources):
    """Every `set_*` name called anywhere in the app's Rust.

    Matched on `.set_name(` and `::set_name(` with no regard for the receiver,
    which is the point: a setter reached through a differently-named handle, a
    weak upgrade, a helper function or `global::<Theme>()` is still a write.
    Comments and string literals are masked out first, so a commented-out
    setter does not count as a call.

    Returns {name: [(path, line)]}.
    """
    found = {}
    for path, text in rust_sources:
        masked = mask_rust(text)
        for m in re.finditer(r"(?:\.|::)\s*(set_[A-Za-z0-9_]+)\s*\(", masked):
            found.setdefault(m.group(1), []).append((path, line_of(masked, m.start())))
    return found


def check_app(app):
    """Findings for one app: a list of dicts, each with a severity."""
    if not app.slint_path:
        return []
    props = parse_window_properties(app.read_slint())
    called = setters_called(app.read_rust())

    findings = []
    for prop in props:
        if prop.setter in called:
            continue
        findings.append(
            {
                "lint": LINT,
                "app": app.name,
                "name": prop.name,
                "kind": prop.kind,
                "severity": "error" if prop.kind == "in" else "warning",
                "setter": prop.setter,
                "line": prop.line,
                "type": prop.type_name,
                "default": prop.default,
                "alias": prop.alias,
                "detail": _detail(prop),
            }
        )
    return findings


def _detail(prop):
    where = "%s property" % prop.kind
    if prop.default is not None:
        shown = prop.default if len(prop.default) <= 40 else prop.default[:37] + "..."
        return "%s, never set from Rust; the window shows its default %s" % (where, shown)
    if prop.alias is not None:
        return "%s aliased to %s, never set from Rust" % (where, prop.alias)
    return "%s, never set from Rust; the window shows the type default" % where


def run(apps):
    return [f for app in apps for f in check_app(app)]


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--app", help="check one app instead of all of them")
    parser.add_argument("--json", action="store_true", help="machine-readable output")
    args = parser.parse_args(argv)

    apps = discover_apps(repo_root())
    if args.app:
        apps = [a for a in apps if a.name == args.app]
        if not apps:
            print("no app named %s" % args.app, file=sys.stderr)
            return 2

    findings = run(apps)
    if args.json:
        print(json.dumps(findings, indent=2))
    else:
        for f in findings:
            print(
                "%-18s %-8s %-28s %s:%d  %s"
                % (f["app"], f["severity"], f["name"], "ui/app.slint", f["line"], f["detail"])
            )
    return 1 if any(f["severity"] == "error" for f in findings) else 0


if __name__ == "__main__":
    sys.exit(main())
