#!/usr/bin/env python3
"""Self-test for yos-mcp's approval flow: the real script, against a fake desktop.

    python3 deploy/yantrik-os/yos-mcp-selftest.py

`yos-mcp` reaches the desktop by running `yos`, so the desktop can be faked by putting a script
called `yos` in front of it — which is what this does. Nothing here touches a real machine, a
socket or a window: `YOS_BIN` points at a scratch script that speaks `yos`'s own output format,
records every call, and answers the approval poll from a file the test writes.

Run it under Linux (the fake is an executable script with a shebang; WSL is fine). It takes a few
seconds: two of the cases wait for an approval that never comes.

What it is actually checking, in one line each:

  * a `standard` action still runs with nobody asked;
  * a `sensitive` action asks, and what the card is bound to is what the app will be run with —
    including the type the CLI will coerce the value to, which is the thing that would silently
    drift apart;
  * granted / denied / no-answer / above-the-machine-ceiling / no-shell each produce a distinct
    message, and only the first of them runs anything;
  * a grant whose arguments do not match is refused and nothing runs;
  * each of the four modes does what `design/mind-modes-2026-09-21.md` says it does — including
    that `bypass` still cannot pass the machine ceiling and `plan` refuses browser writes;
  * in `auto`, an action whose own published purpose says it cannot be undone is asked about
    exactly as a `dangerous` one is, the mind is told why, and no session rule covers it;
  * `YOS_MCP_MAX_PERMISSION` can only make things stricter than the desktop's mode;
  * an unreadable desktop falls back to `ask` and says so, rather than assuming anything;
  * an action nobody was asked about is reported to the shell's audit action, with its outcome;
  * and, last, that this bridge's copy of the decision table still agrees with the shell's, on
    every combination of mode, grade, machine ceiling, session rule, browser tool and harness
    cap — read from `mind-mode-vectors.json`, which the shell's own tests generate.
"""

import importlib.util
import io
import json
import os
import pathlib
import stat
import sys
import tempfile
import time
from importlib.machinery import SourceFileLoader

HERE = pathlib.Path(__file__).resolve().parent
SOURCE = HERE / "yos-mcp"

