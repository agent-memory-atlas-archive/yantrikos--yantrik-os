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
  * a grant whose arguments do not match is refused and nothing runs.
"""

import importlib.util
import io
import json
import os
import pathlib
import stat
import sys
import tempfile
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
    with open(STATE, "w") as fh:
        json.dump(s, fh)

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
  act: delete_event(id)  [sensitive, settles on return]
       Delete an event from the calendar. It is not recoverable.
         id: string - the event's id, as list_events reports it
"""

argv = sys.argv[1:]
state = load()

if argv[:1] == ["describe"]:
    target = argv[1]
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
        }
        if state.get("machine_ceiling"):
            body["tool_permission"] = state["machine_ceiling"]
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

    state.setdefault("acted", []).append({"app": target, "action": action, "args": args})
    save(state)
    envelope({"done": True})
    raise SystemExit(0)

die("unknown command %r" % argv)
'''


def load_mcp(fake, state_path, ceiling="standard", requester=""):
    """A fresh copy of the real yos-mcp, pointed at the fake desktop.

    Reloaded per case because the module reads its ceiling and its wait out of the environment
    at import time, which is right for a server started once per session and inconvenient here.
    """
    os.environ["YOS_BIN"] = str(fake)
    os.environ["FAKE_YOS_STATE"] = str(state_path)
    os.environ["YOS_MCP_MAX_PERMISSION"] = ceiling
    os.environ["YOS_MCP_REQUESTER"] = requester
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
         ceiling="standard", requester=""):
    """A scratch desktop in a known mood, and a yos-mcp pointed at it."""
    state_path = tmp / (name + ".json")
    state_path.write_text(json.dumps({
        "answer": answer,
        "machine_ceiling": machine_ceiling,
        "shell_down": shell_down,
    }), encoding="utf-8")
    fake = tmp / "yos"
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
    return json.loads(state_path.read_text(encoding="utf-8"))


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
    check("a denial is an error to the client", is_error, text)
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
          is_error and "could not be spent" in text, text)

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

print()
if failures:
    print("%d failed: %s" % (len(failures), ", ".join(failures)))
    sys.exit(1)
print("all checks passed")
