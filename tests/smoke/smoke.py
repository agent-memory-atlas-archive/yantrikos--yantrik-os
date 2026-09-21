#!/usr/bin/env python3
"""smoke.py -- Yantrik OS smoke test. Python 3 standard library only.

Drives the `yos` CLI (read-only probes + the sanctioned `shell open_app`):

  1. read inventory.json (built by build_inventory.py)
  2. for each app the shell can open: if its surface is down, ask the shell to
     open it, then re-describe it so its live actions are known
  3. call every action graded `safe` with zero required arguments
  4. write results.json and print only a short summary

Safety model -- the single gate is `is_safe_to_run(action)`. An action is
invoked only if ALL three hold:
  * its grade is exactly `safe`   (standard / sensitive / dangerous are excluded)
  * it has zero required arguments (we never fabricate arguments)
  * its name matches no forbidden verb (send/email, delete, change-settings) --
    a content guard against a dangerous verb ever being mis-graded `safe`.
We never call standard/sensitive/dangerous actions, never send email, never
delete anything, and never change settings. Opening an app is the one
sanctioned side effect (the task authorizes `yos act shell open_app name=<app>`).

Modes:
  (default)   run the suite: open-if-down, re-describe, call safe no-arg
              actions, write results.json, print a short summary.
  --dry-run   report the plan (apps + actions that would pass the gate) using
              inventory.json only. No `yos` calls, no side effects, no results.json.
  --selftest  unit-test the pure decision functions. No I/O, no side effects.
"""
import datetime
import json
import os
import re
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
INVENTORY = os.path.join(HERE, "inventory.json")
RESULTS = os.path.join(HERE, "results.json")
YOS = "/opt/yantrik/bin/yos"
TIMEOUT = 30

# Exact action-line grammar that build_inventory.py validated against the live
# surface:   act: <name>(<arg>,<arg>...)  [<grade>, settles on return|later]
ACT_RE = re.compile(
    r"^\s*act:\s*(?P<name>[A-Za-z0-9_]+)\s*\(\s*(?P<args>[^)]*)\s*\)"
    r"\s*\[\s*(?P<grade>\w+)\s*,\s*settles\s+(?P<settles>on return|later)\s*\]",
    re.MULTILINE,
)
SETTLED_RE = re.compile(r"settled:\s*(true|false)")

# Content guard: verbs we will NEVER invoke, even if one were mis-graded `safe`.
# Substring matching is deliberate -- over-blocking (skip a benign action) is the
# safe failure mode, so we prefer it to under-blocking (calling a bad one).
FORBIDDEN = (
    "send", "email", "mail", "post", "transmit", "dispatch",   # outbound / email
    "delete", "remove", "erase", "wipe", "destroy", "purge",    # deletion
    "set", "change", "configure", "update", "apply", "install",  # change settings
    "uninstall", "modify", "reset", "enable", "disable", "toggle",
)


# ---------------------------------------------------------------------------
# Pure, unit-testable decision functions (no I/O).
# ---------------------------------------------------------------------------

def parse_actions(text):
    """Parse `yos describe <app> --full` output into a list of action dicts.

    Each dict: {"name", "grade", "required_args", "settles_on_return"}.
    """
    acts = []
    for m in ACT_RE.finditer(text or ""):
        args = [a.strip() for a in m.group("args").split(",") if a.strip()]
        acts.append({
            "name": m.group("name"),
            "grade": m.group("grade"),
            "required_args": args,
            "settles_on_return": m.group("settles") == "on return",
        })
    return acts


def is_safe_to_run(action):
    """Decide whether the smoke test may invoke this action.

    True only when the grade is exactly `safe`, there are zero required
    arguments, and the action name contains no forbidden verb (email / delete /
    change-settings). Everything else is recorded but never called.
    """
    if not action:
        return False
    if str(action.get("grade", "")).strip().lower() != "safe":
        return False
    if action.get("required_args"):
        return False
    name = str(action.get("name", "")).lower()
    return not any(word in name for word in FORBIDDEN)


