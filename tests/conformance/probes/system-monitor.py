#!/usr/bin/env python3
"""System Monitor's one job: say what the machine is doing, and end a process when asked.

The audit's finding was the second half of that. `kill_process` — graded `dangerous` — answered
`{"killed": pid}` whether the service call, the local fallback, or neither had managed it, and
the person at the window was told nothing either way. The first half had the quieter fault: when
the service stopped answering, every reading moved to the app's own `sysinfo` in silence, and
the fallback's blind spots (no CPU model, no interface address) arrived looking like
measurements — which is where an earlier audit's `cpu_model: ""` came from.

So nothing here is believed on the action's word. The probe starts a child of its own, asks the
app to end it, and reads `/proc` to find out whether it is gone.

Two outcomes are acceptable for a `dangerous` action and they are not the same thing:

  * the app ran it, and the process table agrees with what it said;
  * the machine's ceiling refused it before the app ever saw it.

The VM's configured ceiling is `sensitive`, which is below `dangerous`, so a policy refusal is
the expected outcome there. It is recorded as its own outcome, and the assertion flips: a kill
that was refused must have killed nothing. What it cannot do is prove the kill path itself, and
the report says so rather than passing quietly — a point a probe does not check is unmeasured,
not met.
"""

import os
import pathlib
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import lib  # noqa: E402

ONE_JOB = ("Report what the machine is doing, naming where each reading came from — and when "
           "asked to end a process, end it and confirm it is gone, or say why it could not.")

APP = "system-monitor"
APP_BIN = "/opt/yantrik/bin/yantrik-system-monitor"
SERVICE_BIN = "/opt/yantrik/bin/system-monitor-service"
SERVICE_SOCK = lib.SOCKET_DIR / "system-monitor.sock"


def alive(pid):
    """Ground truth: is this pid a process that is still doing something?

    Read from `/proc`, not from the app, and not from `kill -0`, which cannot tell a zombie from
    a running process. A child of ours that has been killed but not yet waited on is still in
    the table with state `Z`: it has exited, and counting it as alive would call a successful
    kill a failure. The state is the field after the last `)`, because the second field is the
    executable's own name in parentheses and may itself contain spaces and parentheses.
    """
    try:
        stat = pathlib.Path("/proc/%d/stat" % pid).read_text()
    except OSError:
        return False
    tail = stat[stat.rfind(")") + 1:].split()
    return bool(tail) and tail[0] not in ("Z", "X", "x")


def sleeper():
    """A child of our own to end. This probe only ever kills what it made."""
    child = lib.spawn(["sleep", "120"])
    lib.wait_for(lambda: alive(child.pid), timeout=5.0)
    return child


def reap(child):
    """Take the child out of the table for good, however far the app got with it."""
    try:
        child.kill()
    except OSError:
        pass
    try:
        child.wait(timeout=5)
    except Exception:  # noqa: BLE001 - a child that will not be waited on is the next check's
        pass


def by_policy(answer):
    """A `dangerous` action turned away by the machine's ceiling rather than by the app.

    The gate's wording is fixed — "Permission denied: 'system-monitor.kill_process' is declared
    dangerous but max is sensitive" — and it is a different fact from the app refusing a pid.
    """
    text = (answer.get("refused") or "").lower()
    return "permission denied" in text or "declared dangerous" in text


