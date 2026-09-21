"""The half of a harness that is not the mind.

Every harness in this repo does the same six-method dance with the desktop (docs/harness.md) and
the same stdio dance with `yos-mcp`, and gets the same handful of things wrong when it is written
again from scratch: a turn closed twice, a turn never closed, a heartbeat that stops when the work
gets slow, a second message swallowed while the first one is running. That is what is here, once,
so a new harness is only the part that produces an answer.

Stdlib only, Python 3.10 and later. Nothing here imports the Hermes plugin — the discovery rules
are reimplemented rather than shared, because `harnesses/hermes` ships to a machine that has
Hermes and this ships to machines that do not.

What a harness supplies is a handler:

    class Mind(Handler):
        concurrent = False                 # can two turns run at once?
        def answer(self, turn): ...        # turn.text in, turn.emit(...) out
        def reset(self): ...               # /new
        def cancel(self, turn): ...        # /stop, on top of turn.cancelled being set

    Harness("deepseek", "DeepSeek", Mind(), detail="…", tools=True).run()

The one rule the desktop actually enforces: a turn that was handed over is owed exactly one
`complete` or `fail`. Whatever the handler does — return, raise, emit nothing, get stopped — that
happens here, once, in `_close`.
"""

from __future__ import annotations

import json
import os
import socket
import subprocess
import sys
import threading
import time
from pathlib import Path
from typing import Any, Callable, Dict, List, Optional, Sequence, Tuple, Union

# ── The wire ────────────────────────────────────────────────────────────────────────────

ATTACH = "harness.attach"
POLL = "harness.poll"
CHUNK = "harness.chunk"
COMPLETE = "harness.complete"
FAIL = "harness.fail"
DETACH = "harness.detach"

# How long to wait after an empty poll. The protocol's own pacing; the host does not hold a poll
# open, so this is the client's wait and nothing else (crates/yantrik-harness/src/protocol.rs).
POLL_INTERVAL = 0.2
# The desktop drops a harness that has not called anything for 90 seconds. A turn being worked on
# says so this often, with a chunk whose delta is empty — presence, not text.
HEARTBEAT_SECONDS = 20.0
# How long to wait before looking for the desktop again after it went away.
RETRY_SECONDS = 5.0
# What the person is told when they type while the mind is mid-answer. A message that arrives
# during a turn is a turn too, and it is owed an answer — queueing it is how a chat app behaves
# and here it leaves the desktop waiting on something nobody will ever close.
BUSY_REPLY = "still working on the previous request — ask again in a moment, or say /stop"


class HarnessError(Exception):
    """The desktop refused a call, or could not be reached."""


def socket_path() -> Optional[str]:
    """Where the desktop's harness socket is, if there is one.

    The same places in the same order as the OS binds them, because the shell takes the first
    directory it can write and a client has to find the one it actually chose.
    """
    explicit = os.environ.get("YANTRIK_HARNESS_SOCKET", "").strip()
    if explicit:
        # Named outright: honour it and look nowhere else, so pointing a harness at one desktop
        # can never silently fall through to another.
        return explicit if Path(explicit).exists() else None

    candidates: List[Path] = []
    runtime = os.environ.get("XDG_RUNTIME_DIR", "").strip()
    if runtime:
        candidates.append(Path(runtime) / "yantrik")
    candidates.append(Path("/run/yantrik"))
    try:
        candidates.extend(sorted(p for p in Path("/tmp").iterdir() if p.name.startswith("yantrik-")))
    except OSError:
        pass
    for directory in candidates:
        sock = directory / "harness.sock"
        if sock.exists():
            return str(sock)
    return None


def call(address: str, method: str, params: Dict[str, Any], timeout: float = 10.0) -> Any:
    """One round trip: connect, write a line, read a line, close."""
    request = json.dumps({"jsonrpc": "2.0", "method": method, "params": params, "id": 1})
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as conn:
            conn.settimeout(timeout)
            conn.connect(address)
            conn.sendall(request.encode("utf-8") + b"\n")
            buf = b""
            while not buf.endswith(b"\n"):
                piece = conn.recv(65536)
                if not piece:
                    break
                buf += piece
    except OSError as exc:
        raise HarnessError("%s: %s" % (method, exc)) from exc
    if not buf.strip():
        raise HarnessError("%s: the desktop closed the connection without answering" % method)
    try:
        reply = json.loads(buf)
    except ValueError as exc:
        raise HarnessError("%s: unreadable reply %r" % (method, buf[:200])) from exc
    if isinstance(reply, dict) and reply.get("error"):
        err = reply["error"]
        raise HarnessError(err.get("message", str(err)) if isinstance(err, dict) else str(err))
    return reply.get("result") if isinstance(reply, dict) else None


