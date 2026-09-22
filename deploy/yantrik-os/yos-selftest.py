#!/usr/bin/env python3
"""Self-test for `yos`: the real script, against fake sockets.

    python3 deploy/yantrik-os/yos-selftest.py

`yos` is a JSON-RPC client over unix sockets and nothing else, so the desktop can be faked by
being one: this puts a socket called `app-shell.sock` in a scratch runtime directory, answers
`app.act` from it, and imports the real `yos` with `XDG_RUNTIME_DIR` pointing at that directory.
Nothing here touches a real machine, a window or the shell.

Linux only (AF_UNIX); WSL is fine. It takes a couple of seconds.

What it is checking, in one line each:

  * `yos perception` on a machine where the service has never been started asks the shell to
    start it, with exactly the action and arguments the shell's control surface publishes, and
    then reads the feed — the defect this file was written for, where "on demand" was a caption
    on a process nothing anywhere ever ran and every os_perception answered "no socket";
  * a service that is already answering is not started a second time, so a privileged
    perception-service is found rather than shadowed;
  * a source that could not start is reported whatever the count asked for, because those
    notices are the oldest observations in the run and a limit would slice them off the end —
    leaving a reader to take a quiet feed for a quiet machine;
  * with no desktop to ask, the answer is a plain sentence and exit 0 — not a traceback and not
    the "failed (exit 1)" that a mind used to be handed for a perfectly good question;
  * and `ensure_service`, which the notify path uses, still ends the command when the service
    will not come up.
"""

import contextlib
import importlib.util
import io
import json
import os
import pathlib
import shutil
import socket
import sys
import tempfile
import threading
from importlib.machinery import SourceFileLoader

HERE = pathlib.Path(__file__).resolve().parent
SOURCE = HERE / "yos"

FAILURES = []


def check(label, condition, detail=""):
    if condition:
        print("  ok    %s" % label)
    else:
        print("  FAIL  %s%s" % (label, ("\n        " + detail) if detail else ""))
        FAILURES.append(label)


class FakeService(threading.Thread):
    """One socket that answers one line at a time, and records what it was asked.

    A connection that sends nothing is the liveness probe `yos` uses to decide whether anything
    is behind a socket at all — so it is accepted and dropped rather than treated as an error.
    """

    daemon = True

    def __init__(self, path, reply):
        super().__init__()
        self.path = str(path)
        self.reply = reply
        self.calls = []
        self.listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.listener.bind(self.path)
        self.listener.listen(8)
        self.stopping = False

    def run(self):
        while not self.stopping:
            try:
                conn, _ = self.listener.accept()
            except OSError:
                return
            with conn:
                conn.settimeout(2)
                buf = b""
                try:
                    while not buf.endswith(b"\n"):
                        chunk = conn.recv(1 << 16)
                        if not chunk:
                            break
                        buf += chunk
                except OSError:
                    continue
                if not buf.strip():
                    continue  # the liveness probe
                asked = json.loads(buf)
                self.calls.append(asked)
                answer = self.reply(self, asked)
                if answer is None:
                    continue
                conn.sendall((json.dumps({"jsonrpc": "2.0", "id": asked.get("id"),
                                          "result": answer}) + "\n").encode())

    def close(self):
        self.stopping = True
        self.listener.close()
        with contextlib.suppress(OSError):
            os.unlink(self.path)


PAGE = {
    # What a session-started perception-service actually answers with: it has no CAP_NET_ADMIN
    # and no CAP_SYS_ADMIN, so it comes up on PSI alone and says which sources it had to do
    # without, at the salience it gives a source going blind.
    "observations": [
        {"kind": {"type": "source_failed", "source": "processes",
                  "reason": "bind: needs CAP_NET_ADMIN"},
         "salience": 1.0, "summary": "perception cannot use its processes source: "
                                     "bind: needs CAP_NET_ADMIN"},
        {"kind": {"type": "source_failed", "source": "files",
                  "reason": "fanotify_init: needs CAP_SYS_ADMIN"},
         "salience": 1.0, "summary": "perception cannot use its files source: "
                                     "fanotify_init: needs CAP_SYS_ADMIN"},
        {"kind": {"type": "pressure", "resource": "io", "stalled_pct_10s": 71.6},
         "salience": 0.8, "summary": "io stalled 72% of the last 10s"},
    ],
    "next_seq": 3,
    "missed": 0,
}


