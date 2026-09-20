#!/usr/bin/env python3
"""Calendar's one job: keep an appointment.

An event you add is written to the calendar store on disk, is shown by the app, and is
still there when the app comes back — and when it cannot be kept, the calendar says so
instead of answering success.

Every claim below is checked against the store on disk or against a process the probe did
not start, never against the action's own answer. The audit's finding was exactly that
answer: `add_event` returned `{"added": "audit", "on": "..."}` for an event that existed
on no day. `design/calendar-2026-09-20.md` has the four faults underneath it.
"""

import json
import os
import pathlib
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import lib  # noqa: E402

ONE_JOB = ("Keep an appointment: an event you add is on disk, is shown by the app, and is "
           "still there after a restart.")

APP = "calendar"
APP_BIN = "/opt/yantrik/bin/yantrik-calendar"
SERVICE_BIN = "/opt/yantrik/bin/calendar-service"
STORE = pathlib.Path.home() / ".local/share/yantrik/calendar"
SERVICE_SOCK = lib.SOCKET_DIR / "calendar.sock"

# A date far enough from today that the check is about the calendar and not about today's
# month boundary, and titles nothing else on this machine would write.
DATE = "2026-09-24"
TITLE = "conformance-launch-review"
LATE_TITLE = "conformance-evening-walk"
LATE_DATE = "2026-09-26"
REFUSED_TITLE = "conformance-should-not-exist"


def stored():
    """Every event the store holds, as {title: record}. The witness for point 2."""
    out = {}
    if not STORE.exists():
        return out
    for path in sorted(STORE.glob("*.json")):
        try:
            record = json.loads(path.read_text())
        except (OSError, ValueError):
            continue
        record["_file"] = path.name
        out[record.get("title", path.stem)] = record
    return out


def stop_everything():
    """The app and the service down, and the service's stale socket gone."""
    lib.kill_app(APP_BIN)
    lib.kill_app(SERVICE_BIN)
    for leftover in (SERVICE_SOCK, lib.SOCKET_DIR / "calendar.pid"):
        try:
            leftover.unlink(missing_ok=True)
        except OSError:
            pass


def open_calendar():
    return lib.open_app(APP, expect_process=APP_BIN, window_words=("calendar",), timeout=45)


