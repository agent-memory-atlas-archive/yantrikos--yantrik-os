"""A `yos-mcp` that is not `yos-mcp`: enough of the bridge to exercise `McpTools`.

Run with a scenario in argv (default "normal"):

    normal   the handshake, two tools, and calls that answer
    slow     `os_perception` sleeps for 5s, for the timeout path
    crash    exits the moment a tools/call arrives, for the restart path

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

CLIENT = {}


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
                                             if SCENARIO == "slow" else [])})
        elif method == "tools/call":
            params = msg.get("params") or {}
            if SCENARIO == "crash" and not os.environ.get("FAKE_MCP_RESTARTED"):
                # Die mid-call, once. The client should notice, restart us and try again; the
                # marker file is how the second process knows it is the second.
                open(os.environ.get("FAKE_MCP_MARKER", os.devnull), "a").close()
                os._exit(3)
            text, is_error = run_tool(params.get("name"), params.get("arguments") or {})
            reply(msg_id, {"content": [{"type": "text", "text": text}], "isError": is_error})
        elif msg_id is not None:
            reply(msg_id, error={"code": -32601, "message": "unknown method: %s" % method})


if __name__ == "__main__":
    if os.environ.get("FAKE_MCP_MARKER") and os.path.exists(os.environ["FAKE_MCP_MARKER"]):
        os.environ["FAKE_MCP_RESTARTED"] = "1"
    main()