# ── The tool trail ──────────────────────────────────────────────────────────────────────


def tool_trail(name: str, arguments: Optional[Dict[str, Any]] = None) -> str:
    """The one line a tool call contributes to the conversation.

    Every harness shows tool use the same way the panel already shows it for Hermes, because the
    person reading it should not have to learn a second vocabulary when they switch minds.

    Arguments are deliberately left out, with one exception. A tool call's arguments routinely
    hold the thing the person would least like repeated back on screen — the body of the note,
    the text of the message, the search that was run. `app` and `action` are the two that name
    what was touched rather than what was said, and they are the two that make the line useful:
    `os_act` alone says nothing, `os_act calendar.add_event` says what happened.
    """
    label = str(name or "tool")
    args = arguments if isinstance(arguments, dict) else {}
    app = str(args.get("app") or "").strip()
    action = str(args.get("action") or "").strip()
    # os_describe lists actions as `new_note()`; a model copying that sends the punctuation too.
    action = action.split("(", 1)[0].strip()
    if app and action:
        label = "%s %s.%s" % (label, app, action)
    elif app:
        label = "%s %s" % (label, app)
    return "⚙️ %s" % label


# ── One turn ────────────────────────────────────────────────────────────────────────────


class Turn:
    """One thing the person typed, and the answer being streamed back to it."""

    def __init__(self, harness: "Harness", session: str, turn_id: int, text: str,
                 context: Optional[str] = None) -> None:
        self.harness = harness
        self.session = session
        self.turn_id = turn_id
        self.text = text
        self.context = context
        # Set when the person said /stop, or /new arrived while this was running. A handler that
        # watches it can stop between steps; one that does not is simply left to finish.
        self.cancelled = threading.Event()
        # The panel stopped listening (the desktop answered a chunk with {"dropped": true}).
        self.dropped = False
        self.closed = False
        self.said_anything = False
        # When this turn last said anything to the desktop, so the heartbeat only fires for a
        # turn that has actually gone quiet.
        self.last_call = time.monotonic()
        self._tail = ""          # the last delta, so the trail knows whether a newline is owed
        self._lock = threading.Lock()

    # The two calls a handler makes.

    def emit(self, delta: str) -> bool:
        """Stream a piece of the answer. False means the panel is no longer listening."""
        if not delta:
            # An empty delta is the heartbeat's meaning, not text. A handler that produced no
            # characters should not accidentally send one.
            return not self.dropped
        with self._lock:
            self.said_anything = True
            self._tail = delta
        return self.harness._chunk(self, delta)

    def tool(self, name: str, arguments: Optional[Dict[str, Any]] = None) -> bool:
        """Note a tool call in the conversation, framed the way the panel expects."""
        with self._lock:
            lead = "" if (not self.said_anything or self._tail.endswith("\n")) else "\n"
        return self.emit(lead + tool_trail(name, arguments) + "\n\n")


class Handler:
    """What a harness needs from the mind behind it.

    Subclass or duck-type. Only `answer` is required.
    """

    #: Two turns at once, or one at a time? A mind with a single conversation and a single
    #: subprocess says False and gets the "still working" answer for free.
    concurrent = False

    def answer(self, turn: Turn) -> None:
        raise NotImplementedError

    def reset(self) -> None:
        """/new — forget the conversation so far."""

    def cancel(self, turn: Turn) -> None:
        """/stop — on top of `turn.cancelled` being set, for a mind that needs telling."""


# ── The harness ─────────────────────────────────────────────────────────────────────────


