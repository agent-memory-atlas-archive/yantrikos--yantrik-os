#!/usr/bin/env python3
"""run.py -- both app lints over every app, against a baseline.

    python3 run.py                 # every app, fail on anything new
    python3 run.py --app notes     # one app
    python3 run.py --json          # the same result, machine-readable
    python3 run.py --baseline      # rewrite baseline.json from today's findings
    python3 run.py --ignore-baseline   # the whole debt, as if the baseline were empty

The repo has hundreds of real violations of both lints, which is the finding
that caused them to be written. A check that fails on all of them on its first
run gets switched off in a week, so this one is graded against a baseline:

  * a failure recorded in `baseline.json` is *known debt*. It is counted and
    printed on every run so it stays visible, and it does not fail the run.
  * a failure not in the baseline is *new rot*. It fails the run.
  * a baseline entry with no matching failure has been fixed. The run says
    "stale baseline entry -- remove it" and fails, so the file cannot drift
    away from the truth and the debt count can only go down.

An allowlist is the other exit, and a narrower one: `allowlist.toml` holds
items that are deliberately as they are, each with a written reason. An entry
without a reason is itself an error. The reason is the point -- a dead control
has to be argued for in prose before it is exempted.

`shelved.toml` is the third register, and it is about whole apps rather than
items. A shelved app is in the tree and not in the build -- removed from the
launcher, the Lens, the palette and the release bundle, but still compiling and
still carrying its debt. It is linted and reported under its own heading with
the reason it is on the shelf, it is left out of the shipping totals, and it
does not fail the run: nobody has been asked to fix it. An app named there that
does not exist under apps/ is an error.
"""

import argparse
import datetime
import json
import os
import sys

import lint_dead_handlers
import lint_unset_properties
from appscan import discover_apps, repo_root

HERE = os.path.dirname(os.path.abspath(__file__))
BASELINE_PATH = os.path.join(HERE, "baseline.json")
ALLOWLIST_TOML = os.path.join(HERE, "allowlist.toml")
ALLOWLIST_JSON = os.path.join(HERE, "allowlist.json")
SHELVED_TOML = os.path.join(HERE, "shelved.toml")

LINTS = {
    lint_unset_properties.LINT: ("unset in properties", lint_unset_properties),
    lint_dead_handlers.LINT: ("dead handlers", lint_dead_handlers),
}
LINT_ORDER = [lint_unset_properties.LINT, lint_dead_handlers.LINT]


# -- the allowlist ------------------------------------------------------------


def load_allowlist(path=None):
    """{(lint, app, name): reason}, plus a list of problems with the file itself.

    TOML is read with the standard library's tomllib (Python 3.11+). A JSON
    allowlist of the same shape is accepted for anywhere that has neither.
    """
    problems = []
    raw = None
    if path:
        raw = _load_table(path, problems)
    elif os.path.isfile(ALLOWLIST_TOML):
        raw = _load_table(ALLOWLIST_TOML, problems)
    elif os.path.isfile(ALLOWLIST_JSON):
        raw = _load_table(ALLOWLIST_JSON, problems)
    if raw is None:
        return {}, problems

    entries = {}
    for lint, apps in raw.items():
        if lint not in LINTS:
            problems.append("allowlist: unknown lint section [%s]" % lint)
            continue
        if not isinstance(apps, dict):
            problems.append("allowlist: [%s] must be a table of app names" % lint)
            continue
        for app, items in apps.items():
            if not isinstance(items, list):
                problems.append("allowlist: %s.%s must be a list of entries" % (lint, app))
                continue
            for item in items:
                name = (item or {}).get("name")
                reason = (item or {}).get("reason")
                if not name:
                    problems.append("allowlist: %s.%s has an entry with no name" % (lint, app))
                    continue
                if not (reason or "").strip():
                    problems.append(
                        "allowlist: %s %s.%s has no reason -- an exemption has to be argued for"
                        % (lint, app, name)
                    )
                    continue
                entries[(lint, app, name)] = reason.strip()
    return entries, problems


def _load_table(path, problems):
    try:
        if path.endswith(".json"):
            with open(path, "r", encoding="utf-8") as fh:
                return json.load(fh)
        import tomllib

        with open(path, "rb") as fh:
            return tomllib.load(fh)
    except ImportError:
        problems.append(
            "allowlist: no tomllib (needs Python 3.11+); write allowlist.json instead"
        )
    except Exception as exc:  # a broken allowlist must be loud, not silent
        problems.append("allowlist: cannot read %s: %s" % (os.path.basename(path), exc))
    return None


