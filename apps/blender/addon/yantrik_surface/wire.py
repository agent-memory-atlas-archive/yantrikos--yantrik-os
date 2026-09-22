"""The wire: newline-delimited JSON-RPC 2.0 over a unix socket.

A Python port of `yantrik-ipc-transport`'s server and the envelope half of
`yantrik-ipc-contracts::control_surface`. It has to agree with the Rust original on four
things a caller can observe, and this file is where all four live:

  * the socket directory — the same candidate chain as `server::socket_dir()`, in the same
    order, with the same tightening (directory 0700, socket node 0600), so `yos` and the
    conformance suite find this app exactly where they find every other;
  * the framing — one JSON object per line, request and reply alike, `rpc.ping` and
    `rpc.service_id` answered like the transport answers them;
  * the error codes — -32700 parse, -32601 unknown method, -32602 the app refusing, -32000
    the app failing to answer;
  * the revision — FNV-1a over the summary, a zero byte, and the state rendered compact
    with sorted keys, which is what `View::revision()` hashes on the Rust side.

No `bpy` in here. The handler this file serves is `surface.Surface`, which knows the
vocabulary; this file only knows the wire.
"""

import errno
import json
import os
import socket
import socketserver
import stat
import threading

# JSON-RPC error codes, the transport's own constants.
RPC_PARSE_ERROR = -32700
RPC_METHOD_NOT_FOUND = -32601
RPC_INVALID_PARAMS = -32602
RPC_TRANSPORT_ERROR = -32000

# FNV-1a, 64-bit. Written out rather than taken from a hash library for the same reason the
# Rust side writes it out: this value crosses a socket and turns up in logs, so it has to
# mean the same thing on both sides and in tomorrow's build.
FNV_OFFSET = 0xCBF29CE484222325
FNV_PRIME = 0x00000100000001B3
_MASK64 = (1 << 64) - 1


