"""A `yos-mcp` that is not `yos-mcp`: enough of the bridge to exercise `McpTools`.

Run with a scenario in argv (default "normal"):

    normal    the handshake, two tools, and calls that answer
    slow      `os_perception` sleeps for 5s, for the timeout path
    crash     exits the moment a tools/call arrives, for the restart path
    commands  a bridge running as one of the person's agents: the command tools too, answering
              with the shell's account of each command under `_meta`, as yos-mcp does. What a
              command does is decided by its text (see `COMMANDS`); every tools/call is written
              to $FAKE_MCP_CALLS, one JSON line each.

Launched as `[sys.executable, this file, scenario]`, so it needs no exec bit and no shebang.
"""

import json
import os
import sys
import time

SCENARIO = sys.argv[1] if len(sys.argv) > 1 else "normal"

TOOLS = [
    {
        "name": "os_apps",
        "description": "What is on this desktop.",
        "inputSchema": {"type": "object", "properties": {}},
        "annotations": {"readOnlyHint": True},
    },
    {
        "name": "os_act",
        "description": "Do one published action on one app.",
        "inputSchema": {
            "type": "object",
            "properties": {"app": {"type": "string"}, "action": {"type": "string"},
                           "args": {"type": "object", "additionalProperties": True}},
            "required": ["app", "action"],
        },
    },
]

COMMAND_TOOLS = [
    {"name": "run_command", "description": "Run one command line in your own terminal.",
     "inputSchema": {"type": "object", "properties": {"command": {"type": "string"},
                                                      "cwd": {"type": "string"},
                                                      "wait_seconds": {"type": "number"}},
                     "required": ["command"]}},
    {"name": "command_status", "description": "Wait for a running command.",
     "inputSchema": {"type": "object", "properties": {"job": {"type": "string"},
                                                      "wait_seconds": {"type": "number"}},
                     "required": ["job"]}},
    {"name": "command_input", "description": "Type into a running command.",
     "inputSchema": {"type": "object", "properties": {"job": {"type": "string"},
                                                      "text": {"type": "string"}},
                     "required": ["job", "text"]}},
    {"name": "command_kill", "description": "Stop a running command.",
     "inputSchema": {"type": "object", "properties": {"job": {"type": "string"}},
                     "required": ["job"]}},
]


def ended(job, tail, exit_code=None, signal=None):
    out = {"job": job, "agent": "pi:c-fake", "running": False, "tail": tail,
           "tail_clipped": False, "cwd": "/home/me", "cwd_after": "/home/me", "elapsed_ms": 40}
    if signal is None:
        out["exit_code"] = exit_code
    else:
        out.update(signal=signal, signal_name="SIGTERM", killed=True)
    return out


def still(job, tail, waiting=False):
    return {"job": job, "agent": "pi:c-fake", "running": True, "waiting_for_input": waiting,
            "tail": tail, "tail_clipped": False, "cwd": "/home/me", "elapsed_ms": 1000}


# What each command does, by its text: the first answer, then what a later wait finds.
COMMANDS = {
    "echo hi": ended("job-hi", "hi\n", exit_code=0),
    "exit 3": ended("job-3", "boom", exit_code=3),
    "slow": still("job-slow", "working"),
    "forever": still("job-forever", "partial"),
    "ask": still("job-ask", "Password:", waiting=True),
}
LATER = {"job-slow": ended("job-slow", "working\ndone", exit_code=0)}

CLIENT = {}


def command_tool(name, args):
    """The command tools, as yos-mcp answers them: a sentence, and the shell's answer as `_meta`."""
    if name == "run_command":
        if args.get("command") == "refused":
            return "REFUSED — nothing was run. refused: the person said no.", False, None
        answer = COMMANDS.get(args.get("command"), ended("job-x", "", exit_code=0))
    elif name == "command_status":
        answer = LATER.get(args.get("job")) or dict(still(args.get("job"), "partial"))
    elif name == "command_kill":
        answer = ended(args.get("job"), "partial", signal=15)
    else:
        answer = still(args.get("job"), "sent")
    if answer.get("running") and not answer.get("waiting_for_input"):
        # A running command is answered once its wait is up, as the shell does.
        time.sleep(min(float(args.get("wait_seconds") or 0), 2.0))
    head = "still running" if answer["running"] else "ended"
    return "%s (job %s)\n\n%s" % (head, answer["job"], answer["tail"]), False, answer


