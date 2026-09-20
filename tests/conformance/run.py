#!/usr/bin/env python3
"""The app conformance runner. Runs on the machine under test.

This is the content of the release gate in `design/next-focus-2026-09.md`: it drives
`describe`/`act`, checks the effects against something other than the action's own answer,
and exits nonzero. A mind may launch it and read the failures; it cannot waive one. There
is no flag that turns a failing check into a passing one — the only lever is
`expected-fail.json`, which requires a named app and a written reason, and which fails the
run if the app listed there starts passing.

    python3 run.py                          every probe
    python3 run.py --app calendar           one
    python3 run.py --list                   what would run
    python3 run.py --json report.json       write the machine-readable report too

A probe is any executable Python file in `probes/`. It exits 0 when the app meets the
contract and nonzero when it does not, and prints a JSON object as the last thing on
stdout. That is the whole protocol; `README.md` has it in full. The runner reads the JSON
for detail and the exit code for the verdict, and says so when the two disagree.
"""

import argparse
import json
import os
import pathlib
import signal
import subprocess
import sys
import time

HERE = pathlib.Path(__file__).resolve().parent
DEFAULT_TIMEOUT = 300

# What a green run does not mean. Printed with every run, because a gate that overstates
# its own evidence is the failure it was built to catch.
LIMITS = [
    "A mapped toplevel is not a visible window. wlrctl reports that a surface exists and "
    "has a title; nothing here proves anything was painted, or that it is on top, or the "
    "right size.",
    "Nothing here looks at pixels. An app that meets every check can still be laid out "
    "wrongly, as the image viewer was while passing its own checks by hand.",
    "An app with no control surface can only be checked for a process and a window, which "
    "is the weakest evidence in this suite. Probes that rely on it say so.",
    "These probes check the contract points that apply to each app. A point a probe does "
    "not check is unmeasured, not met.",
]

STATUS_PASS = "pass"
STATUS_FAIL = "fail"
STATUS_XFAIL = "expected fail"
STATUS_UNEXPECTED_PASS = "unexpected pass"


# ── Finding probes ───────────────────────────────────────────────────────────

def discover(probes_dir):
    """Every probe file, by app name. A file starting with `_` is a helper, not a probe."""
    found = {}
    if not probes_dir.is_dir():
        return found
    for path in sorted(probes_dir.glob("*.py")):
        if path.name.startswith("_"):
            continue
        found[path.stem] = path
    return found


def load_expected_fail(path):
    """`[{"app": "weather", "reason": "..."}]`, or `{}` when the file is absent.

    A reason is required. An entry without one is a way to switch a check off quietly,
    which is the thing this suite exists to prevent, so the runner refuses to start.
    """
    if not path.exists():
        return {}
    try:
        data = json.loads(path.read_text())
    except ValueError as exc:
        sys.exit("conformance: %s is not valid JSON (%s)" % (path, exc))
    entries = data.get("expected_fail", data) if isinstance(data, dict) else data
    if isinstance(entries, dict):  # {"app": "reason"} is accepted as shorthand
        entries = [{"app": k, "reason": v} for k, v in entries.items()]
    out = {}
    for entry in entries:
        app = (entry or {}).get("app")
        reason = (entry or {}).get("reason")
        if not app or not reason:
            sys.exit("conformance: every entry in %s needs an app and a reason; got %r"
                     % (path, entry))
        out[app] = reason
    return out


# ── Reading what a probe said ────────────────────────────────────────────────

def last_json_object(text):
    """The last top-level JSON object in a probe's stdout, or None.

    Probes may print progress before their report — the other agents' probes were written
    before this runner existed and cannot be assumed to print only JSON — so the report is
    found by scanning backwards for a `{` that starts a parseable object.
    """
    decoder = json.JSONDecoder()
    starts = [i for i, ch in enumerate(text)
              if ch == "{" and (i == 0 or text[i - 1] == "\n")]
    if not starts or starts[0] != text.find("{"):
        starts = [i for i, ch in enumerate(text) if ch == "{"]
    for index in reversed(starts):
        try:
            value, _ = decoder.raw_decode(text[index:])
        except ValueError:
            continue
        if isinstance(value, dict):
            return value
    return None