def canonical_state(state):
    """The state as the revision hashes it: compact, keys sorted, UTF-8 kept raw.

    `serde_json`'s `to_string` renders object keys in sorted order and does not escape
    non-ASCII; `sort_keys` + `ensure_ascii=False` is the same rendering, and the separators
    are its exact punctuation.
    """
    return json.dumps(state, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def revision(summary, state):
    """A short fingerprint of everything a view reports. `View::revision()` in Python."""
    h = FNV_OFFSET
    for byte in summary.encode("utf-8") + b"\x00" + canonical_state(state).encode("utf-8"):
        h ^= byte
        h = (h * FNV_PRIME) & _MASK64
    return "%016x" % h


def socket_dir():
    """The session's socket directory, by the same chain the Rust runtime walks.

    `$XDG_RUNTIME_DIR/yantrik` → `/run/user/<uid>/yantrik` → `/run/yantrik` →
    `/tmp/yantrik-<uid>`. The first candidate that exists or can be created, tightened to
    0700 on the way out.

    The `/run/user/<uid>` step is the one addition to the runtime's chain, and it is here
    because of the client: `yos` defaults to `/run/user/<uid>/yantrik` when
    `XDG_RUNTIME_DIR` is unset, and a server that fell straight through to `/tmp` would be
    a server `yos` cannot find on a machine that never set the variable. On a normal
    session both sides read the same variable and the extra step never runs.
    """
    candidates = []
    xdg = os.environ.get("XDG_RUNTIME_DIR", "").strip()
    if xdg:
        candidates.append(os.path.join(xdg, "yantrik"))
    candidates.append("/run/user/%d/yantrik" % os.getuid())
    candidates.append("/run/yantrik")
    last_resort = "/tmp/yantrik-%d" % os.getuid()
    candidates.append(last_resort)

    for directory in candidates:
        try:
            if not os.path.isdir(directory):
                os.makedirs(directory)
        except OSError:
            continue
        try:
            _harden(directory)
        except OSError:
            continue
        return directory
    return last_resort


def _harden(directory):
    """Restrict a socket directory to its owner — `server::harden`, including its no-op.

    The early return is what makes asking twice safe: on an already-private directory this
    must not re-issue a chmod, because a second chmod is exactly what a sandbox or a
    read-only mount denies, and the Rust side learned that the hard way (the comment on
    `harden()` in server.rs is the account).
    """
    mode = stat.S_IMODE(os.stat(directory).st_mode)
    if mode & 0o777 == 0o700:
        return
    os.chmod(directory, 0o700)


def default_socket_path(app_id):
    """Where `app-<id>.sock` goes, mirroring `RpcServer::default_address`."""
    return os.path.join(socket_dir(), "app-%s.sock" % app_id)


class RpcError(Exception):
    """A refusal that travels as a JSON-RPC error object rather than a result."""

    def __init__(self, code, message):
        super().__init__(message)
        self.code = code
        self.message = message


class Handler:
    """What the server asks when a request arrives. `surface.Surface` implements it."""

    service_id = "app"

    def handle(self, method, params):
        raise NotImplementedError


class Server:
    """The unix-socket server: a thread of its own, one thread per connection.

    Line-delimited on both sides, like the transport: a connection is read a line at a
    time, each line is one request, each reply is one line. A stale socket file from a
    crashed run is unlinked before binding, and the node comes out 0600 — a socket that
    drives a scene is not something every local user gets to open, and the directory's
    0700 is the belt to this brace, not the other way round.
    """

    def __init__(self, path, handler):
        self.path = path
        self.handler = handler
        self._server = None
        self._thread = None

    def start(self):
        directory = os.path.dirname(self.path)
        if directory and not os.path.isdir(directory):
            os.makedirs(directory)
        try:
            if os.path.exists(self.path) or os.path.islink(self.path):
                os.unlink(self.path)
        except OSError as e:
            if e.errno != errno.ENOENT:
                raise
        self._server = socketserver.ThreadingUnixStreamServer(self.path, _Connection)
        # Connection threads must not outlive the process: a caller that hangs up mid-line
        # would otherwise hold the interpreter open in threading._shutdown, and a background
        # Blender that has been told to stop is a Blender that stops.
        self._server.daemon_threads = True
        self._server.block_on_close = False
        self._server.handler = self.handler  # read by _Connection.handle
        os.chmod(self.path, 0o600)
        self._thread = threading.Thread(
            target=self._server.serve_forever,
            name="%s-rpc" % self.handler.service_id,
            daemon=True,
        )
        self._thread.start()

    def stop(self):
        if self._server is not None:
            self._server.shutdown()
            self._server.server_close()
            self._server = None
        try:
            os.unlink(self.path)
        except OSError:
            pass


class _Connection(socketserver.StreamRequestHandler):
    """One caller. Reads lines until the caller goes away."""

    def handle(self):
        server = self.server
        handler = getattr(server, "handler", None)
        for raw in self.rfile:
            line = raw.decode("utf-8", errors="replace").strip()
            if not line:
                continue
            reply = _answer(handler, line)
            try:
                self.wfile.write((json.dumps(reply, ensure_ascii=False) + "\n").encode("utf-8"))
                self.wfile.flush()
            except OSError:
                return


def _answer(handler, line):
    """One request line, one response object. Framing only; no policy lives here."""
    request_id = None
    try:
        request = json.loads(line)
        request_id = request.get("id")
        method = request.get("method")
        params = request.get("params")
        if params is None:
            params = {}
    except ValueError as e:
        return _error(None, RPC_PARSE_ERROR, "Parse error: %s" % e)

    if not isinstance(method, str):
        return _error(request_id, RPC_INVALID_PARAMS, "a request needs a method")

    if method == "rpc.ping":
        return _result(request_id, "pong")
    if method == "rpc.service_id":
        return _result(request_id, getattr(handler, "service_id", "app"))

    try:
        return _result(request_id, handler.handle(method, params))
    except RpcError as e:
        return _error(request_id, e.code, e.message)
    except Exception as e:  # noqa: BLE001 - an unhandled fault is a transport answer, not a crash
        return _error(request_id, RPC_TRANSPORT_ERROR,
                      "the app failed while answering %s: %s: %s"
                      % (method, type(e).__name__, e))


def _result(request_id, value):
    return {"jsonrpc": "2.0", "id": request_id, "result": value}


def _error(request_id, code, message):
    return {"jsonrpc": "2.0", "id": request_id, "error": {"code": code, "message": message}}


def call_once(path, method, params, timeout=10.0):
    """One request over a socket, the way `yos` sends one. Used by the tests."""
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    client.settimeout(timeout)
    client.connect(path)
    try:
        payload = json.dumps(
            {"jsonrpc": "2.0", "id": 1, "method": method, "params": params}) + "\n"
        client.sendall(payload.encode("utf-8"))
        buf = b""
        while not buf.endswith(b"\n"):
            chunk = client.recv(65536)
            if not chunk:
                break
            buf += chunk
        return json.loads(buf.decode("utf-8"))
    finally:
        client.close()
