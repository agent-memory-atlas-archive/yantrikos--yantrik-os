#!/usr/bin/env python3
"""There is one calendar.

This machine had two. The Calendar app and the control surface used `calendar-service`, one JSON
file per event under `~/.local/share/yantrik/calendar/`; the built-in companion's own calendar
tools kept a SQLite table of their own, local-first, with optional Google sync. Neither read the
other. "Put it on my calendar", asked of the mind, landed where the app would never show it, and
an appointment made in the app was invisible to the mind's own tools.
`design/calendar-2026-09-20.md` found that and did not fix it; this probe is how the fix is
checked from outside.

It is not a probe of one app. It crosses the two ends deliberately: every event is made by one
side and looked for from the other, and from the files on disk, which are the thing both sides
are supposed to be talking about.

The mind's tools are reached over `companion.sock`, not through a conversation. The shell serves
`companion.tool {name, args}` there (`crates/yantrik-ui/src/companion_rpc.rs`), which runs one
tool by name with no language model in the loop — so what is measured here is the tool, not a
model's willingness to choose it.

`lib.py` has no helper for that socket. The one below is local to this probe and should move into
`lib.py` once a second probe wants it: it is the same newline-delimited JSON-RPC every service on
this machine speaks, and `C:\\Users\\sync\\tour-frames\\wake.py` has been framing it by hand for
the same reason.
"""

import datetime
import json
import os
import pathlib
import socket
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import lib  # noqa: E402

ONE_JOB = ("There is one calendar: an appointment the mind makes is one the Calendar app shows, "
           "an appointment made in the app is one the mind's own tools can read, and either can "
           "remove it.")

APP = "calendar"
APP_BIN = "/opt/yantrik/bin/yantrik-calendar"
SERVICE_BIN = "/opt/yantrik/bin/calendar-service"
STORE = pathlib.Path.home() / ".local/share/yantrik/calendar"
SERVICE_SOCK = lib.SOCKET_DIR / "calendar.sock"
COMPANION_SOCK = lib.SOCKET_DIR / "companion.sock"

# Today, because `calendar_today` is one of the two tools that has to see the app's event and it
# only ever looks at today. The store is snapshotted and restored, so a day that already has the
# user's own appointments on it is not a problem.
TODAY = datetime.date.today()

# Titles nothing else on this machine would write.
MIND_TITLE = "conformance-one-calendar-from-the-mind"
APP_TITLE = "conformance-one-calendar-from-the-app"


# ── The mind's own socket ────────────────────────────────────────────────────
#
# Belongs in lib.py when something else needs it. Kept here while this is the only caller, so the
# toolkit does not grow a helper with one user.

class CompanionUnreachable(RuntimeError):
    """The shell is not serving the companion, so none of the mind's tools can be run."""


def companion_call(method, params, timeout=180):
    """One newline-delimited JSON-RPC round trip to `companion.sock`.

    The framing is the one every service on this machine uses: one JSON object, one newline, one
    JSON object back. The timeout is generous because the companion worker is a single lane — a
    tool call arriving while an answer is being generated waits for the whole answer.
    """
    if not COMPANION_SOCK.exists():
        raise CompanionUnreachable("no %s on this machine" % COMPANION_SOCK)
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.settimeout(timeout)
    try:
        sock.connect(str(COMPANION_SOCK))
        request = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params})
        sock.sendall((request + "\n").encode())
        buf = b""
        while not buf.endswith(b"\n"):
            chunk = sock.recv(65536)
            if not chunk:
                break
            buf += chunk
    except OSError as exc:
        raise CompanionUnreachable("%s: %s" % (COMPANION_SOCK, exc))
    finally:
        sock.close()
    if not buf.strip():
        raise CompanionUnreachable("the companion closed the connection without answering")
    return json.loads(buf.decode("utf-8", "replace"))


def tool(name, **args):
    """Run one of the mind's tools by name. Always a dict, whichever way it went.

        text     str          what the tool said
        error    str or None  why it could not be run at all

    A tool that refuses says so in `text`: these tools answer prose, which is what they were
    written to do. `error` is the transport or the registry — no such tool, the worker gone.
    """
    try:
        reply = companion_call(
            "companion.tool",
            {"name": name, "args": args, "timeout_ms": 90_000},
        )
    except CompanionUnreachable as exc:
        return {"text": "", "error": str(exc)}
    except Exception as exc:  # noqa: BLE001 - a crash here is a result, not a stack trace
        return {"text": "", "error": "%s: %s" % (type(exc).__name__, exc)}
    if isinstance(reply, dict) and reply.get("error"):
        message = reply["error"]
        return {"text": "", "error": message.get("message") if isinstance(message, dict) else str(message)}
    result = (reply or {}).get("result") or {}
    return {"text": str(result.get("result", "")), "error": None}


# ── The store on disk, which is what both ends are talking about ─────────────