# The fake `yos`. It mirrors the real one's argument parsing (`json.loads` per value, falling
# back to the raw text) and its printing (`result` as indent-2 JSON after the header), because
# those two details are exactly what yos-mcp reads back.
FAKE_YOS = r'''#!/usr/bin/env python3
import json, os, sys

STATE = os.environ["FAKE_YOS_STATE"]

def load():
    with open(STATE) as fh:
        return json.load(fh)

def save(s):
    # Written beside and renamed over. This was `open(STATE, "w")`, which truncates the file
    # and THEN writes it, while the test reads the same file from another process: on a loaded
    # machine a read landed in between and the whole selftest died on a JSONDecodeError about
    # an empty file. It runs in CI now, where a loaded machine is the normal case.
    tmp = STATE + ".tmp%d" % os.getpid()
    with open(tmp, "w") as fh:
        json.dump(s, fh)
    os.replace(tmp, STATE)

def parse_args(pairs):
    out = {}
    for pair in pairs:
        key, value = pair.split("=", 1)
        try:
            out[key] = json.loads(value)
        except ValueError:
            out[key] = value
    return out

def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"))

def envelope(result):
    print("fake desktop")
    print("accepted: True, settled: True")
    print("revision: deadbeef")
    print(json.dumps(result, indent=2))
    print("(state omitted)")

def die(msg):
    sys.stderr.write("yos: " + msg + "\n")
    raise SystemExit(1)

DESCRIBE_CALENDAR = """Calendar - September 2026
revision: c0ffee
{
  "events": [{"id": "evt-3", "title": "Dentist"}]
}
  act: list_events()  [safe, settles on return]
       List the events currently in view.
  act: add_event(title, date)  [standard, settles on return]
       Add an event to the calendar.
         title: string - what it is
         date: string - YYYY-MM-DD
  act: move_event(id, date)  [sensitive, settles on return]
       Move an event to another day. Move it back to undo it.
         id: string - the event's id, as list_events reports it
         date: string - YYYY-MM-DD
  act: delete_event(id)  [sensitive, settles on return]
       Delete an event from the calendar. It is not recoverable.
         id: string - the event's id, as list_events reports it
"""
# Two `sensitive` actions, and the difference between them is the sentence under the signature.
# `move_event` is the routine sensitive surface `auto` exists for; `delete_event` says it cannot
# be undone, so `auto` asks about it anyway. Before 21 September 2026 there was only the second
# one here, and every "auto runs it quietly" case in this file was written against an action the
# desktop should have been asking about.

argv = sys.argv[1:]
state = load()

if argv[:1] == ["describe"]:
    target = argv[1]
    # Every describe is recorded with its whole command line, so the test can tell the READER's
    # form (`--fold`, for a mind paying by the token) from the one the bridge parses for itself
    # (plain, because a folded family has no purpose lines and the card needs one).
    state.setdefault("describes", []).append(argv)
    save(state)
    if target == "shell":
        if state.get("shell_down"):
            die("shell is not open.")
        body = {
            "screen": "desktop",
            "pending_approvals": [],
            # What the desktop says about who is answering. The last-resort source for the name
            # on an approval card, when the MCP client sent no clientInfo.
            "minds": [
                {"id": "builtin", "name": "Yantrik Mind", "answering": False},
                {"id": "hermes", "name": "Hermes Agent", "answering": True},
            ],
            "mind_audit_recent": [],
        }
        # A desktop with no `mode` key at all is the older-shell case: the bridge must fall back
        # to `ask` and say it did, rather than guessing something looser out of a missing field.
        if "mode" in state:
            body["mind_mode"] = {
                "mode": state["mode"],
                "previous": "ask",
                "bypass_expires_in_secs": None,
                "session_rules": state.get("rules", []),
            }
        if state.get("machine_ceiling"):
            body["tool_permission"] = state["machine_ceiling"]
        if "--fold" in argv:
            # What `render_state` does: the same JSON, one top-level key per line, and the
            # shell's `apps` table left out. Parsed from the first `{` exactly as before.
            body.pop("apps", None)
        print("Yantrik - desktop screen")
        print("revision: c0ffee")
        print(json.dumps(body, indent=2))
        print("  act: open_app(name)  [standard, settles later]")
        print("       Launch an app, or focus it if it is already running.")
        raise SystemExit(0)
    if target == "calendar":
        sys.stdout.write(DESCRIBE_CALENDAR)
        raise SystemExit(0)
    die("%s is not open." % target)

if argv[:1] == ["web"]:
    state.setdefault("web", []).append(argv)
    save(state)
    envelope({"navigated": True})
    raise SystemExit(0)

if argv[:1] == ["act"]:
    target, action = argv[1], argv[2]
    args = parse_args(argv[3:])

    if target == "shell" and action == "request_approval":
        if state.get("shell_down"):
            die("shell.app.act refused: the shell is gone")
        state.setdefault("requests", []).append(args)
        rid = "appr-%d" % len(state["requests"])
        state.setdefault("ids", {})[rid] = canonical(args.get("args_json", {}))
        save(state)
        envelope({"request_id": rid, "status": "pending", "expires_in_secs": 120})
        raise SystemExit(0)

    if target == "shell" and action == "approval_status":
        rid = args["request_id"]
        answer = state.get("answer", "pending")
        if rid in state.get("spent", []):
            answer = "consumed"
        envelope({"request_id": rid, "status": answer})
        raise SystemExit(0)

    if target == "shell" and action == "consume_approval":
        rid = args["request_id"]
        if state.get("answer") != "granted":
            die("shell.app.act refused: `%s` is %s" % (rid, state.get("answer")))
        if rid in state.get("spent", []):
            die("shell.app.act refused: `%s` was already used." % rid)
        want = state.get("ids", {}).get(rid)
        got = canonical(args.get("args_json", {}))
        if want != got:
            die("shell.app.act refused: `%s` was approved with arguments %s and this call "
                "carries %s. Nothing was authorised." % (rid, want, got))
        if args.get("app") != state["requests"][int(rid.split("-")[1]) - 1].get("app"):
            die("shell.app.act refused: wrong app. Nothing was authorised.")
        state.setdefault("spent", []).append(rid)
        save(state)
        envelope({"request_id": rid, "consumed": True})
        raise SystemExit(0)

    if target == "shell" and action == "record_unasked_action":
        if state.get("shell_down"):
            die("shell.app.act refused: the shell is gone")
        state.setdefault("audited", []).append(args)
        save(state)
        envelope({"recorded": "%s.%s" % (args.get("app"), args.get("action"))})
        raise SystemExit(0)

    state.setdefault("acted", []).append({"app": target, "action": action, "args": args})
    save(state)
    envelope({"done": True})
    raise SystemExit(0)

die("unknown command %r" % argv)
'''