# -- the shelf ----------------------------------------------------------------


def load_shelved(path=None, apps=None):
    """{app: reason} for the apps this build does not ship, plus problems.

    `apps` is the discovered app list. An app named on the shelf that is not
    there is a problem, not a silent no-op: the shelf is read by whoever asks
    "why is this app not in the launcher", and a row for something that has been
    deleted answers a question nobody asked.
    """
    problems = []
    path = path or SHELVED_TOML
    if not os.path.isfile(path):
        return {}, problems
    raw = _load_table(path, problems)
    if raw is None:
        return {}, problems

    known = {a.name for a in apps} if apps is not None else None
    entries = {}
    for app, item in raw.items():
        if not isinstance(item, dict):
            problems.append("shelved: [%s] must be a table with a reason" % app)
            continue
        reason = (item.get("reason") or "").strip()
        if not reason:
            problems.append(
                "shelved: %s has no reason -- taking an app off the shelf has to be "
                "argued for in prose" % app
            )
            continue
        if known is not None and app not in known:
            problems.append(
                "shelved: there is no apps/%s -- a shelf entry for an app that is gone "
                "explains nothing" % app
            )
            continue
        entries[app] = reason
    return entries, problems


# -- the baseline -------------------------------------------------------------


def load_baseline(path=BASELINE_PATH):
    if not os.path.isfile(path):
        return {}
    with open(path, "r", encoding="utf-8") as fh:
        data = json.load(fh)
    return data.get("apps", {})


def write_baseline(findings, apps, allowlist=None, path=BASELINE_PATH):
    """Record today's failures as known debt.

    Allowlisted items are left out: the allowlist says an item is correct, the
    baseline says it is wrong but old, and an item cannot be both. Writing them
    to both would make every run report a stale baseline entry.
    """
    allowlist = allowlist or {}
    by_app = {}
    for app in apps:
        entry = {}
        for lint in LINT_ORDER:
            names = sorted(
                f["name"]
                for f in findings
                if f["app"] == app.name
                and f["lint"] == lint
                and f["severity"] == "error"
                and (lint, app.name, f["name"]) not in allowlist
            )
            if names:
                entry[lint] = names
        if entry:
            by_app[app.name] = entry
    document = {
        "written": datetime.date.today().isoformat(),
        "note": (
            "Known debt, recorded so the lints can run today without being switched off. "
            "An entry here is a real violation that predates the check. Fix one, delete its "
            "line; the run reports a stale entry and fails if you do not. Nothing may be "
            "added to this file except by a deliberate rewrite of the whole baseline."
        ),
        "lints": LINT_ORDER,
        "apps": by_app,
    }
    with open(path, "w", encoding="utf-8", newline="\n") as fh:
        json.dump(document, fh, indent=2, sort_keys=False)
        fh.write("\n")
    return document


# -- grading ------------------------------------------------------------------


def grade(findings, apps, baseline, allowlist, shelved=None):
    """Split every finding into allowed / known / new, and find stale baseline rows.

    A shelved app is graded exactly like any other -- it is still linted and its
    debt is still counted and printed -- but its findings go into the `shelved_*`
    lists rather than the ones the exit code is computed from. The debt stays
    visible; it just is not held against a build the app is not in.
    """
    shelved = shelved or {}
    result = {
        "apps": {},
        "new": [],
        "known": [],
        "allowed": [],
        "warnings": [],
        "stale": [],
        "unused_allowlist": [],
        "shelved": dict(shelved),
        "shelved_new": [],
        "shelved_known": [],
        "shelved_allowed": [],
        "shelved_warnings": [],
        "shelved_stale": [],
    }
    used_allow = set()

    for app in apps:
        on_shelf = app.name in shelved
        prefix = "shelved_" if on_shelf else ""
        per_app = {"name": app.name, "shelved": on_shelf, "lints": {}}
        for lint in LINT_ORDER:
            mine = [f for f in findings if f["app"] == app.name and f["lint"] == lint]
            errors = [f for f in mine if f["severity"] == "error"]
            warnings = [f for f in mine if f["severity"] != "error"]
            known_names = set(baseline.get(app.name, {}).get(lint, []))

            allowed, new, known = [], [], []
            for f in errors:
                key = (lint, app.name, f["name"])
                if key in allowlist:
                    used_allow.add(key)
                    f = dict(f, reason=allowlist[key])
                    allowed.append(f)
                elif f["name"] in known_names:
                    known.append(f)
                else:
                    new.append(f)

            seen = {f["name"] for f in errors}
            stale = [
                {"lint": lint, "app": app.name, "name": n}
                for n in sorted(known_names)
                if n not in seen or (lint, app.name, n) in allowlist
            ]

            per_app["lints"][lint] = {
                "new": new,
                "known": known,
                "allowed": allowed,
                "warnings": warnings,
                "stale": stale,
            }
            result[prefix + "new"] += new
            result[prefix + "known"] += known
            result[prefix + "allowed"] += allowed
            result[prefix + "warnings"] += warnings
            result[prefix + "stale"] += stale
        result["apps"][app.name] = per_app

    checked = {a.name for a in apps}
    for app_name, lints in baseline.items():
        if app_name in checked:
            continue
        for lint, names in lints.items():
            for n in names:
                result["stale"].append(
                    {"lint": lint, "app": app_name, "name": n, "note": "no such app"}
                )

    for key in sorted(allowlist):
        if key not in used_allow and key[1] in checked:
            result["unused_allowlist"].append(
                {"lint": key[0], "app": key[1], "name": key[2], "reason": allowlist[key]}
            )
    return result


