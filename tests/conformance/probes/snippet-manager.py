#!/usr/bin/env python3
"""Snippets' one job: keep a piece of code and give it back.

The survey's finding was that it did neither. `on_snip_save` was `let _ = (code, tags)` over a
log line, the model was never updated, and the next selection repainted the editor out of that
stale model — so an edit reverted while you watched. Copy, the one button a snippet manager
exists for, was `tracing::info!`. Nothing was written anywhere, so a restart could only ever show
an empty window.

So the shape of this probe is: keep something, change it, kill the app, and look. Every claim is
checked against the state file on disk or against the clipboard as a separate process reads it,
never against the action's own answer — an app that says `saved` is exactly what was wrong here.

The app is reached as `snippets`, which is the id the launcher's route carries
(`crates/yantrik-ui/src/wire/dock.rs`), so `yos act snippets` and the dock mean the same window.

Two checks can be unexercised on a given machine and say so rather than passing:

* `copy` is only verified when `wl-paste` is installed, because reading the clipboard back is the
  only way to tell a real copy from a claimed one, and the app's own answer is not evidence. On a
  machine with no `wl-copy` at all the app cannot copy anything, and what is checked instead is
  that it says so rather than reporting a copy it never made.
* `delete` is graded `sensitive`, so a machine whose `tool_permission` ceiling is below that
  refuses it before the app ever runs. `lib.refusal_kind` tells that apart from the app declining,
  and a policy refusal is recorded as NOT EXERCISED — it proves nothing about this app.

The probe writes into the person's real snippet store, because the app reads
`YANTRIK_SNIPPETS_DIR` only from its own environment and this launch goes through the shell.
`lib.preserved` snapshots the directory on the way in and puts it back on the way out, and the
last check asserts that it did.
"""

import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import lib  # noqa: E402

ONE_JOB = ("Keep a piece of code and give it back: a snippet you save is on disk with its code, "
           "an edit to it survives the app being killed, and Copy really puts it on the "
           "clipboard.")

APP = "snippets"
APP_BIN = "/opt/yantrik/bin/yantrik-snippet-manager"
STORE = pathlib.Path.home() / ".local/share/yantrik/snippets"
STATE = STORE / "snippets.json"

MARK = "yantrik-conformance-%d" % os.getpid()
FIRST_CODE = "echo '%s first'" % MARK
EDITED_CODE = "echo '%s edited'\necho 'second line'" % MARK
HANDOVER_CODE = "# %s handover\nprint('from the command line')\n" % MARK


def read_state():
    """The state file as the app left it. The witness for points 2 and 6."""
    try:
        return json.loads(STATE.read_text())
    except (OSError, ValueError):
        return {}


def stored(snippet_id):
    """One record out of the state file on disk — not out of `describe`."""
    for record in read_state().get("snippets") or []:
        if record.get("id") == snippet_id:
            return record
    return None


def shown(snippet_id):
    """One row out of `describe`. What the app says, recorded as what the app says."""
    for entry in lib.state(APP).get("snippets") or []:
        if entry.get("id") == snippet_id:
            return entry
    return None


def open_snippets():
    return lib.open_app(APP, expect_process=APP_BIN, window_words=("snippet",), timeout=45)


def session_env():
    env = dict(os.environ)
    env.setdefault("XDG_RUNTIME_DIR", str(lib.RUNTIME_DIR))
    env.setdefault("WAYLAND_DISPLAY", "wayland-0")
    return env


def clipboard_text():
    """What the Wayland clipboard holds, as a separate process sees it, or None."""
    if not shutil.which("wl-paste"):
        return None
    try:
        done = subprocess.run(["wl-paste", "--no-newline"], capture_output=True, text=True,
                              timeout=10, env=session_env())
    except (subprocess.TimeoutExpired, OSError):
        return None
    return done.stdout if done.returncode == 0 else None


def set_clipboard(text):
    """Put the person's own clipboard back. Best effort; failing is reportable, not fatal."""
    if not shutil.which("wl-copy"):
        return False
    try:
        subprocess.run(["wl-copy"], input=text, text=True, timeout=10, env=session_env(),
                       capture_output=True)
        return True
    except (subprocess.TimeoutExpired, OSError):
        return False