def load_mcp(fake, state_path, ceiling="standard", requester="", follow=False):
    """A fresh copy of the real yos-mcp, pointed at the fake desktop.

    Reloaded per case because the module reads its ceiling and its wait out of the environment
    at import time, which is right for a server started once per session and inconvenient here.
    """
    os.environ["YOS_BIN"] = str(fake)
    os.environ["FAKE_YOS_STATE"] = str(state_path)
    # `None` means the harness set no cap at all, which is the ordinary case and the one where
    # the desktop's own mode decides alone. An empty or absent variable and a set one are
    # genuinely different to the bridge, so the test has to be able to produce both.
    if ceiling is None:
        os.environ.pop("YOS_MCP_MAX_PERMISSION", None)
    else:
        os.environ["YOS_MCP_MAX_PERMISSION"] = ceiling
    os.environ["YOS_MCP_REQUESTER"] = requester
    # Off for every case but its own: bringing an app forward is one more `act` on the fake
    # desktop, and the cases below count exactly what ran.
    os.environ["YOS_MCP_FOLLOW"] = "1" if follow else "0"
    # Short, because two cases below wait the whole thing out. The shell's own 120s request
    # lifetime is not involved: the fake answers from a file.
    os.environ["YOS_MCP_APPROVAL_WAIT"] = "4"
    loader = SourceFileLoader("yosmcp_under_test", str(SOURCE))
    spec = importlib.util.spec_from_loader("yosmcp_under_test", loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    # Not a production knob: the poll interval only decides how often the fake is asked.
    module.APPROVAL_POLL = 0.05
    return module


failures = []


def check(name, ok, detail=""):
    print(("ok   " if ok else "FAIL ") + name + ("" if ok else "  -- " + str(detail)))
    if not ok:
        failures.append(name)


def act(module, app, action, args):
    return module.run_tool(module.BY_NAME["os_act"], {"app": app, "action": action, "args": args})


def case(tmp, name, answer="granted", machine_ceiling="sensitive", shell_down=False,
         ceiling="standard", requester="", mode="ask", rules=None, no_mode=False):
    """A scratch desktop in a known mood, and a yos-mcp pointed at it.

    `no_mode` publishes a shell that says nothing about its mode — an older desktop, or one
    answering from a version that predates them. The bridge has to fall back to `ask`.
    """
    state_path = tmp / (name + ".json")
    body = {
        "answer": answer,
        "machine_ceiling": machine_ceiling,
        "shell_down": shell_down,
        "rules": rules or [],
    }
    if not no_mode:
        body["mode"] = mode
    fake = tmp / "yos"
    state_path.write_text(json.dumps(body), encoding="utf-8")
    module = load_mcp(fake, state_path, ceiling=ceiling, requester=requester)
    return module, state_path


def handshake(module, name, version):
    """Drive a real MCP `initialize` through the server, as a client would.

    Through `main()` rather than by poking `_CLIENT`, because the thing being tested is that the
    handshake's `clientInfo` is read at all — that is precisely what was being thrown away.
    """
    message = json.dumps({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"clientInfo": {"name": name, "version": version}},
    })
    saved_in, saved_out = sys.stdin, sys.stdout
    sys.stdin = io.StringIO(message + "\n")
    sys.stdout = io.StringIO()
    try:
        module.main()
    finally:
        sys.stdin, sys.stdout = saved_in, saved_out


def read(state_path):
    # The fake writes atomically now; the retry is for the filesystem, not for the fake — a
    # rename is atomic on Linux and merely quick on a Windows-backed mount.
    for attempt in range(20):
        try:
            return json.loads(state_path.read_text(encoding="utf-8"))
        except (ValueError, OSError):
            if attempt == 19:
                raise
            time.sleep(0.05)