# -- reporting ----------------------------------------------------------------


def report(result, problems, show_known=False, show_warnings=False, baseline_date=None):
    out = []
    width = max([len(label) for label, _ in LINTS.values()] + [4])

    def app_rows(per_app):
        rows = []
        for lint in LINT_ORDER:
            bucket = per_app["lints"][lint]
            label = LINTS[lint][0]
            counts = "%3d known  %3d new" % (len(bucket["known"]), len(bucket["new"]))
            extra = []
            if bucket["allowed"]:
                extra.append("%d allowed" % len(bucket["allowed"]))
            if bucket["warnings"]:
                extra.append("%d in-out never set" % len(bucket["warnings"]))
            if bucket["stale"]:
                extra.append("%d stale" % len(bucket["stale"]))
            rows.append(
                "  %-*s  %s%s"
                % (width, label, counts, ("   (" + ", ".join(extra) + ")") if extra else "")
            )
        clean = all(
            not per_app["lints"][l]["known"]
            and not per_app["lints"][l]["new"]
            and not per_app["lints"][l]["allowed"]
            and not per_app["lints"][l]["stale"]
            for l in LINT_ORDER
        )
        return rows, clean

    for app_name in sorted(result["apps"]):
        per_app = result["apps"][app_name]
        if per_app.get("shelved"):
            continue
        rows, clean = app_rows(per_app)
        out.append(app_name + ("   clean" if clean else ""))
        if not clean:
            out += rows

    # The shelf, under its own heading. These apps are linted and their debt is
    # counted and printed; it is simply not debt against a build they are not in.
    shelf = sorted(n for n in result.get("shelved", {}) if n in result["apps"])
    if shelf:
        out.append("")
        out.append("SHELVED -- in the tree, not in this build. Not counted, does not fail.")
        for app_name in shelf:
            out.append("")
            out.append("  " + app_name)
            for line in _wrap(result["shelved"][app_name], 74):
                out.append("    " + line)
            rows, clean = app_rows(result["apps"][app_name])
            out += ["  " + r for r in (["  clean"] if clean else rows)]

    def block(title, items, render):
        if not items:
            return
        out.append("")
        out.append(title)
        for item in items:
            out.append("  " + render(item))

    block(
        "NEW -- not in the baseline. Fix these.",
        result["new"],
        lambda f: "%-18s %-16s %-28s %s"
        % (f["app"], LINTS[f["lint"]][0], f["name"], f.get("detail", "")),
    )
    block(
        "STALE BASELINE ENTRIES -- fixed or allowlisted. Remove them from baseline.json.",
        result["stale"],
        lambda f: "%-18s %-16s %-28s %s"
        % (f["app"], LINTS[f["lint"]][0], f["name"], f.get("note", "")),
    )
    block("ALLOWLIST PROBLEMS", [{"m": p} for p in problems], lambda f: f["m"])
    block(
        "ALLOWLIST ENTRIES THAT MATCH NOTHING -- the item is gone or already fixed",
        result["unused_allowlist"],
        lambda f: "%-18s %-16s %s" % (f["app"], LINTS[f["lint"]][0], f["name"]),
    )
    if show_known:
        block(
            "KNOWN DEBT",
            result["known"],
            lambda f: "%-18s %-16s %-28s %s:%s  %s"
            % (
                f["app"],
                LINTS[f["lint"]][0],
                f["name"],
                f.get("file", "ui/app.slint"),
                f.get("line", "?"),
                f.get("detail", ""),
            ),
        )
    if show_warnings:
        block(
            "IN-OUT PROPERTIES NEVER SET FROM RUST -- may be driven by the UI alone",
            result["warnings"],
            lambda f: "%-18s %-28s ui/app.slint:%s" % (f["app"], f["name"], f.get("line", "?")),
        )
    if result["allowed"]:
        block(
            "ALLOWED",
            result["allowed"],
            lambda f: "%-18s %-16s %-28s %s"
            % (f["app"], LINTS[f["lint"]][0], f["name"], f.get("reason", "")),
        )

    out.append("")
    out.append(
        "totals: %d new, %d known debt, %d allowed, %d stale baseline entries, "
        "%d in-out properties never set%s"
        % (
            len(result["new"]),
            len(result["known"]),
            len(result["allowed"]),
            len(result["stale"]),
            len(result["warnings"]),
            (" (baseline written %s)" % baseline_date) if baseline_date else "",
        )
    )
    if shelf:
        out.append(
            "shelved (not counted above): %d known debt, %d new, across %d app%s"
            % (
                len(result["shelved_known"]),
                len(result["shelved_new"]),
                len(shelf),
                "" if len(shelf) == 1 else "s",
            )
        )
    return "\n".join(out)


