#!/usr/bin/env python3
"""Notifications' one job: be the only place a notification on this machine can land, and still
have it after a restart.

There were four notification systems on this desktop and none knew about the others.

`mako` held `org.freedesktop.Notifications`, so `notify-send`, Chromium and every ordinary Linux
program drew popups in mako's style that nothing of ours could see. The shell ALSO implemented
that interface, from a thread of its own, and raced mako for the name — whichever won got the
machine's notifications and the other half went dark; the audit of 17 September caught mako
winning by a few hundred milliseconds. The shell kept its own private JSON file in `~/.yantrik/`,
fed by screenshots and focus mode, that no service and no mind could read. And this service —
autostarted, with five methods — had an in-memory `Vec` that **nothing in the whole tree ever
wrote to**, so it was always empty and nobody noticed.

So the checks below are all one question asked five ways: did it land in the ONE store, and is it
still there. Both doors are exercised, because the point is that they are the same door:
`notify-send` (freedesktop, the way every foreign program arrives) and `yos notify` (ours). The
restart is the check the old service could not have passed at all.

Leave-as-found is by DISMISSING what this probe posted, not by restoring the file. The store is
live and shared: a notification that arrived from somewhere else while this ran is real, and
putting an old file back over it would destroy it. Dismissed notifications stay in the file for a
week by design, so "removed" here means "no longer showing", which is what dismiss means
everywhere else on this machine.
"""

import json
import os
import pathlib
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import lib  # noqa: E402

ONE_JOB = ("Take a notification from any sender on this machine — ours or any ordinary Linux "
           "program — into one store, count it where the desktop can see it, keep it across a "
           "restart of the service, and let it be dismissed.")

APP = "notifications"
SERVICE_BIN = "/opt/yantrik/bin/notifications-service"
SERVICE_SOCK = lib.SOCKET_DIR / "notifications.sock"
STORE = pathlib.Path.home() / ".local/share/yantrik/notifications.json"

# Unique per run, so a probe that crashed last time cannot be mistaken for this one's work and
# so two probes on one machine cannot dismiss each other's notifications.
MARK = "conformance-%d-%d" % (os.getpid(), int(time.time()))
FREEDESKTOP_TITLE = "Probe via notify-send %s" % MARK
YANTRIK_TITLE = "Probe via yos notify %s" % MARK


def listed():
    """Everything the store says is showing, newest first."""
    try:
        return lib.call(APP, "notifications.list") or []
    except SystemExit:
        return []


def mine(items=None):
    """Only the notifications this run posted."""
    items = listed() if items is None else items
    return [n for n in items if MARK in (n.get("title") or "")]


def find(title, items=None):
    for n in (listed() if items is None else items):
        if n.get("title") == title:
            return n
    return None


def dismiss_mine():
    """Take back everything this probe put there. Safe to call twice."""
    removed = []
    for n in mine():
        try:
            lib.call(APP, "notifications.dismiss", {"id": n["id"]})
            removed.append(n["id"])
        except SystemExit:
            pass
    return removed


def service_up():
    return lib.surface_up(APP)


def start_service():
    """Ask the desktop to start it, the way anything else on this machine would."""
    asked = lib.act("shell", "start_service", name=APP)
    lib.wait_until(service_up, timeout=10, what="the notifications service to answer")
    return asked