with lib.Probe(APP, ONE_JOB) as probe:
    probe.note("processes_before", lib.running(APP_BIN))
    probe.note("windows_before", lib.toplevels())
    probe.note("store_path", str(STATE))

    work_dir = pathlib.Path(tempfile.mkdtemp(prefix="yantrik-snippets-probe-"))
    clipboard_before = clipboard_text()
    probe.note("clipboard_tools", {"wl_paste": bool(shutil.which("wl-paste")),
                                   "wl_copy": bool(shutil.which("wl-copy")),
                                   "clipboard_held_text_before": clipboard_before is not None})

    store = lib.preserved(STORE)
    snippet_id = None
    try:
        with store:
            probe.note("store_before", store.listing())

            # A cold app, so nothing below is inherited from a window that was already open.
            lib.kill_app(APP_BIN)

            # ── 1. It opens ──────────────────────────────────────────────
            opened = open_snippets()
            probe.check(
                "it opens: a process exists, the compositor has its window, and the surface answers",
                bool(opened["processes"]) and bool(opened["windows"]) and opened["surface_up"],
                contract=1, evidence=opened)
            probe.check(
                "a launch that worked adds nothing to the shell's failed_launches",
                opened["new_failed_launches_for_this_app"] == [],
                contract=1, evidence={"added_by_this_launch": opened["new_failed_launches"]})

            # ── 2. It does its one job, and the store agrees ─────────────
            made = lib.act(APP, "new", title="Conformance %s" % MARK, language="Shell",
                           code=FIRST_CODE, tags="conformance")
            snippet_id = (made.get("result") or {}).get("id")
            probe.check(
                "new answers with the id it stored the snippet under, and where it stored it",
                snippet_id is not None and bool((made.get("result") or {}).get("stored_in")),
                contract=3, evidence={"result": made.get("result"),
                                      "accepted": made.get("accepted"),
                                      "settled": made.get("settled"),
                                      "refused": made.get("refused")})
            if snippet_id is None:
                raise SystemExit("new did not report an id; nothing below can be checked")

            record = lib.wait_for(lambda: stored(snippet_id), timeout=15)
            saved_state = read_state()
            probe.check(
                "the snippet is on disk, with the code it was given",
                record is not None and record.get("code") == FIRST_CODE,
                contract=2, evidence={"state_file": str(STATE), "record": record,
                                      "rows_on_disk": len(saved_state.get("snippets") or [])})
            probe.check(
                "the state file carries a schema version",
                isinstance(saved_state.get("version"), int),
                contract=6, evidence={"version": saved_state.get("version"),
                                      "keys": sorted(saved_state.keys())})

            # ── The bug itself: an edit that is kept ─────────────────────
            edited = lib.act(APP, "save", id=str(snippet_id), code=EDITED_CODE,
                             title="Conformance %s edited" % MARK)
            on_disk_after_save = stored(snippet_id) or {}
            probe.check(
                "save writes the edit through to the file, instead of discarding its arguments",
                on_disk_after_save.get("code") == EDITED_CODE,
                contract=2, evidence={"answer": edited.get("result"),
                                      "refused": edited.get("refused"),
                                      "code_on_disk": on_disk_after_save.get("code"),
                                      "code_sent": EDITED_CODE})

            # Opening another snippet and coming back is what used to revert the typing. There is
            # only one snippet here, so the equivalent is opening it again and reading it back.
            lib.act(APP, "open", id=str(snippet_id))
            probe.check(
                "opening the snippet again shows the edit, not the version before it",
                (shown(snippet_id) or {}).get("title", "").endswith("edited")
                and (stored(snippet_id) or {}).get("code") == EDITED_CODE,
                contract=2, evidence={"row": shown(snippet_id),
                                      "open": lib.state(APP).get("open")})

            # ── 6. It survives a restart ─────────────────────────────────
            killed = lib.kill_app(APP_BIN)
            survived_the_kill = (stored(snippet_id) or {}).get("code")
            probe.check(
                "the edit is on disk before the app is reopened, not flushed by the reopen",
                survived_the_kill == EDITED_CODE,
                contract=6, evidence={"killed": killed, "code_on_disk": survived_the_kill})

            reopened = open_snippets()
            back = lib.wait_until(lambda: shown(snippet_id),
                                  timeout=30,
                                  what="the reopened window to list the snippet again")
            row = shown(snippet_id) or {}
            probe.check(
                "the snippet and its edit are still there after the app is killed and reopened",
                bool(reopened["processes"]) and bool(back)
                and (stored(snippet_id) or {}).get("code") == EDITED_CODE,
                contract=6, evidence=back.evidence(
                    row=row, total=lib.state(APP).get("total"),
                    code_on_disk=(stored(snippet_id) or {}).get("code"),
                    store=lib.state(APP).get("store")))

            # ── Copy, checked from outside the app ───────────────────────
            copied = lib.act(APP, "copy", id=str(snippet_id))
            refusal = str(copied.get("refused") or "")
            if "wl-copy" in refusal:
                # No clipboard tool on this machine. The app cannot do the thing, and what is
                # worth checking is that it said so instead of reporting a copy it never made —
                # which is exactly what the log line it replaced used to do.
                probe.check(
                    "a copy it cannot make is refused in words, not reported as done",
                    copied.get("accepted") is False and refusal not in ("1", "0"),
                    contract=3, evidence={"refusal": refusal})
                probe.check(
                    "copy: NOT EXERCISED — no wl-copy on this machine, so nothing could be copied",
                    True, severity=lib.ADVISORY, contract=2,
                    evidence={"refusal": refusal})
            elif shutil.which("wl-paste"):
                landed = lib.wait_until(
                    lambda: clipboard_text() == EDITED_CODE,
                    timeout=15, interval=0.4,
                    what="the snippet's code to appear on the clipboard")
                probe.check(
                    "copy really puts the code on the clipboard, as another process reads it",
                    bool(landed),
                    contract=2, evidence=landed.evidence(
                        answer=copied.get("result"), refused=copied.get("refused"),
                        clipboard_now=(clipboard_text() or "")[:200], expected=EDITED_CODE))
                probe.check(
                    "copy says what it copied and how, and counts the use",
                    (copied.get("result") or {}).get("bytes") == len(EDITED_CODE)
                    and (copied.get("result") or {}).get("with") == "wl-copy",
                    contract=3, evidence={"answer": copied.get("result")})
                probe.check(
                    "the use count reached the store, not only the window",
                    (stored(snippet_id) or {}).get("use_count", 0) >= 1,
                    contract=2, evidence={"record": stored(snippet_id)})
            else:
                probe.check(
                    "copy: NOT EXERCISED — no wl-paste on this machine to read the clipboard back",
                    True, severity=lib.ADVISORY, contract=2,
                    evidence={"answer": copied.get("result"), "refused": copied.get("refused"),
                              "why": "the app's own answer is not evidence that anything was "
                                     "copied, and nothing here can read the selection"})

            # ── 5. It takes work from outside ────────────────────────────
            #
            # A second launch with a file must hand the file to the window that is already open,
            # not start a second one.
            source = work_dir / "handover.py"
            source.write_text(HANDOVER_CODE)
            before_handover = len(lib.running(APP_BIN))
            handover = lib.run_and_wait([APP_BIN, str(source)], timeout=20)
            imported = lib.wait_until(
                lambda: next((r for r in (read_state().get("snippets") or [])
                              if r.get("code") == HANDOVER_CODE), None),
                timeout=25, what="the handed-over file to be kept as a snippet")
            probe.check(
                "a file named on the command line is kept by the window that is already open",
                bool(imported) and len(lib.running(APP_BIN)) == before_handover,
                contract=5, evidence=imported.evidence(
                    exit=handover.get("exit"), stderr=handover.get("stderr"),
                    processes=lib.running(APP_BIN),
                    title=(imported.value or {}).get("title") if imported else None,
                    language=(imported.value or {}).get("language") if imported else None))
            handover_id = (imported.value or {}).get("id") if imported else None
            probe.check(
                "the language of a handed-over file is read off its name rather than left blank",
                (imported.value or {}).get("language") == "Python" if imported else False,
                contract=3, evidence={"record": imported.value if imported else None})

            # ── 4. Failure is said twice ─────────────────────────────────
            #
            # Forced by taking write permission off the store directory, which is the failure this
            # app actually has: the file cannot be written. The app must refuse in words, say the
            # same thing in `describe.notice`, and leave the file exactly as it was.
            bytes_before_failure = STATE.read_bytes()
            mode_before = STORE.stat().st_mode
            blocked = False
            try:
                STORE.chmod(0o500)
                probe_file = STORE / ("probe-write-test-%d" % os.getpid())
                try:
                    probe_file.write_text("x")
                    probe_file.unlink()
                except OSError:
                    blocked = True
                if blocked:
                    refused = lib.act(APP, "new", title="Should not be kept %s" % MARK,
                                      language="Shell", code="echo no")
                    time.sleep(0.5)
                    notice = lib.state(APP).get("notice") or ""
                    probe.check(
                        "a snippet it cannot write is refused in words, not accepted and dropped",
                        refused.get("accepted") is False and bool(refused.get("refused"))
                        and refused.get("refused") not in ("1", "0"),
                        contract=4, evidence={"refusal": refused.get("refused"),
                                              "accepted": refused.get("accepted")})
                    probe.check(
                        "the same failure is in describe.notice, where a mind reads it",
                        bool(notice),
                        contract=4, evidence={"notice": notice,
                                              "summary": lib.describe(APP).get("summary")})
                    probe.check(
                        "nothing was written while it refused",
                        STATE.read_bytes() == bytes_before_failure,
                        contract=4, evidence={"bytes_before": len(bytes_before_failure),
                                              "bytes_after": len(STATE.read_bytes())})
                else:
                    probe.check(
                        "a failed write: NOT EXERCISED — this user can still write the store "
                        "directory after chmod 0500 (running as root?)",
                        True, severity=lib.ADVISORY, contract=4,
                        evidence={"store": str(STORE), "mode": oct(mode_before)})
            finally:
                STORE.chmod(mode_before & 0o7777)

            # ── 9. Grades mean it ────────────────────────────────────────
            #
            # `delete` is graded sensitive because it destroys something the person wrote and
            # there is no trash to take it back out of. Below the ceiling it never reaches the
            # app at all, and that is not a fact about this app.
            deletable = handover_id if handover_id is not None else snippet_id
            deleted = lib.act(APP, "delete", id=str(deletable))
            kind = lib.refusal_kind(deleted)
            if kind == "policy":
                probe.check(
                    "delete: NOT EXERCISED — this machine's ceiling refuses a sensitive action "
                    "before the app runs",
                    True, severity=lib.ADVISORY, contract=9,
                    evidence={"refusal": deleted.get("refused"), "kind": kind,
                              "why": "the app was never asked, so nothing about its deleting "
                                     "has been tested"})
            else:
                probe.check(
                    "delete removes the snippet from the file, and says so from the store",
                    deleted.get("accepted") is True
                    and stored(deletable) is None
                    and (deleted.get("result") or {}).get("still_present") is False,
                    contract=3, evidence={"answer": deleted.get("result"),
                                          "refused": deleted.get("refused"),
                                          "record_after": stored(deletable),
                                          "rows": len(read_state().get("snippets") or [])})
                probe.check(
                    "deleting one snippet leaves the other alone",
                    stored(snippet_id) is not None if deletable != snippet_id else True,
                    contract=2, evidence={"kept": stored(snippet_id)})

            # ── 3. A refusal names what it could not find ────────────────
            missing = lib.act(APP, "copy", id="no-such-snippet-%s" % MARK)
            probe.check(
                "a snippet that does not exist is named in the refusal, not answered as done",
                missing.get("accepted") is False
                and MARK in str(missing.get("refused") or ""),
                contract=3, evidence={"refusal": missing.get("refused")})

            # ── The write itself ─────────────────────────────────────────
            lib.kill_app(APP_BIN)
            leftovers = sorted(p.name for p in STORE.glob("snippets.json.tmp-*"))
            corrupt = sorted(p.name for p in STORE.glob("snippets.json.corrupt-*"))
            probe.check(
                "no half-written state file is left beside the real one",
                leftovers == [],
                contract=6, evidence={"temp_files": leftovers,
                                      "store_listing": sorted(p.name for p in STORE.iterdir())})
            probe.check(
                "nothing was moved aside as unreadable during this run",
                corrupt == [],
                contract=6, evidence={"kept_aside": corrupt})

    finally:
        # ── Put the machine back ─────────────────────────────────────────
        #
        # `preserved` has already restored the store. What is left is this probe's scratch
        # directory and the person's clipboard, which `copy` legitimately overwrote.
        lib.kill_app(APP_BIN)
        shutil.rmtree(work_dir, ignore_errors=True)
        if clipboard_before is not None:
            probe.note("clipboard_restored", set_clipboard(clipboard_before))
        else:
            probe.note("clipboard_restored",
                       "not restored: the clipboard held nothing readable as text before this run")

    probe.note("store_after", store.listing(store.after))
    probe.check(
        "the snippet store is left as it was found",
        not store.differences(),
        contract="leave-as-found",
        evidence={"before": store.listing(), "after": store.listing(store.after),
                  "differences": store.differences() or "none"})
    probe.check(
        "the probe's scratch directory is gone",
        not work_dir.exists(),
        contract="leave-as-found", evidence={"work_dir": str(work_dir)})

    leftover = lib.running(APP_BIN)
    probe.note("processes_after", leftover)
    probe.note("windows_after", lib.toplevels())
    probe.check(
        "no snippets process is left running that was not running before",
        leftover == probe.notes["processes_before"],
        contract="leave-as-found",
        evidence={"before": probe.notes["processes_before"], "after": leftover})