with lib.Probe(APP, ONE_JOB) as probe:
    probe.note("processes_before", lib.running(APP_BIN))
    probe.note("service_before", lib.running(SERVICE_BIN))
    was_running = bool(lib.running(APP_BIN))

    # ── 1. It opens ──────────────────────────────────────────────────────────
    opened = lib.open_app(APP, expect_process=APP_BIN, window_words=("system monitor",))
    probe.check(
        "it opens: a process exists, the compositor has its window, and the surface answers",
        bool(opened["processes"]) and bool(opened["windows"]) and opened["surface_up"],
        contract=1, evidence=opened)
    probe.check(
        "this launch added nothing to the shell's failed_launches",
        not opened["new_failed_launches_for_this_app"],
        contract=1, evidence=opened["new_failed_launches"])

    state = lib.state(APP)
    probe.note("describe", state)

    # ── 2. It says where its readings came from ──────────────────────────────
    #
    # The fallback to this process's own `sysinfo` is worth keeping — a monitor that goes blank
    # because a service died is worse than one reading the machine itself — but it is not the
    # same reading, so the surface has to say which one a caller is looking at.
    source = state.get("source")
    service_up = lib.running(SERVICE_BIN)
    probe.check(
        "describe names the source of its readings",
        source in ("service", "local"),
        contract=3, evidence={"source": source})
    probe.check(
        "degraded is a boolean and agrees with the source",
        isinstance(state.get("degraded"), bool)
        and state.get("degraded") == (source == "local"),
        contract=3, evidence={"source": source, "degraded": state.get("degraded")})
    probe.check(
        "a claim to be reading from the service is checkable against the service",
        source != "service" or bool(service_up),
        contract=3, evidence={"source": source, "service_processes": service_up,
                              "socket": SERVICE_SOCK.exists()})
    if state.get("degraded"):
        probe.check(
            "a degraded reading says why, and says it on screen as well",
            bool(state.get("degraded_reason")) and bool((state.get("notice") or "").strip()),
            contract=4, evidence={"degraded_reason": state.get("degraded_reason"),
                                  "notice": state.get("notice")})

    # A field nobody measured is absent, not empty. `cpu_model: ""` reads as a CPU whose model
    # is the empty string, which is how an audit came to report one.
    ips = [i.get("ip") for i in state.get("interfaces") or [] if isinstance(i, dict)]
    probe.check(
        "fields nothing measured are null, not empty strings that read like measurements",
        state.get("cpu_model") != "" and "" not in ips,
        contract=3, evidence={"cpu_model": state.get("cpu_model"), "interface_ips": ips})

    # ── 3. The dangerous one, checked against the process table ──────────────
    child = sleeper()
    pid = child.pid
    probe.check(
        "the child this probe started is running before anything is asked of the app",
        alive(pid), contract=2, evidence={"pid": pid})

    killed = lib.act(APP, "kill_process", pid=pid)
    if killed.get("accepted"):
        # The app confirms against the table before it answers, so this is a second opinion
        # rather than a wait. On a refusal there is nothing to wait for; a refusal that killed
        # something anyway would have done it by the time the call returned.
        lib.wait_for(lambda: not alive(pid), timeout=5.0)
    survived = alive(pid)
    evidence = {"pid": pid, "accepted": killed.get("accepted"), "result": killed.get("result"),
                "refused": killed.get("refused"), "alive_afterwards": survived}

    if by_policy(killed):
        evidence["outcome"] = "refused by the machine's ceiling"
        probe.note("kill_path_exercised", False)
        probe.check(
            "a dangerous action refused by the ceiling ends nothing",
            survived, contract=9, evidence=evidence)
        probe.check(
            "the refusal is in words the caller can read, not \"1\"",
            bool(killed.get("refused")) and killed.get("refused") not in ("1", "0"),
            contract=4, evidence=evidence)
    elif killed.get("accepted"):
        evidence["outcome"] = "the app ran it"
        probe.note("kill_path_exercised", True)
        answer = killed.get("result") or {}
        probe.check(
            "the process the app said it ended is gone from /proc",
            not survived, contract=2, evidence=evidence)
        probe.check(
            "the answer reports what was observed: the signal, the path, and that it exited",
            answer.get("exited") is True and answer.get("via") in ("service", "local")
            and answer.get("signal") == "SIGTERM",
            contract=3, evidence=answer)
    else:
        # The app itself would not do it. Allowed — a process may decline SIGTERM — but then the
        # table has to agree, and the reason has to be readable.
        evidence["outcome"] = "the app refused it"
        probe.note("kill_path_exercised", True)
        probe.check(
            "an app that says it did not end the process has not ended it",
            survived, contract=3, evidence=evidence)
        probe.check(
            "the refusal is in words the caller can read, not \"1\"",
            bool(killed.get("refused")) and killed.get("refused") not in ("1", "0"),
            contract=4, evidence=evidence)
    probe.note("kill_own_child", evidence)
    reap(child)

    # ── 4. A pid that does not exist ─────────────────────────────────────────
    #
    # A pid of this probe's own that has already been reaped: nothing else can be holding it,
    # and it is the case the old local fallback swallowed — `sys.process(pid)` returned `None`,
    # the kill did nothing at all, and the action reported the same success as a real one.
    spent = sleeper()
    spent_pid = spent.pid
    reap(spent)
    probe.check(
        "the pid used for the no-such-process case really is free",
        not alive(spent_pid), contract=2, evidence={"pid": spent_pid})

    missing = lib.act(APP, "kill_process", pid=spent_pid)
    missing_evidence = {"pid": spent_pid, "accepted": missing.get("accepted"),
                        "result": missing.get("result"), "refused": missing.get("refused"),
                        "outcome": "refused by the machine's ceiling" if by_policy(missing)
                        else "answered by the app"}
    probe.note("kill_missing_pid", missing_evidence)
    probe.check(
        "ending a pid that does not exist is refused, never reported as a kill",
        missing.get("accepted") is not True, contract=9, evidence=missing_evidence)
    if not by_policy(missing):
        probe.check(
            "the refusal names the reason: there is no such process",
            "no process" in (missing.get("refused") or "").lower(),
            contract=3, evidence=missing_evidence)
        probe.check(
            "the same failure is on screen, not only in the caller's error",
            bool((lib.state(APP).get("notice") or "").strip()),
            contract=4, evidence={"notice": lib.state(APP).get("notice"),
                                  "refusal": missing.get("refused")})

    # ── 5. The pids it will not touch ────────────────────────────────────────
    init = lib.act(APP, "kill_process", pid=1)
    probe.note("kill_init", {"accepted": init.get("accepted"), "refused": init.get("refused")})
    probe.check(
        "pid 1 is refused",
        init.get("accepted") is not True, contract=9,
        evidence={"refused": init.get("refused"), "result": init.get("result")})
    probe.check(
        "pid 1 is still running, which is the only answer that matters here",
        alive(1), contract=9, evidence={"init_alive": alive(1)})

    # ── Put the machine back ─────────────────────────────────────────────────
    if not was_running:
        lib.kill_app(APP_BIN)
        time.sleep(1)
    leftover = lib.running(APP_BIN)
    probe.note("processes_after", leftover)
    probe.check(
        "no system monitor is left running that was not running before",
        leftover == probe.notes["processes_before"],
        contract="leave-as-found",
        evidence={"before": probe.notes["processes_before"], "after": leftover})
    probe.check(
        "both children this probe started are out of the process table",
        not alive(pid) and not alive(spent_pid),
        contract="leave-as-found", evidence={"child": pid, "spent": spent_pid})
