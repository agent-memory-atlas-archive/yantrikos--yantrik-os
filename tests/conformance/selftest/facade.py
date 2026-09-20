#!/usr/bin/env python3
"""A probe that asserts something false, to prove the runner reports it.

A gate nobody has seen fail is a gate nobody has tested. This probe claims an app that is
certainly not on this machine is open, and checks it the way a real probe does — a process
and a compositor window. It must come out as FAIL with a nonzero exit code.

    python3 run.py --probes-dir selftest --app facade    # expect exit 1

Nothing here touches the machine's state.
"""

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import lib  # noqa: E402

ONE_JOB = "Deliberately fail, so the runner can be seen catching a failure."

with lib.Probe("facade", ONE_JOB) as probe:
    probe.check("this one is true, so a mixed report is exercised", True,
                evidence={"note": "the runner must still count the failures below"})

    processes = lib.running("/opt/yantrik/bin/yantrik-no-such-app")
    probe.check("it opens: a process exists and the compositor has its window",
                bool(processes) and lib.has_window("no-such-app"),
                contract=1,
                evidence={"processes": processes, "windows": lib.toplevels(),
                          "note": "asserted false on purpose"})

    probe.check("the store agrees with what the action claimed", False, contract=2,
                evidence={"claimed": {"saved": True},
                          "on_disk": [],
                          "note": "this is the audit's finding, asserted false on purpose"})
