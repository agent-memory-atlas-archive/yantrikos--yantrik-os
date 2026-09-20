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


def grade(findings, apps, baseline, allowlist):
    """Split every finding into allowed / known / new, and find stale baseline rows."""
    result = {
        "apps": {},
        "new": [],
        "known": [],
        "allowed": [],
        "warnings": [],
        "stale": [],
        "unused_allowlist": [],
    }
    used_allow = set()

    for app in apps:
        per_app = {"name": app.name, "lints": {}}
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
            result["new"] += new
            result["known"] += known
            result["allowed"] += allowed
            result["warnings"] += warnings
            result["stale"] += stale
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

    for app_name in sorted(result["apps"]):
        per_app = result["apps"][app_name]
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
        out.append(app_name + ("   clean" if clean else ""))
        if not clean:
            out += rows

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
    return "\n".join(out)


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
    result = grade(findings, apps, baseline, allowlist)

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