with lib.Probe(APP, ONE_JOB) as probe:
    probe.note("processes_before", lib.running(APP_BIN) + lib.running(SERVICE_BIN))
    probe.note("windows_before", lib.toplevels())

    store = lib.preserved(STORE)
    with store:
        probe.note("store_before", store.listing())

        # A cold machine, so nothing below can be inherited from an earlier run.
        stop_everything()

        # ── 1. It opens ──────────────────────────────────────────────────────
        opened = open_calendar()
        probe.check(
            "it opens: a process exists and the compositor has its window",
            bool(opened["processes"]) and bool(opened["windows"]) and opened["surface_up"],
            contract=1, evidence=opened)
        probe.check(
            "a launch that worked adds nothing to the shell's failed_launches",
            opened["new_failed_launches_for_this_app"] == [],
            contract=1, evidence={"added_by_this_launch": opened["new_failed_launches"],
                                  "whole_list": opened["failed_launches"]})

        # ── 2. It does its one job, and the store agrees ─────────────────────
        before_titles = sorted(stored())
        added = lib.act(APP, "add_event", title=TITLE, date=DATE, time="14:00")
        lib.wait_for(lambda: TITLE in stored(), timeout=15)
        after = stored()
        record = after.get(TITLE)
        probe.check(
            "add_event writes the event to the store on disk",
            record is not None,
            contract=2, evidence={"titles_before": before_titles, "titles_after": sorted(after),
                                  "record": record, "store_dir": str(STORE)})

        probe.check(
            "the service it needs is started on demand, not assumed",
            bool(lib.running(SERVICE_BIN)) and SERVICE_SOCK.exists(),
            contract=8, evidence={"service_processes": lib.running(SERVICE_BIN),
                                  "socket": str(SERVICE_SOCK), "socket_exists": SERVICE_SOCK.exists()})

        # ── 3. The action reports what happened ──────────────────────────────
        stored_id = (record or {}).get("id")
        answered_id = (added.get("result") or {}).get("id")
        probe.check(
            "add_event answers with the id it was stored under",
            bool(answered_id) and answered_id == stored_id,
            contract=3, evidence={"action_result": added.get("result"),
                                  "id_in_store": stored_id,
                                  "file_in_store": (record or {}).get("_file"),
                                  "accepted": added.get("accepted"), "settled": added.get("settled")})

        # ── 2 again, from the app's side: describe must agree with the disk ──
        lib.act(APP, "select_day", day=int(DATE[-2:]))
        time.sleep(1)
        view = lib.state(APP)
        days = {d.get("day"): d.get("events") for d in view.get("days_with_events") or []
                if isinstance(d, dict)}
        on_day = [e.get("title") if isinstance(e, dict) else e
                  for e in view.get("events_on_selected_day") or []]
        probe.check(
            "describe shows the event the store holds",
            view.get("events_this_month", 0) >= 1 and int(DATE[-2:]) in days
            and any(TITLE in str(t) for t in on_day),
            contract=2, evidence={"events_this_month": view.get("events_this_month"),
                                  "days_with_events": view.get("days_with_events"),
                                  "events_on_selected_day": on_day,
                                  "titles_on_disk": sorted(after)})

        # A 23:30 event used to build an end of T24:30:00, which is not a time.
        lib.act(APP, "add_event", title=LATE_TITLE, date=LATE_DATE, time="23:30")
        lib.wait_for(lambda: LATE_TITLE in stored(), timeout=15)
        late = stored().get(LATE_TITLE) or {}
        end = str(late.get("end", ""))
        probe.check(
            "a late event is clamped to a real time, not given a 24:30 end",
            bool(end) and "T24:" not in end and end <= LATE_DATE + "T23:59:59",
            contract=3, evidence={"start": late.get("start"), "end": late.get("end")})

        # ── 6. It survives a restart ─────────────────────────────────────────
        killed = lib.kill_app(APP_BIN)
        reopened = open_calendar()
        time.sleep(1)
        after_restart = lib.state(APP)
        on_disk_after_restart = sorted(stored())
        probe.check(
            "what was made is still there after the app is killed and reopened",
            bool(reopened["processes"])
            and after_restart.get("events_this_month", 0) >= 2
            and TITLE in on_disk_after_restart and LATE_TITLE in on_disk_after_restart,
            contract=6, evidence={"killed": killed,
                                  "reopened_processes": reopened["processes"],
                                  "reopened_windows": reopened["windows"],
                                  "events_this_month": after_restart.get("events_this_month"),
                                  "titles_on_disk": on_disk_after_restart})

        # ── 4. Failure is said twice ─────────────────────────────────────────
        # The only way to check a refusal is to make saving impossible. One file in
        # /opt/yantrik/bin is renamed, inside a context manager that puts it back on the
        # way out, on an exception, on SIGTERM and from an atexit hook.
        stop_everything()
        titles_before_failure = sorted(stored())
        try:
            with lib.moved_aside(SERVICE_BIN):
                failure_open = open_calendar()
                refused = lib.act(APP, "add_event", title=REFUSED_TITLE,
                                  date="2026-09-25", time="09:00")
                time.sleep(1)
                notice = lib.state(APP).get("notice") or ""
                titles_after_failure = sorted(stored())
        except (FileNotFoundError, RuntimeError) as exc:
            probe.check("the failure case could be set up", False, contract=4,
                        evidence={"error": str(exc),
                                  "note": "needs passwordless sudo to rename one binary"})
            refused, notice, titles_after_failure = {}, "", titles_before_failure
            failure_open = {}

        probe.check(
            "with the service gone, add_event is refused in words the caller can read",
            refused.get("accepted") is False and bool(refused.get("refused"))
            and refused.get("refused") not in ("1", "0"),
            contract=4, evidence={"refusal": refused.get("refused"),
                                  "accepted": refused.get("accepted"),
                                  "opened_for_failure_case": failure_open.get("processes")})
        probe.check(
            "nothing was written while it was refusing",
            titles_after_failure == titles_before_failure,
            contract=4, evidence={"titles_before": titles_before_failure,
                                  "titles_after": titles_after_failure})
        probe.check(
            "the same failure is in describe.notice, not only in the caller's error",
            bool(notice.strip()),
            contract=4, evidence={"notice": notice, "refusal": refused.get("refused")})

        probe.check(
            "the binary that was renamed away is back",
            pathlib.Path(SERVICE_BIN).exists()
            and not pathlib.Path(SERVICE_BIN + ".conformance-hidden").exists(),
            contract="leave-as-found",
            evidence={SERVICE_BIN: pathlib.Path(SERVICE_BIN).exists()})

        # ── Put the machine back ─────────────────────────────────────────────
        stop_everything()

    probe.note("store_after", store.listing(store.after))
    probe.check(
        "the calendar store is left exactly as it was found",
        not store.differences(),
        contract="leave-as-found",
        evidence={"before": store.listing(), "after": store.listing(store.after),
                  "differences": store.differences() or "none"})

    leftover = lib.running(APP_BIN) + lib.running(SERVICE_BIN)
    probe.note("processes_after", leftover)
    probe.note("windows_after", lib.toplevels())
    probe.check(
        "no calendar process is left running that was not running before",
        leftover == probe.notes["processes_before"],
        contract="leave-as-found",
        evidence={"before": probe.notes["processes_before"], "after": leftover})