def summarise(report, app):
    """Counts and failed names out of a probe report, whatever shape it came in.

    The contract is one JSON object. Its only required field is a verdict, and the runner
    reads several spellings of that, because four of these probes were written in parallel
    against a written protocol rather than against this code.
    """
    if not isinstance(report, dict):
        return {"checks_passed": None, "checks_total": None, "failed": [],
                "one_job": None, "claimed_pass": None}
    checks = report.get("checks")
    passed = total = None
    failed = []
    if isinstance(checks, list):
        total = len(checks)
        passed = 0
        for check in checks:
            if isinstance(check, dict):
                ok = check.get("passed", check.get("ok", check.get("pass")))
                name = check.get("name") or check.get("check") or "(unnamed check)"
            else:
                ok, name = bool(check), str(check)
            if ok:
                passed += 1
            else:
                failed.append(str(name))
    elif isinstance(checks, dict):
        # `{name: true}` and `{name: {"ok": true, "detail": ...}}` are both in use.
        total = len(checks)
        passed = 0
        for name, value in checks.items():
            if isinstance(value, dict):
                ok = value.get("passed", value.get("ok", value.get("pass")))
            else:
                ok = bool(value)
            if ok:
                passed += 1
            else:
                failed.append(str(name))
    counts = report.get("counts")
    if isinstance(counts, dict) and passed is None:
        passed, total = counts.get("passed"), counts.get("total")
    # `failures` is the spelling the self-contained probes used, written against the
    # protocol before this file existed. Both are read; neither is required.
    for key in ("failed", "failures", "failed_checks"):
        if not failed and isinstance(report.get(key), list):
            failed = [str(f) for f in report[key]]
    claimed = report.get("passed", report.get("ok", report.get("pass")))
    return {"checks_passed": passed, "checks_total": total, "failed": failed,
            "one_job": report.get("one_job") or report.get("job"),
            "claimed_pass": claimed if isinstance(claimed, bool) else None,
            "app": report.get("app") or app}


# ── Running one ──────────────────────────────────────────────────────────────

def run_probe(app, path, timeout, verbose=False):
    env = dict(os.environ)
    env["PYTHONPATH"] = os.pathsep.join(filter(None, [str(HERE), env.get("PYTHONPATH", "")]))
    if hasattr(os, "getuid"):  # the suite runs on the machine under test; a workstation
        env.setdefault("XDG_RUNTIME_DIR", "/run/user/%d" % os.getuid())  # can still list it
    env.setdefault("WAYLAND_DISPLAY", "wayland-0")
    env["PATH"] = "/opt/yantrik/bin" + os.pathsep + env.get("PATH", "")
    env["PYTHONUNBUFFERED"] = "1"

    # -P keeps the script's own directory off sys.path. A probe is named after its app, and two
    # apps are named after standard library modules: probes/calendar.py made `datetime.strptime`
    # import the probe instead of the library, and probes/email.py broke `import http.server` in
    # a DIFFERENT probe the day it was added, because every probe in this directory has the
    # directory first on its path. The next app called `queue` or `json` would have done it again.
    # lib.py is reached through PYTHONPATH above, so nothing needs the directory there.
    env["PYTHONSAFEPATH"] = "1"
    started = time.time()
    proc = subprocess.Popen([sys.executable, "-P", str(path)], cwd=str(HERE), env=env,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                            start_new_session=True)
    timed_out = False
    try:
        stdout, stderr = proc.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        timed_out = True
        # SIGTERM first and a real grace period: a probe holds the machine's state open
        # (a renamed binary, a staged folder) and its restoration runs on the way out.
        _signal_group(proc, signal.SIGTERM)
        try:
            stdout, stderr = proc.communicate(timeout=30)
        except subprocess.TimeoutExpired:
            _signal_group(proc, signal.SIGKILL)
            stdout, stderr = proc.communicate(timeout=30)
    duration = round(time.time() - started, 1)

    if verbose:
        sys.stdout.write(stdout)
        sys.stderr.write(stderr)

    report = last_json_object(stdout)
    facts = summarise(report, app)
    outcome = {
        "app": app, "probe": _short(path), "exit_code": proc.returncode,
        "duration_s": duration, "timed_out": timed_out,
        "one_job": facts["one_job"],
        "checks_passed": facts["checks_passed"], "checks_total": facts["checks_total"],
        "failed_checks": facts["failed"], "report": report,
        "stdout_tail": stdout.strip().splitlines()[-40:],
        "stderr_tail": stderr.strip().splitlines()[-40:],
        "problems": [],
    }
    if timed_out:
        outcome["problems"].append("timed out after %ds — a probe that does not finish is a "
                                   "failure, never a skip" % timeout)
    if report is None:
        outcome["problems"].append("printed no JSON report; only its exit code could be read")
    elif facts["claimed_pass"] is not None and facts["claimed_pass"] != (proc.returncode == 0):
        outcome["problems"].append(
            "its report says passed=%s and it exited %d; the exit code decides"
            % (facts["claimed_pass"], proc.returncode))
    # The exit code is the verdict. A probe that crashes exits nonzero and fails.
    outcome["probe_passed"] = proc.returncode == 0 and not timed_out
    return outcome


