#!/usr/bin/env python3
"""check_desktop.py -- Yantrik OS desktop conformance check. Python 3 stdlib only.

Drives the `yos` CLI and runs three checks, writing check_results.json and a
short printed summary (the exact fields the report asks for):

  1. describe conformance -- for every surface in `yos ls`: run
     `yos describe <app> --full`; record ok/failed, seconds, error. For every
     action it lists, record whether a grade is declared; an action with no
     grade is a failure.
  2. launch -- for notes, calendar, email, terminal, download-manager: run
     `yos act shell open_app name=<app>`, poll up to 8s for the app's own
     surface to answer `describe`, then re-open and check it still answers.
  3. allowlist -- run a fixed set of standard actions and record the outcome.

The CLI is at /opt/yantrik/bin/yos (not on PATH). Return code 0 == success.
Action lines look like:  act: new_note(title)  [standard, settles on return]
A grade is "declared" when the bracket's first token is one of the known grades.
"""
import datetime
import json
import os
import re
import subprocess
import time

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "check_results.json")
YOS = "/opt/yantrik/bin/yos"
TIMEOUT = 30

KNOWN_GRADES = {"safe", "standard", "sensitive", "dangerous"}

# Fixed launch targets (check 2) and the fixed allowlist (check 3).
LAUNCH_APPS = ["notes", "calendar", "email", "terminal", "download-manager"]
ALLOWLIST = [
    ("notes", "new_note", {"title": "smoke test"}),
    ("notes", "save", {}),
    ("notes", "search", {"query": "smoke"}),
    ("calendar", "go_to_today", {}),
    ("shell", "refresh_apps", {}),
]

ACT_LINE_RE = re.compile(r"^\s*act:\s*([A-Za-z0-9_]+)\s*\(")


def run_yos(args, timeout=TIMEOUT):
    """Run one `yos` command. Returns (returncode, stdout, stderr, elapsed)."""
    t0 = time.monotonic()
    try:
        p = subprocess.run([YOS] + args, capture_output=True, text=True, timeout=timeout)
        return p.returncode, (p.stdout or ""), (p.stderr or ""), time.monotonic() - t0
    except subprocess.TimeoutExpired:
        return 124, "", "timeout after %ss" % timeout, time.monotonic() - t0
    except OSError as e:
        return 126, "", str(e), time.monotonic() - t0


def parse_surface_list(text):
    """Bare surface tokens from `yos ls`, in order, de-duped."""
    seen, out = set(), []
    for line in (text or "").splitlines():
        s = line.strip()
        if not s or s.endswith(":") or " " in s:
            continue
        if s not in seen:
            seen.add(s)
            out.append(s)
    return out


def grade_declared(line):
    """True if this action line declares a known grade in its bracket."""
    m = re.search(r"\[\s*([^\]]*)\s*\]", line)
    if not m:
        return False
    first = m.group(1).split(",")[0].strip().lower()
    return first in KNOWN_GRADES


def describe_answers(name):
    """True if `yos describe <name> --full` answers (rc 0), trying name + app- forms."""
    cands = [name]
    if name.startswith("app-"):
        cands.append(name[4:])
    else:
        cands.append("app-" + name)
    for c in cands:
        rc, _, _, _ = run_yos(["describe", c, "--full"])
        if rc == 0:
            return True
    return False


def trim(s, n=100):
    s = (s or "").replace("\n", " ").strip()
    return s[:n]


def check_describe():
    """Check 1: every `yos ls` surface answers describe; every action has a grade."""
    rc, out, err, _ = run_yos(["ls"])
    surfaces = parse_surface_list(out) if rc == 0 else []
    records, actions_inspected, missing = [], 0, []
    for s in surfaces:
        rc, out, err, elapsed = run_yos(["describe", s, "--full"])
        ok = rc == 0
        acts, bad = [], []
        for ln in (out or "").splitlines():
            m = ACT_LINE_RE.match(ln)
            if not m:
                continue
            declared = grade_declared(ln)
            acts.append({"name": m.group(1), "grade_declared": declared})
            actions_inspected += 1
            if not declared:
                bad.append(m.group(1))
                missing.append({"surface": s, "action": m.group(1)})
        records.append({
            "surface": s,
            "ok": ok,
            "failed": (not ok) or bool(bad),
            "seconds": round(elapsed, 3),
            "error": trim(err or out, 300) if not ok else None,
            "action_count": len(acts),
            "actions": acts,
            "actions_without_grade": bad,
        })
    return {
        "surfaces_checked": len(surfaces),
        "surfaces_ok": sum(1 for r in records if r["ok"]),
        "surfaces_failed": sum(1 for r in records if not r["ok"]),
        "actions_inspected": actions_inspected,
        "actions_without_grade": missing,
        "records": records,
    }


