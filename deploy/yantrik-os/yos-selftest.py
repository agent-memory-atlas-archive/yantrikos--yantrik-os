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
  * `ensure_service`, which the notify path uses, still ends the command when the service
    will not come up;
  * `yos act` sends each argument as the type the app declared for it — `dismiss id=67`
    reaches the service as the string "67", because that action publishes `id: string`, while
    `add_event duration_min=30` still arrives as the number 30;
  * and when an app refuses an action for want of a grant — `sensitive` in `ask` mode, which
    the app's own dispatch refuses on every door since issue #116 — `yos act` asks the shell
    for the person's Allow, says so, waits, and acts again carrying the grant; a denial and an
    unanswered card are plain sentences, `--no-ask` hands the refusal back, and `--grant`
    carries one already held;
  * an agent's token — `--agent-token`, `YANTRIK_AGENT_TOKEN`, or an `agent_token=` argument —
    rides beside the arguments on `app.act` and never among them, so the approval card it may
    raise is asked for with the arguments alone; and an act told to `wait` is given longer than
    its wait.
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
        # `str`, like the MCP bridge's own selftest: a check handed the dict it was comparing
        # used to die here with a TypeError instead of printing what it saw, which turns a
        # readable failure into a traceback in the middle of the run.
        print("  FAIL  %s%s" % (label, ("\n        " + str(detail)) if detail else ""))
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
                # A reply of `{"__error__": {...}}` is a JSON-RPC error — how an app's dispatch
                # refuses, and what `yos act` has to be able to read past.
                if isinstance(answer, dict) and set(answer) == {"__error__"}:
                    conn.sendall((json.dumps({"jsonrpc": "2.0", "id": asked.get("id"),
                                              "error": answer["__error__"]}) + "\n").encode())
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

# Two surfaces as they really publish themselves, because the types in here are the whole point:
# the notification ids on a live machine are a decimal counter rendered as a string, and the
# calendar is where a number and a flag genuinely are a number and a flag.
NOTIFICATIONS = {
    "app": "notifications",
    "summary": "Notifications — 2 showing, 1 unread",
    "state": {"count": 2, "unread": 1},
    "revision": "dd6f0b179da77956",
    "actions": [
        {"name": "dismiss", "description": "Dismiss one notification by id",
         "permission": "standard", "settles": "on return",
         "parameters": {"type": "object", "required": ["id"], "properties": {
             "id": {"type": "string",
                    "description": "The notification id, as shown in the list"}}}},
        {"name": "mark_read", "description": "Clear the unread badge",
         "permission": "standard", "settles": "on return",
         "parameters": {"type": "object", "required": [], "properties": {
             "id": {"type": "string", "description": "One notification, or omit for all"}}}},
    ],
}

