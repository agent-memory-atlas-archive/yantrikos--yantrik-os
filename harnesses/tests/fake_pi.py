"""A `pi --mode rpc` that is not pi: the RPC lines, and nothing behind them.

Scenario comes from argv (everything after the flags pi itself would take, so the harness's own
argv building is exercised unchanged) or from $FAKE_PI_SCENARIO:

    text      a couple of text deltas, then agent_end + agent_settled
    tool      a tool execution around the text
    dialog    an extension_ui_request confirm, which must come back cancelled
    abort     never finishes on its own; answers `abort` with agent_end + agent_settled
    exit      dies in the middle of the turn
    silent    acknowledges the prompt and then says nothing at all, ever
    noend     text and agent_end, but never agent_settled — the grace path
    refuse    answers the prompt command with success:false

Launched as `[sys.executable, this file, scenario]`, so it needs no exec bit and no shebang.
"""

import json
import os
import sys
import threading

SCENARIO = os.environ.get("FAKE_PI_SCENARIO") or (sys.argv[1] if len(sys.argv) > 1 else "text")
# What the harness passed us, written out so a test can assert on the command line it builds.
ARGV_DUMP = os.environ.get("FAKE_PI_ARGV_DUMP")
# Every command line the harness sends, for the dialog and abort assertions.
COMMANDS_DUMP = os.environ.get("FAKE_PI_COMMANDS_DUMP")


def emit(event):
    sys.stdout.write(json.dumps(event) + "\n")
    sys.stdout.flush()


def record(path, value):
    if not path:
        return
    with open(path, "a", encoding="utf-8") as handle:
        handle.write(value + "\n")


def settle():
    emit({"type": "turn_end"})
    emit({"type": "agent_end", "messages": [], "willRetry": False})
    emit({"type": "agent_settled"})


def handle_prompt(command):
    emit({"type": "response", "command": "prompt", "id": command.get("id"),
          "success": SCENARIO != "refuse",
          **({"error": "no provider configured"} if SCENARIO == "refuse" else {})})
    if SCENARIO == "refuse":
        return
    emit({"type": "agent_start"})

    if SCENARIO == "exit":
        emit({"type": "message_update",
              "assistantMessageEvent": {"type": "text_delta", "delta": "starting"}})
        sys.stdout.flush()
        os._exit(7)

    if SCENARIO == "silent":
        return

    if SCENARIO == "tool":
        emit({"type": "message_update",
              "assistantMessageEvent": {"type": "thinking_delta", "delta": "hmm, notes"}})
        emit({"type": "tool_execution_start", "toolCallId": "t1", "toolName": "os_act",
              "args": {"app": "calendar", "action": "add_event", "args": {"title": "dentist"}}})
        emit({"type": "tool_execution_end", "toolCallId": "t1", "toolName": "os_act",
              "result": {"content": [{"type": "text", "text": "done"}]}, "isError": False})
        emit({"type": "message_update",
              "assistantMessageEvent": {"type": "text_delta", "delta": "Added it."}})
        settle()
        return

    if SCENARIO == "dialog":
        emit({"type": "extension_ui_request", "id": "ui-1", "method": "confirm",
              "params": {"message": "Delete every file in ~/work?"}})
        return  # finished only once the harness answers

    if SCENARIO == "abort":
        emit({"type": "message_update",
              "assistantMessageEvent": {"type": "text_delta", "delta": "working"}})
        return  # only `abort` ends this

    if SCENARIO == "noend":
        emit({"type": "message_update",
              "assistantMessageEvent": {"type": "text_delta", "delta": "Nearly done."}})
        emit({"type": "turn_end"})
        emit({"type": "agent_end", "messages": [], "willRetry": False})
        return  # deliberately no agent_settled

    emit({"type": "message_update",
          "assistantMessageEvent": {"type": "thinking_delta", "delta": "let me think"}})
    for piece in ("Two ", "windows."):
        emit({"type": "message_update",
              "assistantMessageEvent": {"type": "text_delta", "delta": piece}})
    settle()


def main():
    record(ARGV_DUMP, json.dumps(sys.argv[1:]))
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            command = json.loads(line)
        except ValueError:
            continue
        record(COMMANDS_DUMP, line)
        kind = command.get("type")

        if kind == "prompt":
            threading.Thread(target=handle_prompt, args=(command,), daemon=True).start()
        elif kind == "abort":
            emit({"type": "response", "command": "abort", "id": command.get("id"), "success": True})
            emit({"type": "message_update",
                  "assistantMessageEvent": {"type": "text_delta", "delta": " — stopped."}})
            settle()
        elif kind == "new_session":
            emit({"type": "response", "command": "new_session", "id": command.get("id"),
                  "success": True})
        elif kind == "extension_ui_response":
            # The whole point of the dialog scenario: whatever the harness said, it is recorded
            # above and the turn ends here rather than hanging on an unanswered dialog.
            emit({"type": "message_update", "assistantMessageEvent":
                  {"type": "text_delta", "delta": "I will not do that then."}})
            settle()
        elif kind == "get_state":
            emit({"type": "response", "command": "get_state", "id": command.get("id"),
                  "success": True})


if __name__ == "__main__":
    if "--version" in sys.argv:
        print("pi 0.87.0 (fake)")
        sys.exit(0)
    main()
