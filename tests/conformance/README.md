# The app conformance suite

This is the test content of the release gate in `design/next-focus-2026-09.md` section 2, and
the first item of the order in `design/apps-plan-2026-09-20.md`. It drives `app.describe` and
`app.act` on a running machine, checks the effect against something other than the action's own
answer, and exits nonzero.

A mind may launch it and diagnose what it reports. A mind may not waive an assertion: there is no
flag that turns a failing check green, and the one lever there is — `expected-fail.json` — needs a
named app and a written reason, and fails the run if the app listed there starts passing.

It exists because Calendar and Images both answered success while doing nothing, and both were
found by hand with a script written twice (`verify_calendar.py`, `verify_images.py`). The two
copies had the same bug in each: `yos` reports a refusal by printing to stderr and exiting, so
`str(SystemExit)` is `"1"`, and both scripts recorded `"1"` where the refusal text should have
been. Contract point 4 is about that text. This suite captures it.

```
tests/conformance/
  run.py               the runner. Runs on the machine under test.
  run-on-vm.sh         pushes the suite to the test machine, runs it, brings the report back.
  lib.py               the toolkit a probe imports.
  probes/<app>.py      one probe per app.
  selftest/            two probes that must fail, so the gate can be seen working.
  expected-fail.json   apps whose probes are expected to fail today, with reasons.
```

## Running it

From a workstation, against the test machine:

```
tests/conformance/run-on-vm.sh                     every probe
tests/conformance/run-on-vm.sh --app calendar      one
tests/conformance/run-on-vm.sh --app calendar --app image-viewer
```

It stages the suite into a tarball, pushes it to a temp directory on the machine, runs `run.py`
there, writes the JSON report to `target/conformance/conformance-<timestamp>.json`, removes its
scratch directory, and exits with the runner's exit code. The helper scripts it goes through live
at `$YANTRIK_VM_TOOLS` (default `/c/Users/sync/tour-frames`); with neither present it falls back
to plain `ssh`/`scp` at `$YANTRIK_VM`.

On the machine itself:

```
python3 run.py                                  every probe
python3 run.py --list                           what would run
python3 run.py --app calendar --verbose         one, with its own output streamed
python3 run.py --json /tmp/report.json          write the machine-readable report too
python3 run.py --probes-dir selftest --timeout 20
```

The runner needs the session's environment, which `run-on-vm.sh` sets and which a shell on the
machine needs told:

```
export XDG_RUNTIME_DIR=/run/user/1000 WAYLAND_DISPLAY=wayland-0 PATH=/opt/yantrik/bin:$PATH
```

## The probe protocol

A probe is **any executable Python file in `probes/`**, named after the app it checks. The runner
starts it in a subprocess with a timeout and reads two things:

1. **Its exit code.** `0` means the app met the contract; anything else means it did not. This is
   the verdict — a probe that crashes exits nonzero and fails, and a probe that times out fails.
   Never a skip.
2. **A JSON object, as the last thing on stdout.** This is detail, not verdict. A probe may print
   whatever it likes before it; the runner scans backwards for the last parseable top-level
   object. A probe with no JSON still passes or fails on its exit code, with a note that only the
   exit code could be read.

The object's fields, all optional:

| field | meaning |
| --- | --- |
| `app` | the app's id. Defaults to the filename. |
| `one_job` | one sentence: what this app exists to do. |
| `passed` | the probe's own verdict. If it disagrees with the exit code, the exit code wins and the runner says so. |
| `checks` | a list of `{name, passed, severity, contract, evidence}`, or a `{name: bool}` map. This is what fills the `checks passed/total` column. |
| `failed` / `failures` | names of the checks that failed, when there is no `checks` list. |
| `notes` | evidence that is not a check: before and after listings, what was staged. |

Two spellings are read for the same thing because four probes were written in parallel against
this protocol rather than against `lib.py`: `passed` / `ok` / `pass` on a check, and `failed` /
`failures` / `failed_checks` at the top level. Probes need not import `lib.py`. Ones that do get
the toolkit below and a report in the canonical shape for free.