def _short(path):
    """A probe's path relative to the suite, or absolute when it lives outside it."""
    try:
        return str(path.relative_to(HERE))
    except ValueError:
        return str(path)


def _signal_group(proc, sig):
    try:
        os.killpg(os.getpgid(proc.pid), sig)
    except (ProcessLookupError, PermissionError, OSError):
        try:
            proc.send_signal(sig)
        except ProcessLookupError:
            pass


# ── The table ────────────────────────────────────────────────────────────────

def render(outcomes, limits=True):
    width = max([len("app")] + [len(o["app"]) for o in outcomes]) + 2
    status_width = max(len(STATUS_UNEXPECTED_PASS), len("result")) + 2
    lines = ["", "%-*s%-*s%-8s%s" % (width, "app", status_width, "result", "checks",
                                     "failed checks")]
    lines.append("-" * (width + status_width + 8 + 40))
    for outcome in outcomes:
        total, passed = outcome["checks_total"], outcome["checks_passed"]
        checks = "%s/%s" % (passed, total) if total is not None else "-"
        detail = ", ".join(outcome["failed_checks"]) or ""
        if outcome["status"] == STATUS_XFAIL:
            detail = "(%s)" % outcome["expected_fail_reason"]
        elif outcome["status"] == STATUS_UNEXPECTED_PASS:
            detail = "it is on the expected-fail list — remove it (%s)" % \
                     outcome["expected_fail_reason"]
        elif not detail and outcome["problems"]:
            detail = outcome["problems"][0]
        elif not detail and outcome["status"] == STATUS_PASS:
            detail = "-"
        lines.append("%-*s%-*s%-8s%s" % (width, outcome["app"], status_width,
                                         outcome["status"], checks, detail))
    lines.append("")
    for outcome in outcomes:
        if outcome["status"] in (STATUS_FAIL, STATUS_UNEXPECTED_PASS):
            lines.append("%s:" % outcome["app"])
            for problem in outcome["problems"]:
                lines.append("    %s" % problem)
            for name in outcome["failed_checks"]:
                lines.append("    failed: %s" % name)
            for line in outcome["stderr_tail"][-6:]:
                lines.append("    stderr: %s" % line)
            lines.append("")
    if limits:
        lines.append("What a green run does not prove:")
        for limit in LIMITS:
            lines.append("  - %s" % limit)
        lines.append("")
    return "\n".join(lines)


# ── Entry point ──────────────────────────────────────────────────────────────