def check_launch():
    """Check 2: open each app, confirm the surface comes up, re-open, still answers."""
    records = []
    total = passed = failed = 0
    for app in LAUNCH_APPS:
        # first open
        rc, out, err, el = run_yos(["act", "shell", "open_app", "name=" + app])
        first_open = rc == 0
        # poll up to 8s for the surface to answer describe
        deadline = time.monotonic() + 8.0
        surface_up = False
        while True:
            if describe_answers(app):
                surface_up = True
                break
            if time.monotonic() >= deadline:
                break
            time.sleep(0.5)
        # second open
        rc2, out2, err2, el2 = run_yos(["act", "shell", "open_app", "name=" + app])
        second_open = rc2 == 0
        time.sleep(3)
        still = describe_answers(app)

        checks = [
            ("first_open", first_open and surface_up),
            ("second_open", second_open),
            ("still_answering", still),
        ]
        rec_checks = {}
        for label, ok in checks:
            total += 1
            if ok:
                passed += 1
            else:
                failed += 1
            rec_checks[label] = ok
        records.append({
            "app": app,
            "checks": rec_checks,
            "detail": {
                "first_open_rc0": first_open,
                "surface_up_after_first": surface_up,
                "second_open_rc0": second_open,
                "still_answering": still,
            },
        })
    return {"apps": len(LAUNCH_APPS), "total": total, "passed": passed, "failed": failed,
            "records": records}


def check_allowlist():
    """Check 3: run the fixed allowlist of standard actions."""
    records, passed, failed = [], 0, 0
    for app, action, args in ALLOWLIST:
        cmd = ["act", app, action] + ["%s=%s" % (k, v) for k, v in args.items()]
        rc, out, err, el = run_yos(cmd)
        ok = rc == 0
        if ok:
            passed += 1
        else:
            failed += 1
        records.append({
            "app": app, "action": action, "args": args,
            "ok": ok, "failed": not ok, "seconds": round(el, 3),
            "error": trim(err or out, 300) if not ok else None,
        })
    return {"actions": len(ALLOWLIST), "passed": passed, "failed": failed,
            "records": records}


def main():
    c1 = check_describe()
    c2 = check_launch()
    c3 = check_allowlist()
    doc = {
        "generated_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "yos": YOS,
        "check1_describe": c1,
        "check2_launch": c2,
        "check3_allowlist": c3,
        "overall": {
            "surfaces_checked": c1["surfaces_checked"],
            "actions_inspected": c1["actions_inspected"],
            "launch_checks_passed": c2["passed"],
            "launch_checks_failed": c2["failed"],
            "allowlist_passed": c3["passed"],
            "allowlist_failed": c3["failed"],
        },
    }
    with open(OUT, "w") as f:
        json.dump(doc, f, indent=2)

    # Failures as (app, check, error<=100).
    failures = []
    for r in c1["records"]:
        if not r["ok"]:
            failures.append((r["surface"], "describe", trim(r["error"])))
    for a in c1["actions_without_grade"]:
        failures.append((a["surface"], "no_grade:%s" % a["action"], "action lists no grade"))
    for r in c2["records"]:
        for label, ok in r["checks"].items():
            if not ok:
                failures.append((r["app"], label, "launch check did not pass"))
    for r in c3["records"]:
        if not r["ok"]:
            failures.append(("%s %s" % (r["app"], r["action"]), "allowlist", trim(r["error"])))

    print("surfaces checked        : %d" % c1["surfaces_checked"])
    print("actions inspected       : %d" % c1["actions_inspected"])
    print("launch checks           : %d passed / %d failed" % (c2["passed"], c2["failed"]))
    print("allowlist actions       : %d passed / %d failed" % (c3["passed"], c3["failed"]))
    print("results -> %s" % OUT)
    if failures:
        print("failures:")
        for app, check, err in failures:
            print("  %s | %s | %s" % (app, check, err))
    else:
        print("no failures")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