with tempfile.TemporaryDirectory() as d:
    tmp = pathlib.Path(d)
    fake = tmp / "yos"
    fake.write_text(FAKE_YOS, encoding="utf-8")
    fake.chmod(fake.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)

    # 1. Below the ceiling: nothing is asked, and the action runs.
    module, state = case(tmp, "below", answer="granted")
    text, is_error = act(module, "calendar", "add_event", {"title": "Dentist", "date": "2026-10-02"})
    s = read(state)
    check("a standard action is not put in front of anybody", not s.get("requests"), s)
    check("a standard action runs", not is_error and len(s.get("acted", [])) == 1, text)

    # 2. Above the ceiling and allowed: asked, bound, spent, run.
    module, state = case(tmp, "granted", answer="granted")
    text, is_error = act(module, "calendar", "delete_event", {"id": "evt-3"})
    s = read(state)
    req = (s.get("requests") or [{}])[0]
    check("a sensitive action asks the person", len(s.get("requests", [])) == 1, s)
    check("the card names the action and its grade",
          req.get("app") == "calendar" and req.get("action") == "delete_event"
          and req.get("grade") == "sensitive", req)
    check("the card carries the app's own sentence about the action",
          "not recoverable" in (str(req.get("purpose")) or "").lower(), req)
    check("with no client name, the card falls back to the mind the desktop says is answering",
          req.get("requester") == "Hermes Agent", req)
    check("the card is bound to the exact arguments",
          req.get("args_json") == {"id": "evt-3"}, req)
    check("the grant is spent exactly once", s.get("spent") == ["appr-1"], s)
    check("and only then does the action run",
          [a["action"] for a in s.get("acted", [])] == ["delete_event"], s)
    check("the answer says a person allowed it",
          not is_error and "allowed this once" in text, text)

    # 3. The argument the app will receive is the argument the person approved.
    #
    # `yos act` parses every value back with json.loads, so a model sending {"id": "3"} means
    # the app receives 3. The card has to say 3 and the grant has to bind 3, or the person
    # approved one thing and the machine did another.
    module, state = case(tmp, "coercion", answer="granted")
    text, is_error = act(module, "calendar", "delete_event", {"id": "3"})
    s = read(state)
    req = (s.get("requests") or [{}])[0]
    check("the approval binds the value the app will actually get",
          req.get("args_json") == {"id": 3}, req)
    check("and the call carries the same value",
          s.get("acted", [{}])[0].get("args") == {"id": 3}, s.get("acted"))
    check("so the grant matches and the action runs", not is_error, text)

    # 4. Denied: nothing runs, and the mind is told not to ask again.
    module, state = case(tmp, "denied", answer="denied")
    text, is_error = act(module, "calendar", "delete_event", {"id": "evt-3"})
    s = read(state)
    check("a denial runs nothing", not s.get("acted"), s)
    # An answer, not a failure: unflagged, and REFUSED in its first word (see `run_tool`). A
    # client that counts isError results takes three of them as a dead server.
    check("a denial is an answer, and says REFUSED first", not is_error and text.startswith("REFUSED"), text)
    check("a denial says the person said no",
          "said no" in text and "do not ask again" in text.lower(), text)

    # 5. Nobody answered: nothing runs, and it does not read as a fault.
    module, state = case(tmp, "silent", answer="pending")
    text, is_error = act(module, "calendar", "delete_event", {"id": "evt-3"})
    s = read(state)
    check("an unanswered request runs nothing", not s.get("acted"), s)
    check("an unanswered request says the person did not answer",
          "did not answer" in text, text)
    check("an unanswered request does not say the machine failed",
          "timed out" not in text and "failed" not in text, text)

    # 6. Above the MACHINE's ceiling: the person is not asked at all.
    module, state = case(tmp, "machine", answer="granted", machine_ceiling="standard")
    text, is_error = act(module, "calendar", "delete_event", {"id": "evt-3"})
    s = read(state)
    check("nothing above the machine's own ceiling is put to the person",
          not s.get("requests"), s)
    check("and nothing runs", not s.get("acted"), s)
    check("the refusal names the machine's standing policy",
          "tool_permission" in text and "NOT asked" in text, text)

    # 7. No shell: say so, run nothing.
    module, state = case(tmp, "noshell", answer="granted", shell_down=True)
    text, is_error = act(module, "calendar", "delete_event", {"id": "evt-3"})
    s = read(state)
    check("no shell means nothing runs", not s.get("acted"), s)
    check("no shell says the person could not be asked",
          "could not be asked" in text and "Nothing was run" in text, text)

    # 8. A grant that does not match what is being consumed authorises nothing.
    #
    # Driven by rewriting the recorded binding behind yos-mcp's back, which is the argument-swap
    # an attacker would attempt: get one thing approved, consume for another.
    module, state = case(tmp, "swap", answer="granted")
    original = module.shell_call

    def swapped(action, args):
        if action == "consume_approval":
            args = dict(args, args_json={"id": "evt-99"})
        return original(action, args)

    module.shell_call = swapped
    text, is_error = act(module, "calendar", "delete_event", {"id": "evt-3"})
    s = read(state)
    check("a swapped argument spends no grant", not s.get("spent"), s)
    check("a swapped argument runs nothing", not s.get("acted"), s)
    check("a swapped argument is reported as not gone through",
          not is_error and text.startswith("REFUSED") and "could not be spent" in text, text)

    # 9. The name on the card comes from the client's own handshake when it sent one.
    #
    # The first cards read "the mind on this desktop", which told the person nothing about who
    # wanted their calendar changed. Hermes declares itself on `initialize`; that is the name.
    module, state = case(tmp, "clientinfo", answer="granted")
    handshake(module, "Hermes Agent", "0.9.2")
    text, is_error = act(module, "calendar", "delete_event", {"id": "evt-3"})
    req = (read(state).get("requests") or [{}])[0]
    check("the card names the client that introduced itself on the MCP handshake",
          req.get("requester") == "Hermes Agent 0.9.2", req)

    # And an operator naming it explicitly outranks both.
    module, state = case(tmp, "override", answer="granted", requester="the tour script")
    handshake(module, "Hermes Agent", "0.9.2")
    act(module, "calendar", "delete_event", {"id": "evt-3"})
    req = (read(state).get("requests") or [{}])[0]
    check("an operator's YOS_MCP_REQUESTER outranks what the client called itself",
          req.get("requester") == "the tour script", req)

    # With nothing at all to go on it says so rather than inventing a name.
    module, _ = case(tmp, "anon", answer="granted")
    check("with no client name and no shell, the requester is honestly unnamed",
          module.requester_name(None) == "an unnamed caller", module.requester_name(None))

    # 10. The two clocks agree: this bridge must give up before the shell drops the request,
    # or it would report "no answer" for one the person had just allowed.
    module, _ = case(tmp, "clocks")
    check("the bridge waits less than the shell holds the request open",
          module.APPROVAL_WAIT < 120, module.APPROVAL_WAIT)
    check("a client is told how long one os_act can take",
          module.OS_ACT_MAX_SECONDS >= module.APPROVAL_WAIT + module.ACT_TIMEOUT,
          module.OS_ACT_MAX_SECONDS)

    # ── The modes ───────────────────────────────────────────────────────────────────────
    #
    # 11. Plan: reading is open, every change is refused, and the refusal asks for the plan
    # rather than reading as a fault. This is the mode a person picks when they want to see
    # what a mind INTENDS, so the one thing it must not do is sound broken.
    module, state = case(tmp, "plan-standard", mode="plan")
    text, is_error = act(module, "calendar", "add_event", {"title": "X", "date": "2026-10-02"})
    s = read(state)
    check("plan mode runs nothing, not even a standard action", not s.get("acted"), s)
    check("plan mode asks nobody", not s.get("requests"), s)
    check("plan mode says it is a setting and asks for the plan",
          "plan mode" in text and "WOULD do" in text and "not a failure" in text, text)

    module, state = case(tmp, "plan-safe", mode="plan")
    text, is_error = act(module, "calendar", "list_events", {})
    check("plan mode still runs a safe action", not is_error and read(state).get("acted"), text)

    # And the browser: reading a page is looking, typing into one is not.
    module, state = case(tmp, "plan-web", mode="plan")
    text, is_error = module.run_tool(module.BY_NAME["web_go"], {"url": "https://example.com/"})
    check("plan mode refuses a browser write",
          not is_error and text.startswith("REFUSED") and "plan mode" in text, text)
    check("and nothing reached the browser", not read(state).get("web"), read(state))
    text, is_error = module.run_tool(module.BY_NAME["web_text"], {})
    check("plan mode still lets the page be read", not is_error, text)

    # 12. Auto: a routine sensitive action runs unasked and is written down.
    #
    # `ceiling=None` from here on, because these cases are about the DESKTOP's mode and a
    # harness that sets no cap is the ordinary case. The cap gets its own cases at 15.
    module, state = case(tmp, "auto", mode="auto", answer="pending", ceiling=None)
    text, is_error = act(module, "calendar", "move_event", {"id": "evt-3", "date": "2026-10-09"})
    s = read(state)
    check("auto runs a sensitive action without asking", not s.get("requests"), s)
    check("and it actually runs", not is_error and len(s.get("acted", [])) == 1, text)
    check("an unasked run is reported to the shell's audit action",
          len(s.get("audited", [])) == 1, s.get("audited"))
    audited = (s.get("audited") or [{}])[0]
    check("the audit line carries the action, grade, mode, arguments and outcome",
          audited.get("app") == "calendar" and audited.get("action") == "move_event"
          and audited.get("grade") == "sensitive" and audited.get("mode") == "auto"
          and audited.get("args_json") == {"id": "evt-3", "date": "2026-10-09"}
          and audited.get("outcome") == "ok", audited)
    check("and the mind is told nobody was asked",
          "Nobody was asked" in text and "auto" in text, text)

    # 12b. And the defect this rule exists for: in `auto`, an action whose own published purpose
    # says it cannot be undone is asked about exactly as a `dangerous` one is.
    #
    # Found live on 21 September 2026. `calendar.delete_event` is graded `sensitive` and says
    # "It is not recoverable"; in `auto` it deleted an event with nobody asked, while the mode
    # menu was promising "You are still asked about the destructive ones". The grade ladder has
    # no rung for "cannot be undone" to sit on, so the app's own sentence decides too.
    module, state = case(tmp, "auto-unrecoverable", mode="auto", answer="granted", ceiling=None)
    text, is_error = act(module, "calendar", "delete_event", {"id": "evt-3"})
    s = read(state)
    check("auto asks about an action the app says cannot be undone",
          len(s.get("requests", [])) == 1, s)
    check("and the card carries the sentence that caused it",
          "not recoverable" in str((s.get("requests") or [{}])[0].get("purpose")).lower(),
          s.get("requests"))
    check("and it runs only once the person has allowed it",
          not is_error and [a["action"] for a in s.get("acted", [])] == ["delete_event"], s)
    # The mind is told WHY, or the honest report it can make is "the desktop is in auto and it
    # asked me anyway", which reads as a fault and is the shape of a thing somebody works around.
    check("and the mind is told why it was asked in auto mode at all",
          "auto" in text and "cannot be undone" in text and "not a fault" in text, text)
    check("nothing was written into the unasked record, because somebody was asked",
          not s.get("audited"), s.get("audited"))

    # A `safe` action is never asked about, whatever its wording says. A read destroys nothing,
    # and a rule that turned looking into a card would be the fastest way to teach somebody that
    # cards are noise. (Nothing published on this OS today is both `safe` and matching; the
    # bridge's own predicate is what is being pinned here.)
    module, _ = case(tmp, "safe-wording", mode="auto", ceiling=None)
    check("a safe action is not asked about however its purpose is worded",
          module.decide("safe", "notes", "read", True, "auto", [], "dangerous") == ("run", False),
          module.decide("safe", "notes", "read", True, "auto", [], "dangerous"))

    # Bypass is the one mode this does not touch. It says "it does not ask" on a red
    # confirmation with a countdown, and a card after that would make the panel a lie — so the
    # action runs and the record is what the person gets instead.
    module, state = case(tmp, "bypass-unrecoverable", mode="bypass",
                         machine_ceiling="dangerous", ceiling=None)
    text, is_error = act(module, "calendar", "delete_event", {"id": "evt-3"})
    s = read(state)
    check("bypass does not ask about it either, because bypass does not ask",
          not s.get("requests") and len(s.get("acted", [])) == 1, s)
    check("and it is written down instead", len(s.get("audited", [])) == 1, s.get("audited"))

    # And the two phrase lists are one list. The shell draws the card's red warning line and
    # refuses a session rule from `approvals::unrecoverable`; this decides whether there is a
    # card at all. Two readings of the same sentence that disagreed would be a desktop warning
    # about something it had already run.
    module, _ = case(tmp, "phrases", ceiling=None)
    rust = (HERE.parent.parent / "crates" / "yantrik-ui" / "src" / "approvals.rs")
    body = rust.read_text(encoding="utf-8")
    start = body.index("pub fn unrecoverable(")
    in_rust = [p for p in module.UNRECOVERABLE_PHRASES
               if '"%s"' % p in body[start:body.index("\n}", start)]]
    check("every phrase this bridge matches on is one the shell matches on",
          len(in_rust) == len(module.UNRECOVERABLE_PHRASES),
          sorted(set(module.UNRECOVERABLE_PHRASES) - set(in_rust)))
    check("and the sentence Calendar actually publishes is one of them",
          module.unrecoverable("Take an event off the calendar. It is not recoverable")
          and not module.unrecoverable("Move a file or folder to recoverable Trash"), None)

    # 13. Bypass: even a dangerous action runs — but only up to the machine's own ceiling.
    module, state = case(tmp, "bypass", mode="bypass", machine_ceiling="dangerous", ceiling=None)
    text, is_error = act(module, "calendar", "delete_event", {"id": "evt-3"})
    s = read(state)
    check("bypass asks nobody", not s.get("requests"), s)
    check("bypass runs it", not is_error and len(s.get("acted", [])) == 1, text)
    check("bypass writes it down anyway", len(s.get("audited", [])) == 1, s.get("audited"))

    module, state = case(tmp, "bypass-ceiling", mode="bypass", machine_ceiling="standard", ceiling=None)
    text, is_error = act(module, "calendar", "delete_event", {"id": "evt-3"})
    s = read(state)
    check("bypass does NOT reach past the machine ceiling", not s.get("acted"), s)
    check("and the refusal names the standing policy, not the mode",
          "tool_permission" in text and "no mode changes it" in text, text)

    # 14. A session rule: the person said "stop asking me about this one".
    module, state = case(tmp, "rule", mode="ask", answer="pending", ceiling=None,
                         rules=[{"app": "calendar", "action": "move_event"}])
    text, is_error = act(module, "calendar", "move_event", {"id": "evt-9", "date": "2026-10-09"})
    s = read(state)
    check("a session rule covers the action with arguments nobody approved",
          not s.get("requests") and len(s.get("acted", [])) == 1, s)
    check("and it is recorded as a rule rather than as the mode",
          (s.get("audited") or [{}])[0].get("mode") == "rule", s.get("audited"))

    module, state = case(tmp, "rule-other", mode="ask", answer="pending",
                         rules=[{"app": "calendar", "action": "list_events"}])
    text, is_error = act(module, "calendar", "move_event", {"id": "evt-3", "date": "2026-10-09"})
    s = read(state)
    check("a rule for one action is not a rule for its neighbour",
          len(s.get("requests", [])) == 1 and not s.get("acted"), s)

    # 14b. And a rule NEVER covers an action the app says cannot be undone, in any mode.
    #
    # Two layers, and this is the second. The card refuses to OFFER one for such an action,
    # which is checked once, at the press; this is the table refusing to honour one, which is
    # checked on every call. They exist separately because an app can reword its own purpose
    # after a rule was made — and because the auto rule at 12b would be worthless if a rule the
    # card would never have made could answer the card it raises.
    for mode in ("ask", "auto"):
        module, state = case(tmp, "rule-unrecoverable-" + mode, mode=mode, answer="pending",
                             ceiling=None,
                             rules=[{"app": "calendar", "action": "delete_event"}])
        text, is_error = act(module, "calendar", "delete_event", {"id": "evt-9"})
        s = read(state)
        check("in `%s`, a rule does not cover what the app says cannot be undone" % mode,
              len(s.get("requests", [])) == 1 and not s.get("acted"), s)
        check("and nothing is recorded as having run under a rule (%s)" % mode,
              not s.get("audited"), s.get("audited"))

    # 15. YOS_MCP_MAX_PERMISSION can only ever be STRICTER than the desktop's mode.
    module, state = case(tmp, "cap-strict", mode="bypass", machine_ceiling="dangerous",
                         ceiling="standard", answer="pending")
    text, is_error = act(module, "calendar", "delete_event", {"id": "evt-3"})
    s = read(state)
    check("a stricter session cap turns a bypass run back into a question",
          len(s.get("requests", [])) == 1 and not s.get("acted"), s)

    module, state = case(tmp, "cap-loose", mode="ask", machine_ceiling="dangerous",
                         ceiling="dangerous", answer="pending")
    text, is_error = act(module, "calendar", "delete_event", {"id": "evt-3"})
    s = read(state)
    check("and a loose session cap cannot make an `ask` desktop stop asking",
          len(s.get("requests", [])) == 1 and not s.get("acted"), s)

    # 15b. With no cap set at all, the desktop's mode is the whole policy — and for a desktop in
    # `ask` mode that is exactly what the historical default of `standard` used to do, so an
    # existing deployment that simply stops setting the variable sees no change.
    module, state = case(tmp, "nocap", mode="ask", answer="pending", ceiling=None)
    text, is_error = act(module, "calendar", "add_event", {"title": "X", "date": "2026-10-02"})
    check("with no cap, an ask desktop still runs a standard action unasked",
          not is_error and not read(state).get("requests"), text)
    text, is_error = act(module, "calendar", "delete_event", {"id": "evt-3"})
    check("and still asks about a sensitive one",
          len(read(state).get("requests", [])) == 1, read(state))

    # 15c. The taint rule is NOT a permission grade and no mode turns it off — not even bypass.
    #
    # A mode says how much the person trusts this mind; the taint says what this session has
    # already read. They are different questions, and a bypass that switched off the second one
    # would turn "do not ask me about things" into "carry my private state out to a web page".
    module, state = case(tmp, "taint-bypass", mode="bypass", machine_ceiling="dangerous",
                         ceiling=None)
    module.run_tool(module.BY_NAME["os_describe"], {"app": "calendar"})
    text, is_error = module.run_tool(module.BY_NAME["web_type"], {"ref": 1, "text": "secret"})
    check("bypass does not switch off the taint rule",
          not is_error and text.startswith("REFUSED") and "already read private state" in text, text)
    check("and nothing reached the browser", not read(state).get("web"), read(state))

    # 16. A desktop that will not say what mode it is in: fall back to `ask`, and say so.
    module, state = case(tmp, "nomode", no_mode=True, answer="pending")
    text, is_error = act(module, "calendar", "delete_event", {"id": "evt-3"})
    s = read(state)
    check("an unreadable mode still asks about a sensitive action",
          len(s.get("requests", [])) == 1, s)
    check("and the fallback is stated rather than assumed silently",
          "could not read the desktop's mind-mode" in text and "fell back to" in text, text)

    module, state = case(tmp, "nomode-standard", no_mode=True)
    text, is_error = act(module, "calendar", "add_event", {"title": "X", "date": "2026-10-02"})
    check("an unreadable mode does not block ordinary work",
          not is_error and read(state).get("acted"), text)

    # 17. The describe a mind reads is folded; the one the bridge parses for itself is not.
    #
    # `--fold` prints a large family of actions as signatures only, which is the whole saving —
    # and a folded family has no purpose lines, which is exactly what `action_detail` needs for
    # the card. The two must not converge.
    module, state = case(tmp, "fold")
    module.run_tool(module.BY_NAME["os_describe"], {"app": "calendar"})
    describes = read(state).get("describes") or []
    check("os_describe asks for the folded form",
          describes and describes[-1] == ["describe", "calendar", "--fold"], describes)

    module, state = case(tmp, "fold-actions")
    module.run_tool(module.BY_NAME["os_describe"], {"app": "calendar", "actions": "files_"})
    describes = read(state).get("describes") or []
    check("os_describe forwards an actions prefix",
          describes and describes[-1] == ["describe", "calendar", "--fold", "--actions", "files_"],
          describes)

    module, state = case(tmp, "fold-detail")
    module.action_detail("calendar", "delete_event")
    describes = read(state).get("describes") or []
    check("action_detail does NOT fold, because the card needs the purpose line",
          describes and describes[-1] == ["describe", "calendar"], describes)
    grade, purpose = module.action_detail("calendar", "delete_event")
    check("and it still reads the purpose the card shows",
          grade == "sensitive" and "not recoverable" in purpose.lower(), (grade, purpose))

    # ── 18. The table, against the shell's own copy of it ───────────────────────────────
    #
    # `decide` in yos-mcp and `mind_mode::Modes::decide` in the shell are the same table
    # written twice. That is deliberate — the bridge deciding for itself costs one read of the
    # desktop per os_act instead of two — and the design note lists it as the top open item,
    # because two copies drift silently in the direction nobody tests.
    #
    # So the shell writes every combination out and this drives the bridge through all of them.
    # Changing `decide` on the Rust side without regenerating fails
    # `mind_mode_the_checked_in_vectors_are_what_decide_produces`; regenerating without changing
    # this side fails here. Neither can move alone.
    module, _ = case(tmp, "vectors")
    vectors_path = HERE / "mind-mode-vectors.json"
    try:
        document = json.loads(vectors_path.read_text(encoding="utf-8"))
        vectors = document.get("vectors") or []
    except (OSError, ValueError) as e:
        vectors = []
        check("the decision-table vectors are checked in", False, e)

    # A missing or emptied file must not pass by testing nothing, which is the usual way a
    # generated fixture quietly stops being a check.
    check("the decision-table vectors are checked in", len(vectors) > 100, len(vectors))
    named = set(v.get("expect") for v in vectors)
    check("every outcome they name is one this bridge can produce",
          named and named <= set(module.OUTCOMES), sorted(named - set(module.OUTCOMES)))

    drifted = []
    for vector in vectors:
        got = module.decide_outcome(vector)
        if got != vector.get("expect"):
            drifted.append("%s: the shell says %s, this bridge says %s"
                           % (vector.get("id"), vector.get("expect"), got))
    check("this bridge decides all %d of them the way the shell does" % len(vectors),
          not drifted,
          "\n     " + "\n     ".join(drifted[:12])
          + ("\n     (and %d more)" % (len(drifted) - 12) if len(drifted) > 12 else ""))

    # And the file covers what it says it covers. A vector set that had quietly lost its
    # bypass rows would agree with anything.
    check("the vectors cover all four modes",
          set(v.get("mode") for v in vectors) == {"plan", "ask", "auto", "bypass"},
          sorted(set(v.get("mode") for v in vectors)))
    check("every grade, and one this OS does not define",
          {"safe", "standard", "sensitive", "dangerous"} <= set(v.get("grade") for v in vectors)
          and any(v.get("expect") == "refuse_grade" for v in vectors), None)
    check("the browser tools and the harness cap as well as the shell's own table",
          set(v.get("layer") for v in vectors) == {"shell", "harness_cap", "browser"},
          sorted(set(v.get("layer") for v in vectors)))
    check("and each of the six outcomes actually occurs somewhere in them",
          named == set(module.OUTCOMES), sorted(set(module.OUTCOMES) - named))

    # The `unrecoverable` axis, and that it is a real axis rather than a field nobody varies.
    #
    # It was carried for a day as `recoverable`, expected to change nothing, and the file said
    # so. It decides the table now, so the check has to be the opposite one: both values are
    # present, and somewhere among them there is a pair identical in every other input whose
    # outcomes differ. A dimension that never changes an answer is a column, not a test.
    check("both values of `unrecoverable` are covered",
          {False, True} <= set(bool(v.get("unrecoverable")) for v in vectors),
          sorted(set(str(v.get("unrecoverable")) for v in vectors)))

    # Paired on the id with the `unrecoverable=` segment taken out, not on the fields: the two
    # halves of a pair are generated against DIFFERENT actions (`files.move` and
    # `calendar.delete_event`, so a vector reads like something a person could check on a real
    # machine), and their `rules` therefore differ in spelling while meaning the same shape.
    others = {}
    for v in vectors:
        key = "/".join(part for part in str(v.get("id")).split("/")
                       if not part.startswith("unrecoverable="))
        others.setdefault(key, {})[bool(v.get("unrecoverable"))] = v.get("expect")
    paired = [by for by in others.values() if len(by) == 2]
    check("every case is generated both ways, so the axis is a real one",
          len(paired) * 2 + 24 == len(vectors), (len(paired), len(vectors)))
    moved = [by for by in paired if by[False] != by[True]]
    check("and it changes the answer somewhere: %d cells turn on the app's own sentence"
          % len(moved), bool(moved), None)
    # The cell the defect was reported from, named rather than counted.
    auto_sensitive = [v for v in vectors
                      if v.get("layer") == "shell" and v.get("mode") == "auto"
                      and v.get("grade") == "sensitive" and v.get("ceiling") == "dangerous"
                      and not v.get("rules")]
    by_undo = {bool(v.get("unrecoverable")): v.get("expect") for v in auto_sensitive}
    check("auto runs a recoverable sensitive action and asks about one that cannot be undone",
          by_undo == {False: "run_logged", True: "ask"}, by_undo)

    # The person can see what the mind is doing: one raise per change of app, never per action,
    # never for the shell, and never in the way of the action it follows.
    state = tmp / "follow.json"
    state.write_text(json.dumps({"answer": "granted", "machine_ceiling": "sensitive", "mode": "auto"}))
    module = load_mcp(fake, state, ceiling=None, follow=True)
    for title in ("One", "Two"):
        act(module, "calendar", "add_event", {"title": title, "date": "2026-10-02"})
    act(module, "shell", "open_app", {"name": "notes"})
    act(module, "calendar", "list_events", {})
    shown = [a["args"].get("name") for a in read(state).get("acted", []) if a.get("action") == "show_app"]
    check("an app is brought forward once when the mind moves to it, not once per action",
          shown == ["calendar"], shown)
    module._FOLLOWING[0] = "notes"   # the mind has been elsewhere since
    act(module, "calendar", "list_events", {})
    shown = [a["args"].get("name") for a in read(state).get("acted", []) if a.get("action") == "show_app"]
    check("and again when it comes back from another app", shown == ["calendar", "calendar"], shown)

print()
if failures:
    print("%d failed: %s" % (len(failures), ", ".join(failures)))
    sys.exit(1)
print("all checks passed")