def load_yos(runtime_dir):
    """The real `yos`, with its socket directory pointed at the scratch one.

    `SOCKET_DIRS` is built at import time from `XDG_RUNTIME_DIR`, and it also lists
    `/run/yantrik` — which on a developer's machine may hold a real perception socket. Replacing
    the list outright is what makes this test say the same thing everywhere.
    """
    os.environ["XDG_RUNTIME_DIR"] = str(runtime_dir)
    loader = SourceFileLoader("yos_under_test", str(SOURCE))
    spec = importlib.util.spec_from_loader("yos_under_test", loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    module.SOCKET_DIRS = [str(pathlib.Path(runtime_dir) / "yantrik")]
    return module


def run(fn):
    """Call `fn`, returning (stdout, stderr, exit code or None)."""
    out, err = io.StringIO(), io.StringIO()
    code = None
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        try:
            fn()
        except SystemExit as e:
            code = e.code if e.code is not None else 0
    return out.getvalue(), err.getvalue(), code


def main():
    if not hasattr(socket, "AF_UNIX"):
        print("skipped: this test needs unix sockets")
        return 0

    tmp = tempfile.mkdtemp(prefix="yos-selftest-")
    sockets = pathlib.Path(tmp) / "yantrik"
    sockets.mkdir(parents=True)
    yos = load_yos(tmp)
    started = []
    services = []

    def perception_reply(_self, asked):
        return PAGE if asked["method"] == "perception.since" else {}

    def shell_reply(_self, asked):
        """The shell's control surface, as far as this test needs it.

        `start_service` is the standard-grade action the shell publishes and the ServiceManager
        backs; the fake honours it by putting the service's socket where a started one would be.
        """
        params = asked.get("params") or {}
        if asked["method"] == "app.act" and params.get("action") == "start_service":
            name = (params.get("args") or {}).get("name")
            started.append(name)
            svc = FakeService(sockets / ("%s.sock" % name), perception_reply)
            svc.start()
            services.append(svc)
            return {"accepted": True, "settled": True,
                    "result": {"service": name, "state": "started"}}
        return {"accepted": True, "settled": True}

    shell = FakeService(sockets / "app-shell.sock", shell_reply)
    shell.start()
    services.append(shell)

    try:
        print("yos perception, on a machine where the service has never been started")
        out, err, code = run(lambda: yos.cmd_perception([]))
        check("it asked the desktop to start perception", started == ["perception"],
              "start_service was asked for %r" % (started,))
        acts = [c for c in shell.calls if c["method"] == "app.act"]
        check("through the action the shell publishes, with the arguments it names",
              acts and acts[0]["params"] == {"action": "start_service",
                                             "args": {"name": "perception"}},
              "sent %s" % json.dumps(acts[0]["params"] if acts else None))
        check("and then read the feed", "3 held, next_seq 3, missed 0" in out, out)
        check("naming the sources it cannot use", "needs CAP_NET_ADMIN" in out, out)
        check("with nothing on stderr and no failure", err == "" and code is None,
              "stderr=%r exit=%r" % (err, code))

        print("yos perception, with the service already answering")
        started.clear()
        shell.calls.clear()
        # Asserted rather than assumed: with nothing behind the socket the next check would
        # pass for the wrong reason, which is how it read on the version this test was written
        # against. `_answers` and `socket_candidates` rather than the module's own `is_up`, so
        # this line says the same thing about a `yos` that predates it.
        check("the service is up before this case",
              any(yos._answers(p) for p in yos.socket_candidates("perception")))
        out, err, code = run(lambda: yos.cmd_perception([]))
        check("nothing was started a second time", started == [],
              "start_service was asked for %r" % (started,))
        check("and the feed was still read", "3 held" in out, out)

        print("yos perception 1, where the limit would cut off the blindness")
        out, err, code = run(lambda: yos.cmd_perception(["1"]))
        check("the newest observation is shown", "io stalled" in out, out)
        check("and so is every source that could not start, whatever the limit",
              "needs CAP_NET_ADMIN" in out and "needs CAP_SYS_ADMIN" in out, out)

        print("yos perception, with no desktop to ask")
        for svc in services:
            svc.close()
        services.clear()
        out, err, code = run(lambda: yos.cmd_perception([]))
        check("it is a sentence, not an exit code", code is None, "exit=%r" % (code,))
        check("that says the request was fine",
              "Nothing is wrong with the request" in out, out)
        check("and says the desktop could not start it",
              "could not start it" in out, out)
        check("with nothing on stderr", err == "", err)

        print("ensure_service, with no desktop to ask")
        out, err, code = run(lambda: yos.ensure_service("notifications"))
        check("still ends the command", code == 1, "exit=%r" % (code,))
        check("naming the service", "notifications" in err, err)
    finally:
        for svc in services:
            svc.close()
        shutil.rmtree(tmp, ignore_errors=True)

    print()
    if FAILURES:
        print("%d check(s) failed: %s" % (len(FAILURES), ", ".join(FAILURES)))
        return 1
    print("all checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
