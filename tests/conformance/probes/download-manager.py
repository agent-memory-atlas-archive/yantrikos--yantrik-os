#!/usr/bin/env python3
"""Download Manager's one job: fetch a file, and still have it after a restart.

The audit called this app the gold standard and it had one real gap: no persistence. The list
lived in the process, so closing the window lost every record and left every partial transfer on
disk as a file nobody could explain or continue. A mind that started a large download and came
back after a session restart found nothing. That is contract point 6, and it is what this probe
is about.

Every claim is checked against ground truth — the state file, the size of the partial file on
disk, and a SHA-256 this probe computes itself — never against the action's own answer. The
restored row claiming to be restored proves nothing on its own; the bytes on disk do.

No internet. The probe serves the file itself from a throttled loopback HTTP server that honours
`Range` the way a mirror does, so `resume` is tested as a resume and not as a silent restart:
against a server that answered 200 the engine truncates and starts over, and the file would
shrink. The low-water check below is what tells the two apart.

Every action this app publishes that changes anything — `add`, `pause`, `resume`, `verify` —
declares `defers`: the reply is the app accepting the work, not the app having finished it, and
the same is true of the hashing thread that runs after the last byte lands. So nothing here reads
an outcome the moment it asks for one. Each deferred outcome is waited for with `lib.wait_until`
against a bounded deadline, and a deadline that expires is a failed check of its own carrying the
wait as its evidence — because a probe that reads `checksum: verifying` and asserts on it is
testing its own timing, not the app.
"""

import hashlib
import http.server
import json
import os
import pathlib
import shutil
import socketserver
import sys
import tempfile
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import lib  # noqa: E402

ONE_JOB = ("Fetch a file and keep it: a download you start is on disk with the hash it claims, "
           "and both the record and the partial bytes survive the app being killed.")

APP = "download-manager"
APP_BIN = "/opt/yantrik/bin/yantrik-download-manager"
STORE = pathlib.Path.home() / ".local/share/yantrik/downloads"
STATE = STORE / "state.json"

# Big enough and slow enough that there is a middle to catch it in: 6 MiB at ~1.6 MiB/s is about
# four seconds of transfer, which is room to pause even on a loaded VM, and well inside the
# runner's per-probe timeout.
SIZE = 6 * 1024 * 1024
CHUNK = 64 * 1024
CHUNK_DELAY = 0.04

# How long a deferred outcome is given before the wait is called a failure.
#
# `completed` and `checksum: pass` are two different moments: the engine flips the status and
# then hands the file to a hashing thread, so there is a window in which the row says the
# transfer is done and the checksum says `verifying`. Hashing 6 MiB is milliseconds of work on
# an idle machine — these deadlines are generous because this one renders in software and can be
# busy, and a deadline that is merely typical turns a wait into a coin toss.
CHECKSUM_DEADLINE = 60
PAUSE_DEADLINE = 30
TRANSFER_DEADLINE = 150


def read_state():
    """The state file as the app left it. The witness for point 6."""
    try:
        return json.loads(STATE.read_text())
    except (OSError, ValueError):
        return {}


def stored_row(download_id):
    """One record out of the state file on disk — not out of `describe`."""
    for record in read_state().get("downloads") or []:
        if record.get("id") == download_id:
            return record
    return None


def row(download_id):
    """One row out of `describe`. What the app says, recorded as what the app says."""
    for entry in lib.state(APP).get("downloads") or []:
        if entry.get("id") == download_id:
            return entry
    return None


def open_downloads():
    return lib.open_app(APP, expect_process=APP_BIN, window_words=("download",), timeout=45)


# ── The mirror this probe serves from ────────────────────────────────────────

class Ranged(http.server.BaseHTTPRequestHandler):
    """A mirror, slowed down, that answers `Range` properly.

    Python's own `SimpleHTTPRequestHandler` ignores `Range` and answers 200 with the whole body.
    The engine handles that correctly — it truncates and starts over rather than appending a
    second copy — but a probe served that way would be watching a restart and calling it a
    resume. Honouring the range here is what makes the low-water check below mean something.
    """

    body = b""

    def log_message(self, *_args):
        pass

    def do_GET(self):  # noqa: N802 - the name is http.server's
        start = 0
        requested = self.headers.get("Range", "")
        partial = requested.startswith("bytes=")
        if partial:
            try:
                start = int(requested.split("=", 1)[1].split("-", 1)[0] or 0)
            except ValueError:
                start = 0
            start = max(0, min(start, len(self.body)))
        payload = self.body[start:]

        self.send_response(206 if partial else 200)
        self.send_header("Content-Type", "application/octet-stream")
        self.send_header("Content-Length", str(len(payload)))
        self.send_header("Accept-Ranges", "bytes")
        if partial:
            self.send_header("Content-Range",
                             "bytes %d-%d/%d" % (start, len(self.body) - 1, len(self.body)))
        self.end_headers()
        for offset in range(0, len(payload), CHUNK):
            try:
                self.wfile.write(payload[offset:offset + CHUNK])
                self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError, OSError):
                # The app paused, or was killed mid-transfer. That is the test, not a fault.
                return
            time.sleep(CHUNK_DELAY)


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