def summarize(results):
    """Turn a list of action-result records into counts.

    Returns {"apps", "actions_run", "passed", "failed", "apps_list"}.
    """
    results = results or []
    apps = sorted({str(r.get("app")) for r in results})
    passed = sum(1 for r in results if r.get("ok"))
    failed = sum(1 for r in results if not r.get("ok"))
    return {
        "apps": len(apps),
        "actions_run": len(results),
        "passed": passed,
        "failed": failed,
        "apps_list": apps,
    }


def candidate_apps(doc):
    """The apps the shell can open, read from inventory.json.

    "Apps the shell can open" = the `app-*` family (GUI apps). RPC services
    (weather, network, notifications, system-monitor, a11y, companion, harness)
    are control surfaces, not openable apps, so they are excluded.
    """
    ids = []
    for a in doc.get("apps", []):
        ids.append(a.get("id"))
    for s in doc.get("surfaces_not_up", []):
        ids.append(s.get("id"))
    seen, out = set(), []
    for i in ids:
        if i and i.startswith("app-") and i not in seen:
            seen.add(i)
            out.append(i)
    return out


# ---------------------------------------------------------------------------
# Thin `yos` CLI plumbing (the only place that touches the OS).
# ---------------------------------------------------------------------------

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


def describe_once(app):
    """Describe one app. Returns (returncode, actions, detail)."""
    rc, out, err, _ = run_yos(["describe", app, "--full"])
    return rc, (parse_actions(out) if rc == 0 else []), (err or out).strip()


def open_app(app):
    """Ask the shell to open an app, trying the id then the short name.

    Returns (ok, name_used, detail). A `settles later` reply is still a success:
    the launch was accepted.
    """
    names = [app]
    if app.startswith("app-"):
        names.append(app[4:])
    detail = ""
    for n in names:
        rc, out, err, _ = run_yos(["act", "shell", "open_app", "name=" + n])
        detail = (err or out).strip()
        if rc == 0:
            return True, n, detail
    return False, names[-1], detail


def ensure_surface(app, polls=4, poll_delay=1.0):
    """Make sure an app's surface is up, then describe it.

    If describe fails (surface down), ask the shell to open the app and poll
    describe until it settles. Returns (actions, was_opened, describe_ok, detail).
    """
    rc, actions, detail = describe_once(app)
    if rc == 0:
        return actions, False, True, detail
    ok, name, odetail = open_app(app)
    for _ in range(polls):
        time.sleep(poll_delay)
        rc, actions, detail = describe_once(app)
        if rc == 0:
            return actions, ok, True, detail
    return actions, ok, False, (detail or odetail)


def call_action(app, name, args):
    """Invoke one action and record the outcome.

    ok = the CLI round-trip succeeded (returncode 0). On failure we record the
    exact error text (stderr, else the reply). `settled` is captured from the
    reply when present (a `settles later` action is still an accepted success).
    """
    cmd = ["act", app, name] + ["%s=%s" % (k, v) for k, v in (args or {}).items()]
    rc, out, err, elapsed = run_yos(cmd)
    ok = rc == 0
    m = SETTLED_RE.search(out or "")
    return {
        "app": app,
        "action": name,
        "ok": ok,
        "failed": not ok,
        "seconds": round(elapsed, 3),
        "settled": {"true": True, "false": False}.get(m.group(1)) if m else None,
        "reply": (out or "").strip()[:2000],
        "error": ((err or out).strip() if not ok else None),
    }


# ---------------------------------------------------------------------------
# Orchestration + verification modes.
# ---------------------------------------------------------------------------

def load_inventory():
    with open(INVENTORY) as f:
        return json.load(f)


