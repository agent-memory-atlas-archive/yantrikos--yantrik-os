#!/usr/bin/env python3
"""lint_dead_handlers.py -- the callback that only says it was pressed.

    app.on_snip_copy(|id| {
        tracing::info!("Copy snippet {} to clipboard", id);
    });

Copy is what a snippet manager is for. Nothing is copied. The button depresses,
the log line is written where no user will read it, and the app looks like it
worked. Snippets' Save is the same shape with a different disguise:
`let _ = (code, tags); // suppress unused warnings` -- the payload named and
thrown away.

The check: find every `.on_<name>(` registration in an app's Rust, take the
closure's body, and fail it when the body does nothing. "Does nothing" is
exactly four things, and deliberately no more:

  * an empty body -- `|| {}`, `|_| {}`, a body of comments only
  * a `tracing::{info,debug,warn,trace,error}!` or `log::*!` call and nothing else
  * `println!` / `eprintln!` / `print!` / `eprint!` / `dbg!` and nothing else
  * `let _ = <inert>;` -- a discard whose right-hand side calls nothing

That last clause is narrower than "any `let _ =`" on purpose. Download
Manager's every handler is `let _ = settle(&ui, &engine, command(&engine, id));`
and it is the app the audit called a gold standard: the discard is of a
`Result` whose work already happened. So a discard counts as dead only when its
right-hand side is inert -- identifiers, fields, tuples, literals -- which is
what `let _ = (code, tags);` is and what `let _ = settle(...)` is not.

A body that logs *and* does work is alive. A handler given a function path or a
factory call rather than a closure -- `app.on_dl_cancel(id_command(...))` -- is
not read at all; the work is somewhere this lint cannot see, and guessing would
be worse than silence.

Run standalone:  python3 lint_dead_handlers.py [--app NAME]
"""

import argparse
import json
import re
import sys

from appscan import discover_apps, repo_root
from srctext import line_of, mask_rust, match_bracket, skip_ws

LINT = "dead-handler"

REGISTRATION_RE = re.compile(r"\.on_(?P<name>[a-z_][a-z0-9_]*)\s*\(")

# `.on_close_requested` is slint::Window's own API, not a generated callback.
# It is registered on `ui.window()`, it returns a CloseRequestResponse, and it
# is not a control anybody can press.
NOT_GENERATED = {"close_requested"}

LOG_MACRO_RE = re.compile(
    r"^(?:tracing|log)\s*::\s*(?:info|debug|warn|warning|trace|error)\s*!\s*\(",
)
PRINT_MACRO_RE = re.compile(r"^(?:e?print(?:ln)?|dbg)\s*!\s*\(")
BARE_LOG_MACRO_RE = re.compile(r"^(?:info|debug|warn|trace|error)\s*!\s*\(")
DISCARD_RE = re.compile(r"^let\s+_\s*(?::[^=]*)?=\s*(?P<rhs>.*)$", re.DOTALL)
CONTROL_WORD_RE = re.compile(r"\b(?:await|if|match|loop|while|for|return|unsafe|else)\b")
CALL_RE = re.compile(r"[)\]>A-Za-z0-9_]\s*\(")

NOTHING = {"", "()", "{}", "(())"}


class Handler:
    def __init__(self, name, path, line, kind, body, reasons):
        self.name = name
        self.path = path
        self.line = line
        self.kind = kind        # "closure" | "delegated" | "unparsed"
        self.body = body
        self.reasons = reasons  # why it is dead, empty when it is alive

    @property
    def dead(self):
        return bool(self.reasons)


def find_handlers(text, path="<memory>"):
    """Every `.on_*(` registration in one Rust file, classified."""
    masked = mask_rust(text)
    handlers = []
    for m in REGISTRATION_RE.finditer(masked):
        name = m.group("name")
        if name in NOT_GENERATED:
            continue
        open_paren = m.end() - 1
        args_end = match_bracket(masked, open_paren)
        args = masked[open_paren + 1 : args_end - 1]
        line = line_of(masked, m.start())

        body = closure_body(args)
        if body is None:
            handlers.append(Handler(name, path, line, "delegated", None, []))
            continue
        handlers.append(Handler(name, path, line, "closure", body, classify_body(body)))
    return handlers