### What a probe owes the machine

The test machine is shared and stateful, with the user's own windows open on it. Every probe:

- **Leaves the machine as it found it.** Deletes what it created, restores what it moved, closes
  what it opened. `lib.preserved` snapshots a directory on the way in and restores it on the way
  out; the probe then asserts the before and after listings match, so "I cleaned up" is checked
  rather than claimed.
- **Restores even when it dies.** `lib` turns SIGTERM, SIGINT and SIGHUP into `SystemExit`, so a
  probe killed by the runner's timeout still unwinds its `with` blocks. `lib.moved_aside`
  additionally registers its restore with `atexit` before it moves anything.
- **Never restarts the session** and never touches `labwc` or `yantrik-ui`. The one write to
  `/opt/yantrik/bin` in this suite is `lib.moved_aside`, which renames a single service binary to
  force a failure and puts it back.
- **Records instead of raising.** A failed assertion appends a result and the probe carries on, so
  one failure does not hide the six after it.

## How to write one — the calendar probe, line by line

`probes/calendar.py` is the worked example. Its opening states the app's one job in a sentence,
which is the thing every check below is a consequence of:

> **Keep an appointment:** an event you add is written to the calendar store on disk, is shown by
> the app, and is still there when the app comes back — and when it cannot be kept, the calendar
> says so instead of answering success.

Then:

```python
import lib

with lib.Probe("calendar", ONE_JOB) as probe:
    probe.note("processes_before", lib.running(APP_BIN))

    store = lib.preserved(STORE)          # snapshot ~/.local/share/yantrik/calendar
    with store:
        opened = lib.open_app("calendar", expect_process=APP_BIN,
                              window_words=("calendar",))
        probe.check(
            "it opens: a process exists and the compositor has its window",
            bool(opened["processes"]) and bool(opened["windows"]) and opened["surface_up"],
            contract=1, evidence=opened)
        ...
    probe.check("the calendar store is left exactly as it was found",
                not store.differences(), contract="leave-as-found",
                evidence={"before": store.listing(), "after": store.listing(store.after)})
```

The `Probe` context manager writes the report and sets the exit code on the way out, including
when the body raises: an unhandled exception becomes a failed check named "the probe ran to the
end" rather than a stack trace and a lost report.

### What lib.py gives you

| | |
| --- | --- |
| `act(app, action, **args)` | an action, always a dict: `accepted`, `settled`, `result`, `summary`, `revision`, `refused`. `refused` holds the refusal **in the words the caller was given** — the point of the wrapper. |
| `describe(app)`, `state(app)`, `actions(app)` | the published view, its state block, its action names. Unreachable is a value, not an exception. |
| `running(pattern)`, `pids`, `kill_app` | processes by full command line, minus this one. Give it a path, not a word: `pgrep -f calendar` also matches the probe. |
| `toplevels()`, `has_window(*words)` | what the compositor says is mapped. |
| `open_app(name, expect_process=, window_words=)` | asks the shell to open an app and returns the evidence — process, window, surface, and what this launch added to `failed_launches`. |
| `spawn`, `run_and_wait` | start a binary in the session's Wayland environment; the second waits, for a handover launch. |
| `wait_for(predicate, timeout=)` | poll until something is true. |
| `sha256(path)`, `surface_up(app)` | |
| `preserved(path)` | snapshot a file or directory, restore it on the way out, and report `differences()`. |
| `moved_aside(path)` | rename one file away to force a failure, and put it back no matter how the probe ends. |
| `Probe` | the recorder: `check(name, passed, evidence=, severity=, contract=)`, `note(key, value)`. |

`severity=lib.ADVISORY` reports a check without failing the probe. It is for something this suite
genuinely cannot prove on this machine — never for softening a contract point that is simply not
met.

## The contract points, in checkable terms

From `design/apps-plan-2026-09-20.md`. A probe checks the points that apply to its app and says
which; a point a probe does not check is **unmeasured, not met**.