def records():
    """Every event file the calendar store holds, parsed."""
    out = []
    if not STORE.exists():
        return out
    for path in sorted(STORE.glob("*.json")):
        try:
            record = json.loads(path.read_text())
        except (OSError, ValueError):
            continue
        if isinstance(record, dict):
            record["_file"] = path.name
            out.append(record)
    return out


def stored():
    return {r.get("title", r["_file"][:-5]): r for r in records()}


def reported_id(text):
    """The id a create tool answered with, from its `ID: ...` line."""
    for line in (text or "").splitlines():
        if line.startswith("ID:"):
            return line[3:].strip()
    return None


def iso(day):
    return day.isoformat()


def stop_the_service():
    """The calendar service down, and its stale socket gone.

    So that the next thing to need it has to start it, which is the check that "on demand" is a
    mechanism and not a caption. The app is left alone.
    """
    lib.kill_app(SERVICE_BIN)
    for leftover in (SERVICE_SOCK, lib.SOCKET_DIR / "calendar.pid"):
        try:
            leftover.unlink(missing_ok=True)
        except OSError:
            pass


def app_sees():
    """Titles the app publishes for the selected day, after making it read the store again.

    The month forward and back first, deliberately. `refresh` in the app only re-reads when the
    visible range has changed, so an event written or removed by something else while the app sat
    on one month would still be on screen — and a check that read it would be measuring the app's
    cache rather than the calendar. Stepping the month twice changes the range twice and puts it
    back where it was.

    Two values: the titles, and the whole state block, so a check can show what else the app
    thought was true at the same moment — the month's count, the days it marked, its notice.
    """
    lib.act(APP, "show_month", direction="next")
    lib.act(APP, "show_month", direction="previous")
    lib.act(APP, "select_day", day=TODAY.day)
    time.sleep(1)
    view = lib.state(APP)
    return [e.get("title") if isinstance(e, dict) else e
            for e in view.get("events_on_selected_day") or []], view