def run():
    """The probe. In a function so that importing this file does nothing — see calendar.py."""
    not_exercised = []

    with lib.Probe(APP, ONE_JOB) as probe:
        probe.note("processes_before", lib.running(SERVICE_BIN))
        probe.note("store_path", str(STORE))
        probe.note("marker", MARK)

        try:
            # ── 1. The one store is answering ─────────────────────────────
            if not service_up():
                start_service()
            up = lib.wait_until(service_up, timeout=15, what="the notifications service")
            probe.check(
                "the notifications service answers on its socket",
                bool(up),
                contract=1,
                evidence=up.evidence(socket=str(SERVICE_SOCK),
                                     processes=lib.running(SERVICE_BIN)))
            if not up:
                # Nothing below can be checked, and saying so once beats nine identical
                # failures that all mean "it is not running".
                probe.note("stopped_early",
                           "The service never answered, so no sender could be tested.")
                return

            state = lib.state(APP)
            probe.note("describe_before", state)
            freedesktop = str(state.get("freedesktop", ""))
            holds_the_bus = freedesktop.startswith("serving")
            probe.check(
                "the service holds org.freedesktop.Notifications, or names who has it instead",
                holds_the_bus or "owned by" in freedesktop or "unavailable" in freedesktop,
                contract=4,
                evidence={"freedesktop": freedesktop})

            # ── 2. An ordinary Linux program ──────────────────────────────
            #
            # notify-send is libnotify's own client and knows nothing about Yantrik. If it lands
            # here, so does Chromium, and so does any script anybody writes.
            if not holds_the_bus:
                not_exercised.append(
                    "a notify-send lands in the store with source=freedesktop")
                probe.note("freedesktop_not_exercised", {
                    "statement": "The freedesktop door was NOT EXERCISED: this service does not "
                                 "hold org.freedesktop.Notifications, so notify-send reached "
                                 "whoever does. On a machine that has not taken the update that "
                                 "is mako, still started from the labwc autostart.",
                    "freedesktop": freedesktop,
                })
            else:
                sent = lib.run_and_wait(
                    ["notify-send", "--app-name=ConformanceProbe", "--urgency=low",
                     FREEDESKTOP_TITLE, "posted by the conformance probe"],
                    timeout=20)
                probe.note("notify_send", sent)
                landed = lib.wait_until(lambda: find(FREEDESKTOP_TITLE), timeout=10,
                                        what="the notify-send notification to reach the store")
                from_fd = landed.value if landed else None
                probe.check(
                    "a notify-send lands in the store with source=freedesktop",
                    bool(from_fd) and from_fd.get("source") == "freedesktop",
                    contract=8,
                    evidence={"stored": from_fd, "app": (from_fd or {}).get("app"),
                              "source": (from_fd or {}).get("source"),
                              "waited": landed.evidence() if landed else None})
                probe.check(
                    "the sender's own name is kept, not replaced by ours",
                    bool(from_fd) and from_fd.get("app") == "ConformanceProbe",
                    contract=3,
                    evidence={"app": (from_fd or {}).get("app")})

            # ── 3. Our own one-liner ──────────────────────────────────────
            sent = lib.run_and_wait(
                [lib.YOS, "notify", YANTRIK_TITLE, "posted by the conformance probe",
                 "--app", "ConformanceProbe"],
                timeout=20)
            probe.note("yos_notify", sent)
            landed = lib.wait_until(lambda: find(YANTRIK_TITLE), timeout=10,
                                    what="the yos notify notification to reach the store")
            from_us = landed.value if landed else None
            probe.check(
                "`yos notify` lands in the same store, with source=yantrik",
                bool(from_us) and from_us.get("source") == "yantrik",
                contract=8,
                evidence={"stored": from_us, "source": (from_us or {}).get("source"),
                          "waited": landed.evidence() if landed else None})
            if not from_us:
                probe.note("stopped_early",
                           "Nothing was stored, so the restart and dismiss checks below would "
                           "be about an empty store rather than about keeping anything.")
                return

            # ── 4. The desktop can see it ─────────────────────────────────
            #
            # The whole reason there is one store: the shell's own account of the machine has to
            # agree with it. The shell polls once a second, so this is given a moment.
            def shell_unread():
                block = lib.describe("shell").get("state", {}).get("notifications") or {}
                return block.get("unread")

            counted = lib.wait_until(lambda: (shell_unread() or 0) > 0, timeout=8,
                                     what="the desktop to count the unread notification")
            shell_block = lib.describe("shell").get("state", {}).get("notifications") or {}
            probe.note("shell_notifications", shell_block)
            probe.check(
                "the desktop's own describe reports the unread count from the same store",
                bool(counted) and isinstance(shell_block.get("unread"), int)
                and shell_block["unread"] > 0,
                contract=8,
                evidence={"shell_notifications": shell_block,
                          "waited": counted.evidence() if counted else None})
            probe.check(
                "the desktop names the most recent notifications, not just a number",
                any(MARK in (n.get("title") or "") for n in (shell_block.get("latest") or [])),
                contract=3,
                evidence={"latest": shell_block.get("latest")})

            # ── 5. Kill it and start it again ─────────────────────────────
            #
            # The check the old service could not have passed: its store was a `Vec` in memory,
            # so everything on this machine died with the process — and the shell autostarts it,
            # which meant every reboot.
            before = {n["id"]: n["title"] for n in mine()}
            probe.note("mine_before_restart", before)
            killed = lib.kill_app(SERVICE_BIN)
            try:
                SERVICE_SOCK.unlink(missing_ok=True)
            except OSError:
                pass
            probe.note("killed", killed)
            time.sleep(1)
            start_service()
            back = lib.wait_until(service_up, timeout=15,
                                  what="the notifications service to come back")
            probe.check(
                "the service comes back when it is asked for",
                bool(back),
                contract=1,
                evidence=back.evidence(processes=lib.running(SERVICE_BIN)))

            after = {n["id"]: n["title"] for n in mine()} if back else {}
            probe.note("mine_after_restart", after)
            probe.check(
                "what was stored is still there after the service is restarted",
                bool(before) and before == after,
                contract=6,
                evidence={"before": before, "after": after,
                          "store_exists": STORE.exists(),
                          "store_bytes": STORE.stat().st_size if STORE.exists() else None})

            # The ids are the same ones, which is what lets a sender close its own notification
            # across a restart — the freedesktop spec promises exactly that.
            probe.check(
                "the ids survive too, so a sender can still close what it sent",
                bool(before) and set(before) == set(after),
                contract=6,
                evidence={"before_ids": sorted(before), "after_ids": sorted(after)})

            # ── 6. Dismiss ────────────────────────────────────────────────
            target = next(iter(after or before), None)
            if target:
                answer = lib.act(APP, "dismiss", id=target)
                probe.note("dismiss", answer)
                # Checked against the store's own list, not against the action's answer: an
                # action that reports what it did not do is the fault this whole suite exists
                # for, and this service's `dismiss` used to answer success over an id it had
                # never seen.
                still_listed = [n["id"] for n in listed() if n["id"] == target]
                probe.check(
                    "dismissing one notification takes it off the list",
                    not still_listed,
                    contract=2,
                    evidence={"dismissed": target, "answer": answer.get("result"),
                              "refused": answer.get("refused"),
                              "still_listed": still_listed})

            # A refusal names its reason rather than reporting success over nothing.
            missing = lib.act(APP, "dismiss", id="this-id-does-not-exist")
            probe.check(
                "dismissing something that is not there is refused, in words that say why",
                bool(missing.get("refused")) and "this-id-does-not-exist" in missing["refused"],
                contract=3,
                evidence={"refused": missing.get("refused"),
                          "kind": lib.refusal_kind(missing)})

        finally:
            # ── Put the machine back ──────────────────────────────────────
            removed = dismiss_mine()
            probe.note("dismissed_on_the_way_out", removed)

        leftover = mine()
        probe.check(
            "nothing this probe posted is still showing",
            not leftover,
            contract="leave-as-found",
            evidence={"leftover": leftover})

        running_after = lib.running(SERVICE_BIN)
        probe.note("processes_after", running_after)
        probe.check(
            "the notifications service is running afterwards",
            bool(running_after),
            contract="leave-as-found",
            evidence={"before": probe.notes["processes_before"], "after": running_after})

        if not_exercised:
            probe.note("checks_not_exercised", not_exercised)


if __name__ == "__main__":
    run()