class Harness:
    """Attach, poll, hand each turn to the handler, and close every turn exactly once."""

    def __init__(self, id: str, name: str, handler: Handler, detail: Optional[str] = None,
                 tools: bool = False, memory: bool = False, address: Optional[str] = None,
                 log: Optional[Callable[[str], None]] = None,
                 heartbeat_seconds: float = HEARTBEAT_SECONDS,
                 poll_interval: float = POLL_INTERVAL,
                 retry_seconds: float = RETRY_SECONDS,
                 busy_reply: str = BUSY_REPLY) -> None:
        self.id = id
        self.name = name
        self.handler = handler
        self.detail = detail
        self.tools = tools
        self.memory = memory
        # None means "find it each time we attach", so a harness started before the desktop
        # picks it up when it appears.
        self.address = address
        self.log = log or (lambda message: print("[%s] %s" % (id, message), file=sys.stderr))
        self.heartbeat_seconds = heartbeat_seconds
        self.poll_interval = poll_interval
        self.retry_seconds = retry_seconds
        self.busy_reply = busy_reply

        self.session: Optional[str] = None
        self._resolved: Optional[str] = address
        self._open: Dict[int, Turn] = {}
        self._lock = threading.Lock()
        self._stopping = threading.Event()
        self._workers: List[threading.Thread] = []
        self._beat: Optional[threading.Thread] = None
        self._complained_about_socket = False

    # ── running ─────────────────────────────────────────────────────────

    def run(self) -> None:
        """Poll forever. Returns when `stop()` is called."""
        self._beat = threading.Thread(target=self._heartbeat, name="harness-heartbeat", daemon=True)
        self._beat.start()
        try:
            while not self._stopping.is_set():
                if self.session is None:
                    if not self._attach():
                        self._stopping.wait(self.retry_seconds)
                    continue
                try:
                    reply = self._call(POLL, {"session": self.session}) or {}
                except HarnessError as exc:
                    # The shell restarted, or this session aged out. Attaching again is the whole
                    # recovery — and the new session id MUST replace the old one, or the loop
                    # recovers forever and never succeeds.
                    self.log("poll failed (%s); re-attaching" % exc)
                    self.session = None
                    self._stopping.wait(min(self.retry_seconds, 2.0))
                    continue
                turn_id = reply.get("turn_id")
                if not isinstance(turn_id, int):
                    self._stopping.wait(self.poll_interval)
                    continue
                self._dispatch(Turn(self, self.session, turn_id,
                                    str(reply.get("text") or ""), reply.get("context")))
        finally:
            self._shutdown()

    def stop(self) -> None:
        self._stopping.set()

    def _shutdown(self) -> None:
        for turn in list(self._open.values()):
            turn.cancelled.set()
            self._close(turn, error="the harness is shutting down")
        if self.session:
            try:
                self._call(DETACH, {"session": self.session})
            except HarnessError:
                pass
            self.session = None

    def _attach(self) -> bool:
        address = self.address or socket_path()
        if not address:
            if not self._complained_about_socket:
                self.log("no desktop harness socket yet; waiting for one")
                self._complained_about_socket = True
            return False
        params: Dict[str, Any] = {"id": self.id, "name": self.name,
                                  "tools": self.tools, "memory": self.memory}
        if self.detail:
            params["detail"] = self.detail
        try:
            reply = call(address, ATTACH, params) or {}
        except HarnessError as exc:
            self.log("could not attach: %s" % exc)
            return False
        session = reply.get("session")
        if not session:
            self.log("the desktop attached us without a session id, which cannot be used")
            return False
        self.session = str(session)
        self._resolved = address
        self._complained_about_socket = False
        self.log("attached as `%s` (session %s)" % (self.id, self.session))
        return True

    def _call(self, method: str, params: Dict[str, Any]) -> Any:
        address = self.address or getattr(self, "_resolved", None) or socket_path()
        if not address:
            raise HarnessError("%s: the desktop's harness socket is gone" % method)
        return call(address, method, params)

    # ── turns ───────────────────────────────────────────────────────────

    def _dispatch(self, turn: Turn) -> None:
        command = turn.text.strip().split(None, 1)[0].lower() if turn.text.strip() else ""
        if command in ("/stop", "/new"):
            self._command(turn, command)
            return

        with self._lock:
            busy = bool(self._open) and not getattr(self.handler, "concurrent", False)
            if not busy:
                self._open[turn.turn_id] = turn
        if busy:
            # Answered immediately and closed here: this turn is owed an answer exactly like the
            # one being worked on, and the worst thing to do with it is nothing.
            turn.emit(self.busy_reply)
            self._close(turn)
            return

        worker = threading.Thread(target=self._work, args=(turn,),
                                  name="turn-%d" % turn.turn_id, daemon=True)
        with self._lock:
            self._workers = [t for t in self._workers if t.is_alive()]
            self._workers.append(worker)
        worker.start()

    def _command(self, turn: Turn, command: str) -> None:
        """/stop and /new are answered here, never handed to the mind."""
        running = list(self._open.values())
        for other in running:
            other.cancelled.set()
            try:
                self.handler.cancel(other)
            except Exception as exc:  # a mind that cannot be stopped must not break /stop
                self.log("cancel raised: %s" % exc)
        if command == "/stop":
            turn.emit("stopping." if running else "nothing was running.")
        else:
            try:
                self.handler.reset()
            except Exception as exc:
                turn.emit("could not start a new conversation: %s" % _readable(exc))
                self._close(turn)
                return
            turn.emit("new conversation — what came before is forgotten." if not running
                      else "stopped, and started a new conversation.")
        self._close(turn)

    def _work(self, turn: Turn) -> None:
        try:
            self.handler.answer(turn)
        except Exception as exc:
            # The mind failed. The person gets a sentence instead of an answer, and the turn is
            # closed — a failure that leaves a turn open is worse than the failure.
            self.log("turn %d failed: %r" % (turn.turn_id, exc))
            if turn.said_anything:
                turn.emit("\n\n" + _readable(exc))
                self._close(turn)
            else:
                self._close(turn, error=_readable(exc))
            return
        if not turn.said_anything:
            # A handler that returns without saying anything leaves the panel with an empty
            # bubble and no way to tell it from a hang. Say something.
            turn.emit("(stopped)" if turn.cancelled.is_set() else "(no answer)")
        self._close(turn)

    def _chunk(self, turn: Turn, delta: str) -> bool:
        if turn.closed or self._stopping.is_set():
            return False
        try:
            reply = self._call(CHUNK, {"session": turn.session, "turn_id": turn.turn_id,
                                       "delta": delta}) or {}
        except HarnessError as exc:
            self.log("chunk on turn %d failed: %s" % (turn.turn_id, exc))
            turn.dropped = True
            return False
        turn.last_call = time.monotonic()
        if reply.get("dropped"):
            turn.dropped = True
            return False
        return True

    def _close(self, turn: Turn, error: Optional[str] = None) -> None:
        """Complete or fail, once. Every path out of a turn comes through here."""
        with self._lock:
            if turn.closed:
                return
            turn.closed = True
            self._open.pop(turn.turn_id, None)
        try:
            if error is None:
                self._call(COMPLETE, {"session": turn.session, "turn_id": turn.turn_id})
            else:
                self._call(FAIL, {"session": turn.session, "turn_id": turn.turn_id, "error": error})
        except HarnessError as exc:
            # The desktop has already given up on this turn (it restarted, or we re-attached and
            # it failed what the old session owed). Nothing left to close.
            self.log("could not close turn %d: %s" % (turn.turn_id, exc))

    def _heartbeat(self) -> None:
        """An empty chunk on every open turn, well inside the desktop's 90-second window.

        A long turn can go minutes between anything worth showing — a model thinking, an `os_act`
        waiting on an approval card — and without this the desktop reaps the harness mid-answer
        and the person is told it stopped responding while it is working.
        """
        while not self._stopping.is_set():
            self._stopping.wait(max(0.05, min(self.heartbeat_seconds / 2.0, 2.0)))
            if self._stopping.is_set():
                return
            now = time.monotonic()
            for turn in list(self._open.values()):
                if turn.closed or now - turn.last_call < self.heartbeat_seconds:
                    continue
                turn.last_call = now
                try:
                    self._call(CHUNK, {"session": turn.session, "turn_id": turn.turn_id, "delta": ""})
                except HarnessError:
                    pass