def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--app", action="append", default=[],
                        help="run only this app's probe; may be given more than once")
    parser.add_argument("--list", action="store_true", help="print the probes and stop")
    parser.add_argument("--timeout", type=int, default=DEFAULT_TIMEOUT,
                        help="seconds for one probe (default %d)" % DEFAULT_TIMEOUT)
    parser.add_argument("--json", type=pathlib.Path, help="write the full report here")
    parser.add_argument("--probes-dir", type=pathlib.Path, default=HERE / "probes")
    parser.add_argument("--expected-fail", type=pathlib.Path, default=HERE / "expected-fail.json")
    parser.add_argument("--verbose", action="store_true",
                        help="stream each probe's own output as it runs")
    args = parser.parse_args(argv)

    probes = discover(args.probes_dir.resolve())
    expected_fail = load_expected_fail(args.expected_fail)

    if args.app:
        unknown = [a for a in args.app if a not in probes]
        if unknown:
            sys.exit("conformance: no probe for %s (have: %s)"
                     % (", ".join(unknown), ", ".join(sorted(probes)) or "none"))
        probes = {a: probes[a] for a in args.app}

    if args.list:
        for app, path in sorted(probes.items()):
            mark = "  (expected fail: %s)" % expected_fail[app] if app in expected_fail else ""
            print("%-22s %s%s" % (app, _short(path), mark))
        if args.probes_dir.resolve() == (HERE / "probes").resolve():
            for app in sorted(set(expected_fail) - set(discover(args.probes_dir.resolve()))):
                print("%-22s (on the expected-fail list with no probe)" % app)
        return 0

    if not probes:
        print("conformance: no probes in %s" % args.probes_dir)
        return 1

    started = time.time()
    outcomes = []
    for app, path in sorted(probes.items()):
        print("running %s ..." % app, flush=True)
        outcome = run_probe(app, path, args.timeout, verbose=args.verbose)
        reason = expected_fail.get(app)
        outcome["expected_fail_reason"] = reason
        if outcome["probe_passed"]:
            outcome["status"] = STATUS_UNEXPECTED_PASS if reason else STATUS_PASS
            if reason:
                outcome["problems"].append(
                    "listed in expected-fail.json as %r, but it passed; take it off the list "
                    "so the list cannot go stale" % reason)
        else:
            outcome["status"] = STATUS_XFAIL if reason else STATUS_FAIL
        outcomes.append(outcome)

    # An expected-fail entry for an app with no probe is the other way the list goes stale.
    # Only meaningful against the real probe directory: `--probes-dir selftest` is not a
    # statement that every app on the list has lost its probe.
    orphans = []
    if args.probes_dir.resolve() == (HERE / "probes").resolve():
        orphans = sorted(set(expected_fail) - set(discover(args.probes_dir.resolve())))
    bad = [o for o in outcomes if o["status"] in (STATUS_FAIL, STATUS_UNEXPECTED_PASS)]

    print(render(outcomes))
    for app in orphans:
        print("note: expected-fail.json lists %r and there is no probes/%s.py" % (app, app))

    report = {
        "generated_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "host": os.uname().nodename if hasattr(os, "uname") else "",
        "duration_s": round(time.time() - started, 1),
        "passed": not bad,
        "counts": {
            "probes": len(outcomes),
            "pass": sum(1 for o in outcomes if o["status"] == STATUS_PASS),
            "fail": sum(1 for o in outcomes if o["status"] == STATUS_FAIL),
            "expected_fail": sum(1 for o in outcomes if o["status"] == STATUS_XFAIL),
            "unexpected_pass": sum(1 for o in outcomes if o["status"] == STATUS_UNEXPECTED_PASS),
        },
        "expected_fail_without_a_probe": orphans,
        "limits": LIMITS,
        "probes": outcomes,
    }
    if args.json:
        args.json.parent.mkdir(parents=True, exist_ok=True)
        args.json.write_text(json.dumps(report, indent=2, default=str))
        print("report: %s" % args.json)

    print("%d probe(s): %d pass, %d fail, %d expected fail, %d unexpected pass"
          % (report["counts"]["probes"], report["counts"]["pass"], report["counts"]["fail"],
             report["counts"]["expected_fail"], report["counts"]["unexpected_pass"]))
    return 0 if not bad else 1


if __name__ == "__main__":
    sys.exit(main())