CALENDAR = {
    "app": "calendar",
    "summary": "Calendar — September 2026",
    "state": {"showing": "September 2026"},
    "revision": "1234567890123456",
    "actions": [
        {"name": "add_event", "description": "Put something on the calendar",
         "permission": "standard", "settles": "on return",
         "parameters": {"type": "object", "required": ["title", "date"], "properties": {
             "title": {"type": "string", "description": ""},
             "date": {"type": "string", "description": "YYYY-MM-DD"},
             "duration_min": {"type": "number", "description": "How long it runs, in minutes"},
             "all_day": {"type": "boolean", "description": "A whole day rather than a time"}}}},
    ],
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

    # A token in the environment this runs in would ride on every act below and change what they
    # send; the agent-token checks set their own.
    os.environ.pop("YANTRIK_AGENT_TOKEN", None)
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

        print("yos act, against the types the app publishes")
        # `parse_args` ran every value through `json.loads`, so `dismiss id=67` sent the number
        # 67 to an action that publishes `id: string` and the service answered "missing `id`" —
        # a mind that had read the id off the list could not dismiss a notification at all.

        def surface(view):
            """A control surface that describes itself and accepts anything."""
            def reply(_self, asked):
                if asked["method"] == "app.describe":
                    return view
                return {"summary": view["summary"], "accepted": True, "settled": True,
                        "revision": view["revision"], "result": {"ok": True}}
            return reply

        surfaces = {}
        for view in (NOTIFICATIONS, CALENDAR):
            service = FakeService(sockets / ("%s.sock" % view["app"]), surface(view))
            service.start()
            services.append(service)
            surfaces[view["app"]] = service

        def last_act(app):
            acts = [c for c in surfaces[app].calls if c["method"] == "app.act"]
            return acts[-1]["params"] if acts else None

        def describes(app):
            return len([c for c in surfaces[app].calls if c["method"] == "app.describe"])

        run(lambda: yos.cmd_act(["notifications", "dismiss", "id=67"]))
        check("an id bound for a `string` parameter arrives as the text it was typed as",
              (last_act("notifications") or {}).get("args") == {"id": "67"},
              last_act("notifications"))
        check("and the app was asked once what its arguments are",
              describes("notifications") == 1, describes("notifications"))

        surfaces["notifications"].calls.clear()
        run(lambda: yos.cmd_act(["notifications", "dismiss", "id=abc"]))
        check("a value that is not JSON in the first place costs no extra round trip",
              describes("notifications") == 0, surfaces["notifications"].calls)

        surfaces["notifications"].calls.clear()
        run(lambda: yos.cmd_act(["notifications", "dismiss", 'id="67"']))
        check("quoting still says `string` explicitly, without the quotes surviving",
              (last_act("notifications") or {}).get("args") == {"id": "67"},
              last_act("notifications"))

        surfaces["notifications"].calls.clear()
        run(lambda: yos.cmd_act(["notifications", "dismiss", "id=67", "reason=3"]))
        check("an argument the app does not publish is still read as JSON, as it always was",
              (last_act("notifications") or {}).get("args") == {"id": "67", "reason": 3},
              last_act("notifications"))

        surfaces["notifications"].calls.clear()
        # 16 hex digits, and about one revision in six thousand is all of them decimal. It
        # guards the call rather than being an argument to it, so no app declares it — and as a
        # number the runtime reads it with `as_str`, finds nothing, and acts without the guard.
        run(lambda: yos.cmd_act(["notifications", "dismiss", "id=67",
                                 "expect_revision=1234567890123456"]))
        check("a revision made only of digits is still a revision",
              (last_act("notifications") or {}).get("expect_revision") == "1234567890123456",
              last_act("notifications"))

        run(lambda: yos.cmd_act(["calendar", "add_event", "title=Dentist", "date=2026-10-02",
                                 "duration_min=30", "all_day=false"]))
        check("a number stays a number and a flag stays a flag",
              (last_act("calendar") or {}).get("args") == {
                  "title": "Dentist", "date": "2026-10-02",
                  "duration_min": 30, "all_day": False},
              last_act("calendar"))

        print("yos act, when the desktop wants a person's Allow")
        # The account from inside VM 520 (issue #116): `blender.render` is `sensitive`, the
        # machine was in `ask` mode, and `yos act` ran it in 1.72 s with no card. The app's own
        # dispatch refuses that now, on every door, and says how to get a grant. `yos` has to
        # read that refusal, ask the shell, say so, wait for the person, and act again with the
        # grant — the same three steps the MCP bridge does for a mind, for whoever is at a
        # terminal.
        REFUSAL = ("GRANT: blender.render is graded `sensitive` and this machine is in ask mode, "
                   "which runs nothing above `standard` without asking — so it was not run. Ask "
                   "the shell for approval first (`request_approval` with this app, action and "
                   "these exact arguments, poll `approval_status`, then send the granted "
                   "request_id as `grant` on app.act — `yos act` does all of that for you), or "
                   "have the person at the machine press Allow when the card appears.")
        RENDERED = {"summary": "Blender — rendered", "accepted": True, "settled": True,
                    "revision": "b1", "result": {"rendered_to": "x.png"}}
        answers = {"status": "granted"}
        polls = []

        def blender_reply(_self, asked):
            params = asked.get("params") or {}
            if asked["method"] != "app.act":
                return {"app": "blender", "summary": "Blender — cube.blend", "state": {},
                        "revision": "b0", "actions": []}
            if params.get("grant") == "appr-7":
                return RENDERED
            if params.get("grant"):
                return {"__error__": {"code": -32602, "message": (
                    "GRANT: `%s` does not authorise blender.render — no approval request `%s`. "
                    "Nothing was run" % (params["grant"], params["grant"]))}}
            return {"__error__": {"code": -32602, "message": REFUSAL}}

        def asking_shell_reply(_self, asked):
            params = asked.get("params") or {}
            action = params.get("action")
            if asked["method"] == "app.act" and action == "request_approval":
                return {"accepted": True, "settled": True, "result": {
                    "request_id": "appr-7", "status": "pending", "expires_in_secs": 120}}
            if asked["method"] == "app.act" and action == "approval_status":
                polls.append(1)
                # Pending on the first poll, so the wait is a real wait.
                status = answers["status"] if len(polls) >= 2 else "pending"
                return {"accepted": True, "settled": True,
                        "result": {"request_id": "appr-7", "status": status}}
            return {"accepted": True, "settled": True}

        blender = FakeService(sockets / "app-blender.sock", blender_reply)
        blender.start()
        services.append(blender)
        shell.reply = asking_shell_reply
        shell.calls.clear()
        yos.APPROVAL_POLL = 0.02

        def acts():
            return [c["params"] for c in blender.calls if c["method"] == "app.act"]

        def asked():
            return [c["params"]["args"] for c in shell.calls
                    if c["method"] == "app.act" and c["params"].get("action") == "request_approval"]

        out, err, code = run(lambda: yos.cmd_act(["blender", "render", "out=x.png"]))
        check("the refusal made yos ask the shell for the person's Allow, once",
              len(asked()) == 1, shell.calls)
        check("with the app, action, grade and the exact arguments the grant binds",
              asked() and asked()[0].get("app") == "blender" and asked()[0].get("action") == "render"
              and asked()[0].get("grade") == "sensitive" and asked()[0].get("args_json") == {"out": "x.png"},
              asked())
        check("and said so on the terminal",
              "asking — a card is on the screen (120 s)" in out, out)
        check("the action was sent once without a grant and once with the one the person gave",
              [a.get("grant") for a in acts()] == [None, "appr-7"], acts())
        check("carrying the same arguments both times",
              all(a.get("args") == {"out": "x.png"} for a in acts()), acts())
        check("and the second one ran, with nothing on stderr",
              "accepted: True" in out and code is None and err == "", (out, err, code))

        answers["status"] = "denied"
        polls.clear()
        blender.calls.clear()
        shell.calls.clear()
        out, err, code = run(lambda: yos.cmd_act(["blender", "render", "out=x.png"]))
        check("a denial runs nothing", [a.get("grant") for a in acts()] == [None], acts())
        check("and is a plain sentence that ends the command",
              code == 1 and "said no" in err and "Traceback" not in err, (err, code))

        answers["status"] = "pending"
        polls.clear()
        blender.calls.clear()
        yos.APPROVAL_WAIT = 0.2
        out, err, code = run(lambda: yos.cmd_act(["blender", "render", "out=x.png"]))
        check("an unanswered card runs nothing and says nobody answered",
              code == 1 and "nobody answered" in err and len(acts()) == 1, (err, acts()))
        yos.APPROVAL_WAIT = 120

        shell.calls.clear()
        blender.calls.clear()
        out, err, code = run(lambda: yos.cmd_act(["blender", "render", "out=x.png", "--no-ask"]))
        check("--no-ask hands the refusal back instead of asking",
              code == 1 and "GRANT:" in err and not asked(), (err, shell.calls))
        check("in the app's own words, without the transport's prefix",
              "app.act refused:" not in err, err)

        blender.calls.clear()
        out, err, code = run(lambda: yos.cmd_act(["blender", "render", "out=x.png", "--grant", "appr-7"]))
        check("--grant carries a grant already held, and asks nobody",
              [a.get("grant") for a in acts()] == ["appr-7"] and code is None and not asked(),
              (acts(), err))

        blender.calls.clear()
        out, err, code = run(lambda: yos.cmd_act(["blender", "render", "out=x.png", "--grant", "stale"]))
        check("a grant that does not hold is a refusal, not a second card",
              code == 1 and "does not authorise" in err and not asked() and len(acts()) == 1,
              (err, acts()))

        print("yos act, for one of the person's agents")
        # The agent token says which agent a call is for. It rides BESIDE the arguments, never
        # among them: the arguments are what the approval card shows and the audit log keeps,
        # and a token in either is a token anyone reading them could replay.

        def shell_acts(action):
            return [c["params"] for c in shell.calls
                    if c["method"] == "app.act" and c["params"].get("action") == action]

        shell.calls.clear()
        run(lambda: yos.cmd_act(["shell", "agent_run", "command=ls -la", "--agent-token", "tok-flag"]))
        sent = shell_acts("agent_run")
        check("--agent-token travels beside the arguments",
              sent and sent[-1].get("agent_token") == "tok-flag", sent)
        check("and never among them", sent and sent[-1].get("args") == {"command": "ls -la"}, sent)

        os.environ["YANTRIK_AGENT_TOKEN"] = "tok-env"
        try:
            shell.calls.clear()
            run(lambda: yos.cmd_act(["shell", "agent_run", "command=ls"]))
            sent = shell_acts("agent_run")
            check("YANTRIK_AGENT_TOKEN, as a harness sets it, is carried the same way",
                  sent and sent[-1].get("agent_token") == "tok-env"
                  and sent[-1].get("args") == {"command": "ls"}, sent)
            shell.calls.clear()
            run(lambda: yos.cmd_act(["shell", "agent_run", "command=ls", "--agent-token", "tok-flag"]))
            sent = shell_acts("agent_run")
            check("a token given on the command line wins over the environment's",
                  sent and sent[-1].get("agent_token") == "tok-flag", sent)
        finally:
            os.environ.pop("YANTRIK_AGENT_TOKEN", None)

        shell.calls.clear()
        run(lambda: yos.cmd_act(["shell", "agent_run", "command=ls", "agent_token=0042"]))
        sent = shell_acts("agent_run")
        check("an agent_token= argument is lifted out beside the rest, as the text it was typed as",
              sent and sent[-1].get("agent_token") == "0042"
              and sent[-1].get("args") == {"command": "ls"}, sent)

        shell.calls.clear()
        run(lambda: yos.cmd_act(["shell", "agent_run", "command=ls"]))
        sent = shell_acts("agent_run")
        check("with no token anywhere, none is sent", sent and "agent_token" not in sent[-1], sent)

        # The card: what the person is shown, and what a grant is bound to, is the arguments —
        # so the token has to be absent from the request and present on both acts.
        answers["status"] = "granted"
        polls.clear()
        blender.calls.clear()
        shell.calls.clear()
        out, err, code = run(lambda: yos.cmd_act(["blender", "render", "out=x.png",
                                                  "--agent-token", "tok-card"]))
        check("the card is asked for with the arguments alone",
              asked() and asked()[0].get("args_json") == {"out": "x.png"}, asked())
        check("and nothing sent to the shell carries the token",
              "tok-card" not in json.dumps(shell.calls), shell.calls)
        check("while both acts carry it beside the same arguments",
              [a.get("agent_token") for a in acts()] == ["tok-card", "tok-card"]
              and all(a.get("args") == {"out": "x.png"} for a in acts())
              and [a.get("grant") for a in acts()] == [None, "appr-7"], acts())

        check("an act told to wait is given longer than its wait before yos gives up on it",
              yos.act_timeout("agent_run", {}) == 140
              and yos.act_timeout("agent_run", {"wait": 600}) == 620
              and yos.act_timeout("agent_job", {"wait": 0}) == 40
              and yos.act_timeout("dismiss", {"id": "67"}) == 40,
              [yos.act_timeout("agent_run", {}), yos.act_timeout("agent_run", {"wait": 600})])

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