def reply(msg_id, result=None, error=None):
    out = {"jsonrpc": "2.0", "id": msg_id}
    if error is not None:
        out["error"] = error
    else:
        out["result"] = result
    sys.stdout.write(json.dumps(out) + "\n")
    sys.stdout.flush()


def run_tool(name, args):
    if name == "os_apps":
        # The client's self-declared name is echoed so a test can prove it reached the bridge:
        # it is what the approval card shows the person.
        who = " ".join(str(part) for part in (CLIENT.get("name"), CLIENT.get("version")) if part)
        return "open: notes, files (asked by %s)" % (who or "an unnamed caller"), False
    if name == "os_act":
        if args.get("app") == "shell" and args.get("action") == "forbidden":
            # A policy answer: it ran, it was healthy, it said no. Unflagged on purpose.
            return "REFUSED: plan mode is on, so nothing was changed.", False
        if args.get("app") == "missing":
            return "files is not running, so it cannot be acted on.", True
        return "done: %s.%s" % (args.get("app"), args.get("action")), False
    if name == "os_perception":
        if SCENARIO == "slow":
            time.sleep(5)
        return "nothing much", False
    return "no such tool: %s" % name, True


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except ValueError:
            continue
        method, msg_id = msg.get("method"), msg.get("id")

        if method == "initialize":
            info = (msg.get("params") or {}).get("clientInfo") or {}
            CLIENT["name"] = info.get("name")
            CLIENT["version"] = info.get("version")
            reply(msg_id, {"protocolVersion": "2024-11-05", "capabilities": {"tools": {}},
                           "serverInfo": {"name": "fake-yantrik-os", "version": "0"}})
        elif method == "tools/list":
            reply(msg_id, {"tools": TOOLS + ([{"name": "os_perception",
                                               "description": "Recent observations.",
                                               "inputSchema": {"type": "object", "properties": {}}}]
                                             if SCENARIO == "slow" else [])
                           + (COMMAND_TOOLS if SCENARIO == "commands" else [])})
        elif method == "tools/call":
            params = msg.get("params") or {}
            if SCENARIO == "crash" and not os.environ.get("FAKE_MCP_RESTARTED"):
                # Die mid-call, once. The client should notice, restart us and try again; the
                # marker file is how the second process knows it is the second.
                open(os.environ.get("FAKE_MCP_MARKER", os.devnull), "a").close()
                os._exit(3)
            name, arguments = params.get("name"), params.get("arguments") or {}
            if os.environ.get("FAKE_MCP_CALLS"):
                with open(os.environ["FAKE_MCP_CALLS"], "a", encoding="utf-8") as handle:
                    handle.write(json.dumps({"name": name, "arguments": arguments}) + "\n")
            if SCENARIO == "commands" and name in {t["name"] for t in COMMAND_TOOLS}:
                text, is_error, meta = command_tool(name, arguments)
                result = {"content": [{"type": "text", "text": text}], "isError": is_error}
                if meta is not None:
                    result["_meta"] = {"yantrik/command": meta}
                reply(msg_id, result)
                continue
            text, is_error = run_tool(name, arguments)
            reply(msg_id, {"content": [{"type": "text", "text": text}], "isError": is_error})
        elif msg_id is not None:
            reply(msg_id, error={"code": -32601, "message": "unknown method: %s" % method})


if __name__ == "__main__":
    if os.environ.get("FAKE_MCP_MARKER") and os.path.exists(os.environ["FAKE_MCP_MARKER"]):
        os.environ["FAKE_MCP_RESTARTED"] = "1"
    main()