def run():
    with lib.Probe("one-calendar", ONE_JOB) as probe:
        probe.note("processes_before", lib.running(APP_BIN) + lib.running(SERVICE_BIN))
        probe.note("store_dir", str(STORE))
        service_was_running = bool(lib.running(SERVICE_BIN))
        app_was_running = bool(lib.running(APP_BIN))
        probe.note("calendar_service_running_before", service_was_running)
        probe.note("calendar_app_running_before", app_was_running)

        store = lib.preserved(STORE)
        with store:
            probe.note("store_before", store.listing())

            # Nothing below may be inherited from a service somebody else started.
            stop_the_service()

            # ── 1. The mind puts something on the calendar ───────────────────
            made = tool("calendar_create_event",
                        summary=MIND_TITLE,
                        start="%sT14:00:00" % iso(TODAY),
                        end="%sT15:00:00" % iso(TODAY))
            probe.check(
                "the mind's calendar_create_event tool can be run at all",
                made["error"] is None,
                contract=1, evidence={"error": made["error"], "socket": str(COMPANION_SOCK)})

            lib.wait_for(lambda: MIND_TITLE in stored(), timeout=20)
            on_disk = stored().get(MIND_TITLE)
            mind_id = reported_id(made["text"])
            probe.check(
                "an event the mind creates is a file in the calendar store, the one the app reads",
                on_disk is not None,
                contract=2, evidence={"answer": made["text"], "titles_on_disk": sorted(stored()),
                                      "store_dir": str(STORE)})
            probe.check(
                "the create tool answers with the id the calendar stored it under, not one of its own",
                bool(mind_id) and on_disk is not None and mind_id == on_disk.get("id"),
                contract=3, evidence={"answered": mind_id,
                                      "id_in_store": (on_disk or {}).get("id"),
                                      "file": (on_disk or {}).get("_file"),
                                      "answer": made["text"]})
            probe.check(
                "the calendar service was started on demand by the tool that needed it",
                bool(lib.running(SERVICE_BIN)) and SERVICE_SOCK.exists(),
                contract=8, evidence={"service_was_running_before": service_was_running,
                                      "service_processes": lib.running(SERVICE_BIN),
                                      "socket": str(SERVICE_SOCK),
                                      "socket_exists": SERVICE_SOCK.exists()})

            # ── 2. The app shows it ──────────────────────────────────────────
            opened = lib.open_app(APP, expect_process=APP_BIN, window_words=("calendar",),
                                  timeout=45)
            probe.check(
                "the Calendar app opens",
                bool(opened["processes"]) and opened["surface_up"],
                contract=1, evidence=opened)

            titles, view = app_sees()
            probe.check(
                "the Calendar app shows the appointment the mind made, on the day it was made for",
                any(MIND_TITLE in str(t) for t in titles)
                and TODAY.day in {d.get("day") for d in view.get("days_with_events") or []
                                  if isinstance(d, dict)},
                contract=2, evidence={"events_on_selected_day": titles,
                                      "days_with_events": view.get("days_with_events"),
                                      "events_this_month": view.get("events_this_month"),
                                      "notice": view.get("notice")})

            # ── 3. The app puts something on the calendar ────────────────────
            added = lib.act(APP, "add_event", title=APP_TITLE, date=iso(TODAY), time="16:00")
            lib.wait_for(lambda: APP_TITLE in stored(), timeout=20)
            app_record = stored().get(APP_TITLE)
            probe.check(
                "the app's add_event still writes to the store",
                app_record is not None,
                contract=2, evidence={"action": added.get("result"),
                                      "refused": added.get("refused"),
                                      "titles_on_disk": sorted(stored())})

            # ── 4. And the mind's own tools can read it ──────────────────────
            listed = tool("calendar_list_events", start_date=iso(TODAY), end_date=iso(TODAY))
            probe.check(
                "the mind's calendar_list_events returns the appointment made in the app",
                listed["error"] is None and APP_TITLE in listed["text"],
                contract=2, evidence={"answer": listed["text"], "error": listed["error"],
                                      "id_in_store": (app_record or {}).get("id")})
            probe.check(
                "and it names it by the id the store gave it, which is the id a delete takes",
                app_record is not None and str(app_record.get("id")) in listed["text"],
                contract=3, evidence={"id_in_store": (app_record or {}).get("id"),
                                      "answer": listed["text"]})

            today_answer = tool("calendar_today")
            probe.check(
                "the mind's calendar_today sees both of today's appointments, whichever end made them",
                today_answer["error"] is None
                and APP_TITLE in today_answer["text"] and MIND_TITLE in today_answer["text"],
                contract=2, evidence={"answer": today_answer["text"],
                                      "error": today_answer["error"],
                                      "titles_on_disk": sorted(stored())})

            # ── 5. A delete by the mind is a delete everywhere ───────────────
            removed = tool("calendar_delete_event", event_id=mind_id or "")
            lib.wait_for(lambda: MIND_TITLE not in stored(), timeout=20)
            probe.check(
                "an event the mind deletes is gone from the store",
                MIND_TITLE not in stored(),
                contract=2, evidence={"answer": removed["text"], "error": removed["error"],
                                      "titles_on_disk": sorted(stored())})

            titles_after, view_after = app_sees()
            probe.check(
                "and the Calendar app stops showing it",
                not any(MIND_TITLE in str(t) for t in titles_after),
                contract=2, evidence={"events_on_selected_day": titles_after,
                                      "days_with_events": view_after.get("days_with_events"),
                                      "notice": view_after.get("notice")})

            # ── 6. A delete that cannot happen is said, not fabricated ───────
            #
            # The old tool answered "Event ... deleted locally. Will sync to Google when
            # connection is restored." for an id that was never on this machine.
            phantom = tool("calendar_delete_event", event_id="conformance-no-such-event")
            probe.check(
                "deleting something that is not there is refused in the calendar's own words",
                phantom["error"] is None
                and "NOT deleted" in phantom["text"]
                and "conformance-no-such-event" in phantom["text"],
                contract=4, evidence={"answer": phantom["text"], "error": phantom["error"]})

            # ── Put the machine back ─────────────────────────────────────────
            #
            # Through the mind's tool rather than by deleting the file, because that is one more
            # crossing: the app made this one.
            if app_record:
                cleanup = tool("calendar_delete_event", event_id=str(app_record.get("id")))
                lib.wait_for(lambda: APP_TITLE not in stored(), timeout=20)
                probe.note("cleanup_delete", cleanup["text"] or cleanup["error"])

            # Only what this probe started. A Calendar window the person already had open is
            # theirs, and `open_app` focuses one rather than starting a second.
            if not app_was_running:
                lib.kill_app(APP_BIN)
            if not service_was_running:
                stop_the_service()

        probe.note("store_after", store.listing(store.after))
        probe.check(
            "the calendar store is left exactly as it was found",
            not store.differences(),
            contract="leave-as-found",
            evidence={"before": store.listing(), "after": store.listing(store.after),
                      "differences": store.differences() or "none"})

        leftover = lib.running(APP_BIN) + lib.running(SERVICE_BIN)
        probe.note("processes_after", leftover)
        probe.check(
            "no calendar process is left running that was not running before",
            leftover == probe.notes["processes_before"],
            contract="leave-as-found",
            evidence={"before": probe.notes["processes_before"], "after": leftover})

        probe.note("companion_socket_helper", {
            "where": "local to this probe",
            "what": "newline-delimited JSON-RPC over %s, calling companion.tool" % COMPANION_SOCK,
            "should_move_to_lib": True,
            "why": "lib.py speaks the same framing to every app and service through `yos`, but "
                   "has nothing for the companion's own socket. The second probe that wants to "
                   "run one of the mind's tools without a language model should find it there "
                   "rather than write this again — the two copies of verify_calendar.py are why "
                   "lib.py exists.",
        })


if __name__ == "__main__":
    run()