with lib.Probe(APP, ONE_JOB) as probe:
    probe.note("processes_before", lib.running(APP_BIN))
    probe.note("windows_before", lib.toplevels())

    work_dir = pathlib.Path(tempfile.mkdtemp(prefix="yantrik-dl-probe-"))
    save_dir = work_dir / "saved"
    save_dir.mkdir()
    server = None

    store = lib.preserved(STORE)
    try:
        with store:
            probe.note("store_before", store.listing())

            # Deterministic bytes, so a hash mismatch means a broken transfer and not a random
            # file this probe could not check.
            body = bytes((i * 31 + 7) % 251 for i in range(4096)) * (SIZE // 4096)
            expected_sha = hashlib.sha256(body).hexdigest()
            Ranged.body = body

            server = Server(("127.0.0.1", 0), Ranged)
            threading.Thread(target=server.serve_forever, daemon=True).start()
            url = "http://127.0.0.1:%d/probe-payload.bin" % server.server_address[1]
            probe.note("serving", {"url": url, "bytes": len(body), "sha256": expected_sha,
                                   "save_dir": str(save_dir)})

            # A cold app, so nothing below is inherited from a window that was already open.
            lib.kill_app(APP_BIN)

            # ── 1. It opens ──────────────────────────────────────────────────
            opened = open_downloads()
            probe.check(
                "it opens: a process exists and the compositor has its window",
                bool(opened["processes"]) and bool(opened["windows"]) and opened["surface_up"],
                contract=1, evidence=opened)
            probe.check(
                "a launch that worked adds nothing to the shell's failed_launches",
                opened["new_failed_launches_for_this_app"] == [],
                contract=1, evidence={"added_by_this_launch": opened["new_failed_launches"]})

            # ── 2. It does its one job, and the store agrees ─────────────────
            added = lib.act(APP, "add", url=url, save_dir=str(save_dir), sha256=expected_sha)
            download_id = (added.get("result") or {}).get("id")
            reported_path = (added.get("result") or {}).get("path")
            probe.check(
                "add answers with an id and the path it will write",
                download_id is not None and bool(reported_path),
                contract=3, evidence={"result": added.get("result"), "accepted": added.get("accepted"),
                                      "settled": added.get("settled"), "refused": added.get("refused")})
            if download_id is None:
                raise SystemExit("add did not report an id; nothing below can be checked")
            saved_path = pathlib.Path(reported_path)

            # The record has to reach the disk while the transfer is still running: a crash one
            # second into a 4 GB fetch must still leave something naming the URL, or the partial
            # file beside it means nothing.
            on_disk_record = lib.wait_for(lambda: stored_row(download_id), timeout=20)
            saved_state = read_state()
            probe.check(
                "the download is written to the state file while it is still running",
                on_disk_record is not None and on_disk_record.get("url") == url,
                contract=6, evidence={"state_file": str(STATE), "record": on_disk_record,
                                      "rows_on_disk": len(saved_state.get("downloads") or [])})
            probe.check(
                "the state file carries a schema version",
                isinstance(saved_state.get("version"), int),
                contract=6, evidence={"version": saved_state.get("version"),
                                      "keys": sorted(saved_state.keys())})

            # ── Stop it half way ─────────────────────────────────────────────
            caught = lib.wait_for(
                lambda: (lambda r: r if r and 0 < int(r.get("percent") or 0) < 70 else None)(
                    row(download_id)),
                timeout=40, interval=0.3)
            probe.check(
                "the transfer reports partial progress while it is running",
                caught is not None,
                contract=2, evidence={"row": caught, "summary": lib.state(APP).get("summary")})
            # `pause` defers: it sets a flag the worker reads at its next chunk boundary, and the
            # row it answers with can still say `downloading`. The worker flushes the file before
            # it flips the status, so `paused` is also the point at which the byte count stops
            # moving — which is why the size below is read after the wait and not after the call.
            paused_action = lib.act(APP, "pause", id=download_id)
            came_to_rest = lib.wait_until(
                lambda: (row(download_id) or {}).get("status") == "paused",
                timeout=PAUSE_DEADLINE,
                what="the running transfer to come to rest at `paused`")
            partial_bytes = saved_path.stat().st_size if saved_path.exists() else 0
            probe.check(
                "pause stops the transfer, and keeps the bytes that arrived",
                bool(came_to_rest) and 0 < partial_bytes < len(body),
                contract=2, evidence=came_to_rest.evidence(
                    path=str(saved_path), bytes_on_disk=partial_bytes, of_total=len(body),
                    status=(row(download_id) or {}).get("status"),
                    pause_answer=paused_action.get("result"),
                    pause_refused=paused_action.get("refused")))

            # ── 6. It survives a restart ─────────────────────────────────────
            killed = lib.kill_app(APP_BIN)
            after_death = saved_path.stat().st_size if saved_path.exists() else 0
            probe.check(
                "the partial file outlives the process",
                after_death >= partial_bytes > 0,
                contract=6, evidence={"killed": killed, "bytes_before_kill": partial_bytes,
                                      "bytes_after_kill": after_death})

            reopened = open_downloads()
            restored = row(download_id)
            view = lib.state(APP)
            on_disk_percent = round(after_death / len(body) * 100)
            probe.check(
                "the download is still listed after the app is killed and reopened",
                bool(reopened["processes"]) and restored is not None,
                contract=6, evidence={"reopened": reopened["processes"], "row": restored,
                                      "total_rows": view.get("total"),
                                      "state_file": view.get("state_file")})
            probe.check(
                "a restored row says it was restored, and does not claim to be running",
                (restored or {}).get("restored") is True
                and (restored or {}).get("status") in ("paused", "queued"),
                contract=3, evidence={"row": restored, "notice": view.get("notice"),
                                      "restored_count": view.get("restored")})
            # The progress it reports has to be the progress on disk. This is the whole point of
            # reconciling against the filesystem rather than believing the record: the record was
            # written seconds before the process died and is behind the file, and resuming from
            # the record's number would append the same bytes twice.
            probe.check(
                "the restored progress is measured from the partial file, not from the record",
                restored is not None
                and abs(int(restored.get("percent") or 0) - on_disk_percent) <= 1,
                contract=2, evidence={"percent_reported": (restored or {}).get("percent"),
                                      "percent_on_disk": on_disk_percent,
                                      "bytes_on_disk": after_death, "total": len(body),
                                      "recorded_bytes": (stored_row(download_id) or {}).get("downloaded")})

            # ── Finish it from where it stopped ──────────────────────────────
            resumed = lib.act(APP, "resume", id=download_id)
            probe.note("resume_answer", {"accepted": resumed.get("accepted"),
                                         "settled": resumed.get("settled"),
                                         "refused": resumed.get("refused")})

            low_water = [len(body)]

            def transfer_over():
                """The row once the transfer has stopped moving, sampling the file on the way.

                The low-water mark can only be read while the transfer is running — a resume
                that truncated and started over shows up as the file shrinking and nowhere
                else — so the sampling lives inside the poll rather than in a loop of its own.
                """
                if saved_path.exists():
                    low_water[0] = min(low_water[0], saved_path.stat().st_size)
                current = row(download_id) or {}
                return current if current.get("status") in ("completed", "failed", "missing") \
                    else None

            transfer = lib.wait_until(
                transfer_over, timeout=TRANSFER_DEADLINE, interval=0.4,
                what="the resumed download to reach a final status")
            lowest = low_water[0]
            finished = row(download_id) or {}
            final_bytes = saved_path.stat().st_size if saved_path.exists() else 0
            on_disk_sha = lib.sha256(saved_path)
            probe.check(
                "the restored download finishes, and the file on disk is the file that was served",
                bool(transfer) and final_bytes == len(body) and on_disk_sha == expected_sha,
                contract=2, evidence=transfer.evidence(
                    status=finished.get("status"), bytes=final_bytes,
                    expected_bytes=len(body), sha256_on_disk=on_disk_sha,
                    sha256_served=expected_sha, error=finished.get("error")))
            probe.check(
                "resume continues the partial file instead of starting it over",
                lowest >= after_death,
                contract=2, evidence={"low_water_bytes": lowest, "bytes_at_resume": after_death,
                                      "note": "a file that shrinks was restarted, not resumed"})

            # ── The checksum, once the hashing thread has finished ───────────
            #
            # `completed` is set in one edit with `checksum: verifying`, and the verdict lands a
            # thread later. Reading the field the instant the status turns is reading the hashing
            # and not its result — which is exactly what this check used to do, and it saw
            # `verifying` with no `sha256` beside it on a loaded VM.
            #
            # Both the surface and the state file have to settle. `describe` is what a caller
            # reads; the state file is what survives the kill on the next line but one, and the
            # digest gets there one `save()` after the field that reports it.
            def checksum_verdict():
                shown = (row(download_id) or {}).get("checksum")
                stored = (stored_row(download_id) or {}).get("checksum_status")
                if "verifying" in (shown, stored):
                    return None
                return {"describe": shown, "state_file": stored}

            verdict = lib.wait_until(
                checksum_verdict, timeout=CHECKSUM_DEADLINE, interval=0.3,
                what="the checksum to settle out of `verifying`, in describe and in the state file")
            finished = row(download_id) or {}
            probe.check(
                "the checksum settles inside the deadline instead of being read mid-hash",
                bool(verdict), contract=3,
                evidence=verdict.evidence(
                    row=finished,
                    stored=(stored_row(download_id) or {}).get("checksum_status"),
                    note="a timeout here is this app hashing forever, or this probe reading a "
                         "deferred outcome too early; either way it is not a pass"))
            probe.check(
                "the app confirms the checksum it was given, against the file it wrote",
                finished.get("checksum") == "pass" and finished.get("sha256") == expected_sha,
                contract=3, evidence={"checksum": finished.get("checksum"),
                                      "sha256_reported": finished.get("sha256"),
                                      "sha256_on_disk": on_disk_sha,
                                      "sha256_served": expected_sha,
                                      "waited_s": round(verdict.seconds, 1),
                                      "checksum_settled": verdict.settled})

            # ── 6 again: and still there the next time ───────────────────────
            #
            # The digest is read back off disk first: the wait above is what makes this check a
            # statement about persistence rather than about whether the hashing thread happened
            # to have saved before the kill landed on it.
            digest_before_kill = (stored_row(download_id) or {}).get("file_hash")
            lib.kill_app(APP_BIN)
            second = open_downloads()
            kept = row(download_id) or {}
            probe.check(
                "a finished download and its digest survive a second restart",
                bool(second["processes"]) and kept.get("status") == "completed"
                and kept.get("sha256") == expected_sha,
                contract=6, evidence={"row": kept, "summary": lib.state(APP).get("summary"),
                                      "digest_in_the_state_file_before_the_kill":
                                          digest_before_kill})

            # ── A completed file that is gone is said to be gone ─────────────
            # Point 2 read the other way: the value of a completed row is the path it points at,
            # so when the file is not there the row must not still read completed.
            lib.kill_app(APP_BIN)
            saved_path.unlink()
            third = open_downloads()
            vanished = row(download_id) or {}
            probe.check(
                "a finished file that is no longer on disk is reported missing, not completed",
                vanished.get("status") == "missing",
                contract=3, evidence={"row": vanished, "path": str(saved_path),
                                      "exists": saved_path.exists(),
                                      "missing_count": lib.state(APP).get("missing"),
                                      "opened": third["processes"]})

            # ── 3/4. A refusal names its reason ──────────────────────────────
            refused_url = lib.act(APP, "add", url="/etc/passwd")
            refused_id = lib.act(APP, "resume", id=987654)
            probe.check(
                "a URL it will not fetch is refused in words, not accepted and dropped",
                refused_url.get("accepted") is False and bool(refused_url.get("refused"))
                and refused_url.get("refused") not in ("1", "0"),
                contract=4, evidence={"refusal": refused_url.get("refused"),
                                      "accepted": refused_url.get("accepted")})
            probe.check(
                "a command against a download that does not exist says which one",
                refused_id.get("accepted") is False
                and "987654" in str(refused_id.get("refused") or ""),
                contract=3, evidence={"refusal": refused_id.get("refused")})

            # ── The write itself ─────────────────────────────────────────────
            lib.kill_app(APP_BIN)
            leftovers = sorted(p.name for p in STORE.glob("state.json.tmp-*"))
            probe.check(
                "no half-written state file is left beside the real one",
                leftovers == [],
                contract=6, evidence={"temp_files": leftovers,
                                      "store_listing": sorted(p.name for p in STORE.iterdir())})

    finally:
        # ── Put the machine back ─────────────────────────────────────────────
        #
        # `preserved` has already restored the store; what is left is the probe's own server and
        # scratch directory. The downloaded file never went near ~/Downloads: every `add` above
        # named a save_dir inside this temp tree.
        lib.kill_app(APP_BIN)
        if server is not None:
            try:
                server.shutdown()
                server.server_close()
            except Exception as exc:  # noqa: BLE001 - cleanup failing is reportable, not fatal
                probe.note("server_shutdown_error", "%s: %s" % (type(exc).__name__, exc))
        shutil.rmtree(work_dir, ignore_errors=True)

    probe.note("store_after", store.listing(store.after))
    probe.check(
        "the downloads store is left as it was found",
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
        "no download-manager process is left running that was not running before",
        leftover == probe.notes["processes_before"],
        contract="leave-as-found",
        evidence={"before": probe.notes["processes_before"], "after": leftover})
