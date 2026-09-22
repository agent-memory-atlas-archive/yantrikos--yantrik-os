"""An OpenClaw that is not OpenClaw: the gateway's wire, and the CLI's stdout.

Two fakes in one file because they are two ends of the same harness.

`FakeGateway` is a WebSocket server in a thread. It writes its own RFC 6455 framing rather than
importing the harness's — the point is to check the client against something that is not itself,
so the client's masking is actually unmasked by someone else and the server's unmasked frames are
actually read by the client.

Run as a script it is the CLI instead — `python3 fake_openclaw.py <scenario> …` stands in for
`openclaw agent …`, which is exactly how the harness launches it, so the harness's own argv
building is exercised unchanged.

Gateway scenarios (constructor):

    text        two assistant deltas, then done
    tool        a tool event around the text
    abort       says "working" and then nothing until an abort arrives
    drop        takes the message and kills the TCP connection without a close frame
    silent      accepts everything and never says anything at all
    fragments   one JSON event split across three frames, with a ping before it
    snapshot    resends the whole answer each time instead of sending deltas

CLI scenarios (argv[1]):

    text        JSON-lines events and exit 0
    tool        a tool event, some text, exit 0
    plain       no JSON at all — plain text on stdout, exit 0
    silent      says nothing and waits to be killed
    working     says one delta and then waits to be killed
    fail        exits 2 having said nothing, complaining about a flag
    down        exits 1 the way a CLI does when its daemon is not running
"""

from __future__ import annotations

import base64
import hashlib
import json
import os
import socket
import struct
import sys
import threading
import time
from typing import Any, Dict, List, Optional, Tuple

_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"


def free_port() -> int:
    """A port nothing is listening on, so a test can be refused on it and then serve on it."""
    sock = socket.socket()
    try:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])
    finally:
        sock.close()


# ── The gateway ─────────────────────────────────────────────────────────────────────────