def _wrap(text, width):
    """The reason, as lines. One paragraph; the file writes them as prose."""
    words = " ".join(text.split()).split(" ")
    lines, line = [], ""
    for word in words:
        if line and len(line) + 1 + len(word) > width:
            lines.append(line)
            line = word
        else:
            line = (line + " " + word).strip()
    if line:
        lines.append(line)
    return lines


# -- entry point --------------------------------------------------------------


def collect(apps):
    findings = []
    for lint in LINT_ORDER:
        findings += LINTS[lint][1].run(apps)
    return findings


def main(argv=None):
    parser = argparse.ArgumentParser(
        description=__doc__.splitlines()[0],
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("--app", help="one app instead of all of them")
    parser.add_argument("--json", action="store_true", help="machine-readable result")
    parser.add_argument(
        "--baseline", action="store_true", help="rewrite baseline.json from today's findings"
    )
    parser.add_argument(
        "--ignore-baseline",
        action="store_true",
        help="grade against an empty baseline: the whole debt as new",
    )
    parser.add_argument("--show-known", action="store_true", help="list the known debt too")
    parser.add_argument(
        "--show-warnings", action="store_true", help="list in-out properties never set from Rust"
    )
    parser.add_argument("--root", help="repository root (default: found from this file)")
    args = parser.parse_args(argv)

    root = args.root or repo_root()
    apps = discover_apps(root)
    if args.app:
        apps = [a for a in apps if a.name == args.app]
        if not apps:
            print("no app named %s under %s/apps" % (args.app, root), file=sys.stderr)
            return 2

    findings = collect(apps)
    allowlist, problems = load_allowlist()
    # Validated against every app in the tree, not against the filtered list, so
    # `--app notes` does not report the shelf as naming apps that are not there.
    shelved, shelf_problems = load_shelved(apps=discover_apps(root))
    problems = problems + shelf_problems

    if args.baseline:
        if args.app:
            print("--baseline rewrites the whole file; run it without --app", file=sys.stderr)
            return 2
        if problems:
            for p in problems:
                print(p, file=sys.stderr)
            print("fix the allowlist before rewriting the baseline", file=sys.stderr)
            return 1
        document = write_baseline(findings, apps, allowlist)
        total = sum(len(v) for app in document["apps"].values() for v in app.values())
        print("wrote %s: %d known violations across %d apps"
              % (os.path.relpath(BASELINE_PATH, root), total, len(document["apps"])))
        return 0

    baseline = {} if args.ignore_baseline else load_baseline()
    result = grade(findings, apps, baseline, allowlist, shelved)

    # A shelved app's findings are deliberately not in these lists. Nobody has been
    # asked to fix an app that is not in the build, and a check that goes red every
    # day about work nobody is doing is a check that gets switched off -- which is
    # the whole reason these lints are graded against a baseline in the first place.
    failed = bool(result["new"] or result["stale"] or problems)
    if args.json:
        print(json.dumps({"ok": not failed, **result, "allowlist_problems": problems}, indent=2))
    else:
        baseline_date = None
        if os.path.isfile(BASELINE_PATH) and not args.ignore_baseline:
            with open(BASELINE_PATH, "r", encoding="utf-8") as fh:
                baseline_date = json.load(fh).get("written")
        print(report(result, problems, args.show_known, args.show_warnings, baseline_date))
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
