#!/usr/bin/env python3
"""A probe that hangs, to prove a timeout is a failure and not a skip.

An app that wedges its UI thread makes `describe` block; a probe waiting on it would hang.
The tempting reading of that is "could not be checked", and the honest one is "failed".
This probe sleeps past any sane timeout so the runner can be seen taking the honest one.

    python3 run.py --probes-dir selftest --app never-finishes --timeout 10   # expect exit 1

It also proves that a killed probe still restores what it moved: the `preserved` block
below is unwound by the SIGTERM handler in `lib`, so the scratch file it made is gone even
though the probe never reached the end of the `with`.
"""

import os
import pathlib
import sys
import tempfile
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import lib  # noqa: E402

ONE_JOB = "Deliberately never finish, so the runner can be seen failing a timeout."

SCRATCH = pathlib.Path(tempfile.gettempdir()) / "yantrik-conformance-timeout-selftest"

with lib.Probe("never-finishes", ONE_JOB) as probe:
    # Snapshotted before it is made, so restoring removes the directory as well as its
    # contents: a path that was not there on the way in is not there on the way out.
    with lib.preserved(SCRATCH) as scratch:
        probe.note("scratch_before", scratch.listing())
        SCRATCH.mkdir(exist_ok=True)
        (SCRATCH / "left-behind.txt").write_text("the timeout must still clean this up\n")
        probe.check("something was written that the runner's SIGTERM must undo", True,
                    evidence={"path": str(SCRATCH / "left-behind.txt")})
        # Longer than any timeout the gate would use.
        time.sleep(3600)
        probe.check("this line is never reached", False)