def closure_body(args):
    """The body of the closure passed as the first argument, or None.

    None means the argument is not a closure: a function path (`handler`), a
    factory call (`id_command(app, &engine, ...)`), a method, anything else.
    Returns the body with its braces stripped when it has them, and the bare
    expression when it does not (`|| tracing::info!("...")`).
    """
    i = skip_ws(args, 0)
    if args.startswith("move", i) and (len(args) <= i + 4 or not (args[i + 4].isalnum() or args[i + 4] == "_")):
        i = skip_ws(args, i + 4)
    if i >= len(args) or args[i] != "|":
        return None
    if args.startswith("||", i):
        i += 2  # no parameters
    else:
        close = args.find("|", i + 1)
        if close < 0:
            return None
        i = close + 1
    i = skip_ws(args, i)
    if i >= len(args):
        return ""
    if args[i] == "{":
        end = match_bracket(args, i)
        return args[i + 1 : end - 1]
    # A braceless body runs to the end of the argument list; a trailing comma
    # would belong to a second argument, which a callback registration has not.
    return args[i:].rstrip().rstrip(",")


def classify_body(body):
    """Reasons the body does nothing. Empty list means it does something."""
    statements = [s.strip() for s in split_statements(body)]
    statements = [s for s in statements if s not in NOTHING]
    if not statements:
        return ["empty body"]

    reasons = []
    for stmt in statements:
        kind = classify_statement(stmt)
        if kind is None:
            return []  # one statement that works is enough to make it alive
        reasons.append(kind)
    return sorted(set(reasons))


def classify_statement(stmt):
    """"log only", "discarded arguments", or None when the statement works."""
    if LOG_MACRO_RE.match(stmt) and _is_whole_call(stmt):
        return "log only"
    if BARE_LOG_MACRO_RE.match(stmt) and _is_whole_call(stmt):
        return "log only"
    if PRINT_MACRO_RE.match(stmt) and _is_whole_call(stmt):
        return "log only"
    discard = DISCARD_RE.match(stmt)
    if discard and is_inert(discard.group("rhs")):
        return "discarded arguments"
    return None


def _is_whole_call(stmt):
    """True when the statement is one macro call and nothing after it.

    `tracing::info!("x"); ui.set_y(1)` never reaches here -- statements are
    split first -- but `tracing::info!("x").foo()` would, and it is work.
    """
    open_paren = stmt.find("(")
    if open_paren < 0:
        return False
    end = match_bracket(stmt, open_paren)
    return stmt[end:].strip() in ("", ";")


def is_inert(expr):
    """True when the expression calls nothing, builds nothing and awaits nothing.

    `(code, tags)`, `x`, `&ui`, `row.text`, `7` are inert. `settle(&ui, r)`,
    `tx.send(v)`, `vec![]`, `foo?`, `if a { b }` are not. The parenthesis test
    asks what is to the left of the `(`: a tuple's `(` follows an operator or
    nothing, a call's `(` follows an identifier, `)`, `]` or `>`.
    """
    expr = expr.strip().rstrip(";").strip()
    if not expr:
        return True
    if "!" in expr or "{" in expr or "?" in expr or "|" in expr:
        return False
    if CONTROL_WORD_RE.search(expr):
        return False
    if CALL_RE.search(expr):
        return False
    return True


def split_statements(body):
    """Split a closure body on the semicolons that are at its own depth."""
    parts, depth, start = [], 0, 0
    for i, c in enumerate(body):
        if c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
        elif c == ";" and depth == 0:
            parts.append(body[start:i])
            start = i + 1
    parts.append(body[start:])
    return parts


def check_app(app):
    """Findings for one app, one per callback name.

    Keyed by name rather than by line, because a line number moves every time
    somebody edits the file above it and the baseline has to survive that. A
    callback registered in two places is dead only if every registration of it
    is dead.
    """
    by_name = {}
    for path, text in app.read_rust():
        rel = path[len(app.path) + 1 :].replace("\\", "/")
        for handler in find_handlers(text, rel):
            by_name.setdefault(handler.name, []).append(handler)

    findings = []
    for name in sorted(by_name):
        registrations = by_name[name]
        closures = [h for h in registrations if h.kind == "closure"]
        if not closures or any(not h.dead for h in closures):
            continue
        if len(closures) < len(registrations):
            continue  # one registration delegates; the work may be there
        first = closures[0]
        reasons = sorted({r for h in closures for r in h.reasons})
        findings.append(
            {
                "lint": LINT,
                "app": app.name,
                "name": name,
                "severity": "error",
                "file": first.path,
                "line": first.line,
                "registrations": len(registrations),
                "reasons": reasons,
                "detail": "callback body is %s" % " and ".join(reasons),
            }
        )
    return findings


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
                "%-18s %-28s %s:%d  %s"
                % (f["app"], f["name"], f["file"], f["line"], f["detail"])
            )
    return 1 if findings else 0


if __name__ == "__main__":
    sys.exit(main())