class FakeGateway:
    """A WebSocket server speaking the envelope the harness assumes."""

    def __init__(self, scenario: str = "text", port: int = 0,
                 accept_path: Optional[str] = None) -> None:
        self.scenario = scenario
        # None means "any path upgrades". A path here makes every other one a 404, which is how
        # the client's path probing gets exercised.
        self.accept_path = accept_path
        self.lock = threading.Lock()
        self.messages: List[Dict[str, Any]] = []
        self.headers: Dict[str, str] = {}
        self.paths: List[str] = []
        # Counted so a test can prove the client answered a ping rather than ignoring it.
        self.pongs = 0
        self._stop = threading.Event()
        self._conns: List[socket.socket] = []

        self.server = socket.socket()
        self.server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.server.bind(("127.0.0.1", port))
        self.server.listen(8)
        self.server.settimeout(0.2)
        self.port = int(self.server.getsockname()[1])
        self.thread = threading.Thread(target=self._serve, name="fake-gateway", daemon=True)
        self.thread.start()

    @property
    def url(self) -> str:
        """No path, so the harness's candidate list is what finds the way in."""
        return "ws://127.0.0.1:%d" % self.port

    def sent(self, kind: str) -> List[Dict[str, Any]]:
        with self.lock:
            return [m for m in self.messages if m.get("type") == kind]

    def stop(self) -> None:
        self._stop.set()
        try:
            self.server.close()
        except OSError:
            pass
        with self.lock:
            conns, self._conns = list(self._conns), []
        for conn in conns:
            try:
                conn.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
            try:
                conn.close()
            except OSError:
                pass
        self.thread.join(timeout=2)

    # ── plumbing ────────────────────────────────────────────────────────

    def _serve(self) -> None:
        while not self._stop.is_set():
            try:
                conn, _ = self.server.accept()
            except (socket.timeout, OSError):
                continue
            with self.lock:
                self._conns.append(conn)
            threading.Thread(target=self._one, args=(conn,), daemon=True).start()

    def _one(self, conn: socket.socket) -> None:
        try:
            if not self._handshake(conn):
                return
            while not self._stop.is_set():
                frame = _read_frame(conn)
                if frame is None:
                    return
                fin, opcode, data = frame
                if opcode == 0x8:
                    return
                if opcode == 0x9:
                    _send_frame(conn, 0xA, data)
                    continue
                if opcode == 0xA:
                    with self.lock:
                        self.pongs += 1
                    continue
                if opcode not in (0x1, 0x0):
                    continue
                try:
                    message = json.loads(data.decode("utf-8"))
                except ValueError:
                    continue
                with self.lock:
                    self.messages.append(message)
                threading.Thread(target=self._answer, args=(conn, message),
                                 daemon=True).start()
        except Exception:
            pass
        finally:
            try:
                conn.close()
            except OSError:
                pass

    def _handshake(self, conn: socket.socket) -> bool:
        buf = b""
        conn.settimeout(5)
        while b"\r\n\r\n" not in buf:
            piece = conn.recv(4096)
            if not piece:
                return False
            buf += piece
        head = buf.split(b"\r\n\r\n", 1)[0].decode("latin-1")
        lines = head.split("\r\n")
        path = lines[0].split(" ")[1] if len(lines[0].split(" ")) > 1 else "/"
        headers = {}
        for line in lines[1:]:
            name, _, value = line.partition(":")
            if name:
                headers[name.strip().lower()] = value.strip()
        with self.lock:
            self.paths.append(path)
            self.headers = headers
        if self.accept_path is not None and path != self.accept_path:
            conn.sendall(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
            return False
        key = headers.get("sec-websocket-key", "")
        accept = base64.b64encode(
            hashlib.sha1((key + _GUID).encode("ascii")).digest()).decode("ascii")
        conn.sendall(("HTTP/1.1 101 Switching Protocols\r\n"
                      "Upgrade: websocket\r\nConnection: Upgrade\r\n"
                      "Sec-WebSocket-Accept: %s\r\n\r\n" % accept).encode("ascii"))
        conn.settimeout(None)
        return True

    # ── what it answers ─────────────────────────────────────────────────

    def _answer(self, conn: socket.socket, message: Dict[str, Any]) -> None:
        kind = message.get("type")
        if kind == "abort":
            if self.scenario == "abort":
                _send_json(conn, {"type": "assistant", "delta": " — stopped."})
                _send_json(conn, {"type": "done"})
            return
        if kind == "session.new":
            _send_json(conn, {"type": "ack"})
            return
        if kind != "message":
            return

        if self.scenario == "silent":
            return

        if self.scenario == "drop":
            # No close frame, no warning: the daemon died or was restarted mid-answer.
            try:
                conn.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
            try:
                conn.close()
            except OSError:
                pass
            return

        if self.scenario == "abort":
            _send_json(conn, {"type": "assistant", "delta": "working"})
            return

        if self.scenario == "tool":
            _send_json(conn, {"type": "thinking", "delta": "which calendar"})
            _send_json(conn, {"type": "tool_use", "name": "os_act",
                              "input": {"app": "calendar", "action": "add_event",
                                        "args": {"title": "dentist"}}})
            _send_json(conn, {"type": "assistant", "delta": "Added it."})
            _send_json(conn, {"type": "done"})
            return

        if self.scenario == "fragments":
            _send_frame(conn, 0x9, b"are you there")          # a ping the client must pong
            payload = json.dumps({"type": "assistant",
                                  "delta": "Two windows."}).encode("utf-8")
            cut = len(payload) // 3
            _send_frame(conn, 0x1, payload[:cut], fin=False)
            _send_frame(conn, 0x0, payload[cut:2 * cut], fin=False)
            _send_frame(conn, 0x0, payload[2 * cut:], fin=True)
            _send_json(conn, {"type": "done"})
            return

        if self.scenario == "snapshot":
            # No `delta` anywhere: each event is the whole answer so far.
            _send_json(conn, {"type": "message", "text": "Two "})
            _send_json(conn, {"type": "message", "text": "Two windows."})
            _send_json(conn, {"type": "done"})
            return

        _send_json(conn, {"type": "assistant", "delta": "Two "})
        _send_json(conn, {"type": "assistant", "delta": "windows."})
        _send_json(conn, {"type": "done"})


def _recv_exact(conn: socket.socket, count: int) -> Optional[bytes]:
    buf = b""
    while len(buf) < count:
        try:
            piece = conn.recv(count - len(buf))
        except OSError:
            return None
        if not piece:
            return None
        buf += piece
    return buf


def _read_frame(conn: socket.socket) -> Optional[Tuple[bool, int, bytes]]:
    head = _recv_exact(conn, 2)
    if head is None:
        return None
    fin = bool(head[0] & 0x80)
    opcode = head[0] & 0x0F
    masked = bool(head[1] & 0x80)
    length = head[1] & 0x7F
    if length == 126:
        extra = _recv_exact(conn, 2)
        if extra is None:
            return None
        length = struct.unpack("!H", extra)[0]
    elif length == 127:
        extra = _recv_exact(conn, 8)
        if extra is None:
            return None
        length = struct.unpack("!Q", extra)[0]
    key = b""
    if masked:
        key = _recv_exact(conn, 4) or b""
        if len(key) != 4:
            return None
    data = _recv_exact(conn, length) if length else b""
    if data is None:
        return None
    if masked and data:
        # A client that did not mask would come out as mojibake here, which is the point.
        data = bytes(byte ^ key[i % 4] for i, byte in enumerate(data))
    return fin, opcode, data


def _send_frame(conn: socket.socket, opcode: int, payload: bytes, fin: bool = True) -> None:
    head = bytearray([(0x80 if fin else 0x00) | opcode])
    length = len(payload)
    if length < 126:
        head.append(length)
    elif length < 65536:
        head.append(126)
        head += struct.pack("!H", length)
    else:
        head.append(127)
        head += struct.pack("!Q", length)
    try:
        conn.sendall(bytes(head) + payload)       # server frames are never masked
    except OSError:
        pass


def _send_json(conn: socket.socket, event: Dict[str, Any]) -> None:
    _send_frame(conn, 0x1, json.dumps(event).encode("utf-8"))


# ── The CLI ─────────────────────────────────────────────────────────────────────────────


def _cli() -> int:
    scenario = os.environ.get("FAKE_OPENCLAW_SCENARIO") or (sys.argv[1] if len(sys.argv) > 1
                                                            else "text")
    dump = os.environ.get("FAKE_OPENCLAW_ARGV_DUMP")
    if dump:
        with open(dump, "a", encoding="utf-8") as handle:
            handle.write(json.dumps(sys.argv[1:]) + "\n")

    def say(event: Dict[str, Any]) -> None:
        sys.stdout.write(json.dumps(event) + "\n")
        sys.stdout.flush()

    if scenario == "down":
        sys.stderr.write("Error: connect ECONNREFUSED 127.0.0.1:18789\n")
        return 1
    if scenario == "fail":
        sys.stderr.write("error: unknown option '--json'\n")
        return 2
    if scenario == "plain":
        for line in ("Two windows.", "Notes and Files."):
            sys.stdout.write(line + "\n")
            sys.stdout.flush()
        return 0
    if scenario == "silent":
        sys.stdout.flush()
        while True:
            time.sleep(0.2)
    if scenario == "working":
        say({"type": "assistant", "delta": "working"})
        while True:
            time.sleep(0.2)
    if scenario == "tool":
        say({"type": "tool_call", "name": "os_act",
             "arguments": {"app": "calendar", "action": "add_event",
                           "args": {"title": "dentist"}}})
        say({"type": "assistant", "delta": "Added it."})
        say({"type": "done"})
        return 0

    say({"type": "assistant", "delta": "Two "})
    say({"type": "assistant", "delta": "windows."})
    say({"type": "done"})
    return 0


if __name__ == "__main__":
    if "--version" in sys.argv:
        print("openclaw 2026.6.10 (fake)")
        sys.exit(0)
    sys.exit(_cli())