def _readable(exc: BaseException) -> str:
    """A failure as a sentence a person can act on, never a traceback."""
    detail = str(exc).strip() or exc.__class__.__name__
    if not detail.endswith((".", "!", "?")):
        detail += "."
    return detail


# ── The desktop's tools, over MCP ───────────────────────────────────────────────────────

# How long one tool call may take. An `os_act` above the session's ceiling asks the person and
# waits for them inside the bridge — up to OS_ACT_MAX_SECONDS, which is a little over 270s (see
# deploy/yantrik-os/yos-mcp). A client that gives up sooner cuts the person off mid-decision and
# reports a timeout for a machine that was working correctly.
MCP_TIMEOUT = 300.0
MCP_COMMAND = os.environ.get("YOS_MCP_BIN", "/opt/yantrik/bin/yos-mcp")
MCP_PROTOCOL_VERSION = "2024-11-05"


class McpTools:
    """The desktop's own tools, as an MCP client over stdio.

    One child process, one reader thread, thread-safe, and restarted if it dies — a harness that
    loses the bridge should lose one tool call, not the desktop.

    `clientInfo` is the harness's own name and version because that is what the approval card
    shows the person: the bridge keeps it for the card's `says the caller` line, and a card that
    reads "an unnamed caller is asking to use this machine" is one the person cannot answer.
    """

    def __init__(self, client_name: str, client_version: str = "1.0.0",
                 command: Optional[Union[str, Sequence[str]]] = None,
                 env: Optional[Dict[str, str]] = None,
                 timeout: float = MCP_TIMEOUT,
                 log: Optional[Callable[[str], None]] = None) -> None:
        self.client_name = client_name
        self.client_version = client_version
        self.command: Sequence[str] = ([command] if isinstance(command, str)
                                       else list(command) if command else [MCP_COMMAND])
        self.env = env
        self.timeout = timeout
        self.log = log or (lambda message: print("[mcp] %s" % message, file=sys.stderr))

        self._proc: Optional[subprocess.Popen] = None
        self._proc_lock = threading.RLock()
        self._write_lock = threading.Lock()
        self._pending: Dict[int, Dict[str, Any]] = {}
        self._pending_lock = threading.Lock()
        self._next_id = 0
        self._tools: Optional[List[Dict[str, Any]]] = None

    # ── public ──────────────────────────────────────────────────────────

    def list(self, refresh: bool = False) -> List[Dict[str, Any]]:
        """Every tool the desktop publishes, with its real description and schema."""
        if self._tools is not None and not refresh:
            return self._tools
        self._ensure()
        result = self._rpc("tools/list", {}, timeout=30.0)
        tools = [t for t in (result.get("tools") or []) if isinstance(t, dict) and t.get("name")]
        self._tools = tools
        return tools

    def call(self, name: str, arguments: Optional[Dict[str, Any]] = None,
             timeout: Optional[float] = None) -> Tuple[str, bool]:
        """Run one tool. Returns (text, is_error) — never raises.

        `is_error` is the bridge's own flag and means "this did not run". A policy answer — the
        person said no, the mode forbids it, the session is tainted — comes back unflagged and
        says REFUSED in its first word, because it is an answer and not a fault. A mind must not
        retry it or route around it, and a harness must not turn it into an error either.
        """
        try:
            self._ensure()
            result = self._rpc("tools/call", {"name": name, "arguments": arguments or {}},
                               timeout=self.timeout if timeout is None else timeout)
        except McpError as exc:
            return (str(exc), True)
        parts = result.get("content") or []
        text = "".join(str(p.get("text") or "") for p in parts
                       if isinstance(p, dict) and p.get("type") == "text")
        return (text or "(the tool returned nothing)", bool(result.get("isError")))

    def as_openai_tools(self) -> List[Dict[str, Any]]:
        """The same tools in the shape an OpenAI-compatible chat API wants."""
        out = []
        for tool in self.list():
            out.append({
                "type": "function",
                "function": {
                    "name": tool["name"],
                    "description": str(tool.get("description") or "")[:4000],
                    "parameters": tool.get("inputSchema") or {"type": "object", "properties": {}},
                },
            })
        return out

    def close(self) -> None:
        with self._proc_lock:
            proc, self._proc = self._proc, None
        end_process(proc)

    # ── plumbing ────────────────────────────────────────────────────────

    def _ensure(self) -> None:
        with self._proc_lock:
            if self._proc is not None and self._proc.poll() is None:
                return
            if self._proc is not None:
                self.log("the desktop bridge exited (%s); restarting it" % self._proc.poll())
                end_process(self._proc)
            self._start()

    def _start(self) -> None:
        env = dict(os.environ)
        if self.env:
            env.update(self.env)
        try:
            self._proc = subprocess.Popen(
                list(self.command), stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL, text=True, bufsize=1, env=env,
            )
        except OSError as exc:
            self._proc = None
            raise McpError("the desktop bridge could not be started (%s): %s"
                           % (" ".join(self.command), exc)) from exc
        threading.Thread(target=self._reader, args=(self._proc,),
                         name="mcp-reader", daemon=True).start()
        self._rpc("initialize", {
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {},
            # Self-declared, and the bridge says so on the card. It is the name the person
            # recognises, which is the whole reason to send it.
            "clientInfo": {"name": self.client_name, "version": self.client_version},
        }, timeout=30.0)
        self._notify("notifications/initialized", {})

    def _reader(self, proc: subprocess.Popen) -> None:
        try:
            for line in proc.stdout:  # type: ignore[union-attr]
                line = line.strip()
                if not line:
                    continue
                try:
                    msg = json.loads(line)
                except ValueError:
                    continue  # a stray line on stdout is not a reason to lose the bridge
                if not isinstance(msg, dict) or msg.get("id") is None:
                    continue
                with self._pending_lock:
                    slot = self._pending.get(msg["id"])
                if slot is None:
                    continue
                slot["message"] = msg
                slot["event"].set()
        except Exception:
            pass
        finally:
            try:
                if proc.stdout:
                    proc.stdout.close()
            except Exception:
                pass
            self._wake_all("the desktop bridge stopped before it answered")

    def _wake_all(self, why: str) -> None:
        with self._pending_lock:
            slots = list(self._pending.values())
        for slot in slots:
            if "message" not in slot:
                slot["error"] = why
            slot["event"].set()

    def _notify(self, method: str, params: Dict[str, Any]) -> None:
        self._write({"jsonrpc": "2.0", "method": method, "params": params})

    def _rpc(self, method: str, params: Dict[str, Any], timeout: float) -> Dict[str, Any]:
        with self._pending_lock:
            self._next_id += 1
            msg_id = self._next_id
            slot: Dict[str, Any] = {"event": threading.Event()}
            self._pending[msg_id] = slot
        try:
            self._write({"jsonrpc": "2.0", "id": msg_id, "method": method, "params": params})
            if not slot["event"].wait(timeout):
                raise McpError(
                    "%s did not answer within %ds. The desktop may be busy rather than broken; "
                    "nothing was rolled back." % (method, int(timeout)))
            if slot.get("error"):
                raise McpError(str(slot["error"]))
            msg = slot["message"]
        finally:
            with self._pending_lock:
                self._pending.pop(msg_id, None)
        if msg.get("error"):
            err = msg["error"]
            raise McpError(err.get("message", str(err)) if isinstance(err, dict) else str(err))
        result = msg.get("result")
        return result if isinstance(result, dict) else {}

    def _write(self, msg: Dict[str, Any]) -> None:
        with self._write_lock:
            proc = self._proc
            if proc is None or proc.stdin is None or proc.poll() is not None:
                raise McpError("the desktop bridge is not running")
            try:
                proc.stdin.write(json.dumps(msg) + "\n")
                proc.stdin.flush()
            except (OSError, ValueError) as exc:
                raise McpError("could not reach the desktop bridge: %s" % exc) from exc


class McpError(Exception):
    """The bridge could not be reached, or did not answer. Not a refusal — see `McpTools.call`."""


def end_process(proc: Optional[subprocess.Popen]) -> None:
    """End a child and close its pipes.

    Shared because a harness restarts its child and a leaked pipe per restart is a file
    descriptor leak in a process that is meant to run for weeks.
    """
    if proc is None:
        return
    for stream in (proc.stdin, proc.stdout, proc.stderr):
        try:
            if stream:
                stream.close()
        except Exception:
            pass
    try:
        if proc.poll() is None:
            proc.terminate()
        proc.wait(timeout=5)
    except Exception:
        try:
            proc.kill()
        except Exception:
            pass