| # | The point | What a probe actually asserts |
| --- | --- | --- |
| 1 | **It opens.** | `pgrep` finds the app's binary **and** `wlrctl toplevel list` has its window **and** its `app-<id>.sock` answers `app.describe`. Never `accepted: true` alone. Plus: this launch added nothing to the shell's `failed_launches`. |
| 2 | **It does its one job, and the store agrees.** | The witness is on disk or in a service: the event's JSON file, the file's own bytes. Then `describe` is checked against that witness — not the other way round. |
| 3 | **Actions report what happened.** | The id in the action's answer is the id in the store's filename. `rotate` answers `file_unchanged: true` and the file's SHA-256 is identical either side. |
| 4 | **Failure is said twice.** | Force the failure (rename the service binary away), then: the action is refused with words the caller can read — not `"1"` — **and** the same reason is in `describe.state.notice`, **and** nothing was written while it refused. |
| 5 | **It takes work from outside.** | A path on the command line, or an `open` action; a second launch hands its file to the running window instead of opening a second one. |
| 6 | **It survives a restart.** | Kill the app, open it again, and what was made is still there. For an app that keeps nothing — a viewer — this weakens to restart-and-still-works, and the probe's evidence says so. |
| 7 | **No dead controls.** | Not checked here. It is a source lint, not a runtime probe. |
| 8 | **One owner per domain.** | Partly: that the service the app needs is started on demand, with the process and socket to show for it. |
| 9 | **Grades mean it.** | Partly, per app: that a `dangerous` action does not answer a success it did not observe. |
| — | **Leave as found.** | Before and after listings of everything the probe touched, compared, and reported as evidence. |

## expected-fail.json

```json
{"expected_fail": [{"app": "weather", "reason": "fixed in source, not yet deployed to the VM"}]}
```

An app listed here is reported as `expected fail (reason)` and does not break the exit code. The
list cannot go stale in either direction:

- an app listed here that **starts passing** is reported as `unexpected pass` and **fails the
  run** until it is taken off;
- an entry naming an app with no probe is reported as a note on every run.

An entry without a reason is refused before anything runs. It is not a place to park a known bug;
it is for a probe that cannot run yet.

## What this does not prove

Printed at the end of every run, because a gate that overstates its own evidence is the failure it
was built to catch.

- **A mapped toplevel is not a visible window.** `wlrctl` reports that a surface exists and has a
  title. Nothing here proves anything was painted, that the window is on top, or that it is the
  right size. `design/next-focus-2026-09.md` leaves window visibility unverified on purpose, and
  the shell's own bookkeeping is not a witness — it was the bug.
- **Nothing here looks at pixels.** The image viewer passed every check it was given by hand while
  drawing its picture 400 pixels wide in a corner of an empty window. Visual correctness is not in
  scope and no amount of green here implies it.
- **An app with no control surface** can only be checked for a process and a window, which is the
  weakest evidence in this suite. A probe that relies on it labels it.
- **`describe` is the app's own account of itself.** It is treated as a claim to be checked against
  the store, never as the witness. Where there is no store — a viewer — the file's own bytes stand
  in, and that is stated in the evidence.
- **A green run means the checks that were written passed.** It does not mean the app is correct.

## Proving the gate still bites

`selftest/` holds two probes that must fail. They are not in `probes/` because they are not apps.

```
python3 run.py --probes-dir selftest --app facade              # asserts something false
python3 run.py --probes-dir selftest --app never-finishes --timeout 20
```

Both must print `fail` and exit 1. `never-finishes` also proves that a probe killed on a timeout
still restores what it staged: it writes a scratch file inside a `preserved` block and sleeps for
an hour, and the file is gone after the runner kills it.

## A note on `failed_launches`

The shell's `failed_launches` is a running list and nothing resets it. A probe that kills an app —
which the restart check has to do — leaves a `signal: 15 (SIGTERM)` entry behind, and the next
probe would read it as its own launch failing. So `lib.open_app` records the list before it asks
and reports `new_failed_launches_for_this_app`, and probes assert on that. This was found by the
image viewer's probe failing on an entry the calendar's probe had made.