def run_smoke():
    doc = load_inventory()
    cands = candidate_apps(doc)
    app_records, results = [], []
    for app in cands:
        actions, was_opened, describe_ok, detail = ensure_surface(app)
        safe_acts = [a for a in actions if is_safe_to_run(a)]
        app_records.append({
            "id": app,
            "was_opened": was_opened,
            "describe_ok": describe_ok,
            "action_count": len(actions),
            "safe_action_count": len(safe_acts),
            "detail": detail,
        })
        for a in safe_acts:
            results.append(call_action(app, a["name"], {}))
    summary = summarize(results)
    out = {
        "generated_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "mode": "smoke",
        "inventory": os.path.basename(INVENTORY),
        "candidate_apps": cands,
        "apps": app_records,
        "results": results,
        "summary": summary,
    }
    with open(RESULTS, "w") as f:
        json.dump(out, f, indent=2)
    print_short(out)
    all_ok = summary["failed"] == 0 and all(a["describe_ok"] for a in app_records)
    return 0 if all_ok else 2


def print_short(out):
    s = out["summary"]
    print("smoke: %d app(s) targeted" % len(out["candidate_apps"]))
    print("  actions run : %d" % s["actions_run"])
    print("  passed      : %d" % s["passed"])
    print("  failed      : %d" % s["failed"])
    if s["apps_list"]:
        print("  apps        : %s" % ", ".join(s["apps_list"]))
    print("results.json written.")


def dry_run():
    """Report the plan using inventory.json only -- no `yos` calls, no writes."""
    doc = load_inventory()
    cands = candidate_apps(doc)
    print("dry-run: would target %d app(s): %s" % (len(cands), ", ".join(cands)))
    total = 0
    for app in cands:
        rec = next((a for a in doc.get("apps", []) if a.get("id") == app), None)
        acts = rec.get("actions", []) if rec else []
        would = [a["name"] for a in acts if is_safe_to_run(a)]
        total += len(would)
        print("  %-22s %d action(s) in inventory -> would call: %s"
              % (app, len(acts), ", ".join(would) if would else "(none)"))
    print("dry-run: %d action(s) would pass the gate; no `yos` calls made." % total)
    return 0


def selftest():
    """Unit-test the pure decision functions (no I/O, no side effects)."""
    a = lambda g, args, n: {"grade": g, "required_args": args, "name": n}
    assert is_safe_to_run(a("safe", [], "status")) is True
    assert is_safe_to_run(a("safe", ["channel"], "check_update")) is False      # required arg
    assert is_safe_to_run(a("standard", [], "refresh_apps")) is False            # standard
    assert is_safe_to_run(a("sensitive", [], "lock")) is False                   # sensitive
    assert is_safe_to_run(a("dangerous", [], "files_delete")) is False           # dangerous
    assert is_safe_to_run(a("safe", [], "send_email")) is False                  # forbidden verb
    assert is_safe_to_run(a("safe", [], "set_do_not_disturb")) is False          # forbidden verb
    assert is_safe_to_run(None) is False

    s = summarize([{"app": "a", "ok": True}, {"app": "a", "ok": False}, {"app": "b", "ok": True}])
    assert s == {"apps": 2, "actions_run": 3, "passed": 2, "failed": 1,
                 "apps_list": ["a", "b"]}, s

    txt = ("  act: status()  [safe, settles on return]\n"
           "  act: send(x)  [standard, settles on return]\n")
    acts = parse_actions(txt)
    assert len(acts) == 2 and acts[0]["name"] == "status" and acts[0]["grade"] == "safe"
    assert acts[1]["required_args"] == ["x"]

    doc = {"apps": [{"id": "app-notes"}, {"id": "weather"}],
           "surfaces_not_up": [{"id": "app-email"}]}
    assert candidate_apps(doc) == ["app-notes", "app-email"]

    print("selftest: OK  (is_safe_to_run, summarize, parse_actions, candidate_apps)")
    return 0


def main(argv):
    if "--selftest" in argv:
        return selftest()
    if "--dry-run" in argv:
        return dry_run()
    return run_smoke()


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
