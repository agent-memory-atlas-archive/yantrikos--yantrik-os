"""The wire: newline-delimited JSON-RPC 2.0 over a unix socket, and the revision hash.

A port of `yantrik-ipc-transport`'s server and of the envelope half of
`yantrik-ipc-contracts::control_surface`. A caller has to be unable to tell a surface built on
this package from one built on the Rust runtime, and these are the things it can observe:

  * the socket directory — the chain `server::socket_dir()` walks, in its order
    (`$XDG_RUNTIME_DIR/yantrik` → `/run/yantrik` → `/tmp/yantrik-<uid>`), tightened the same
    way (directory 0700, socket node 0600), so `yos` and every other client find a surface
    here exactly where they find the rest;
  * the framing — one JSON object per line, request and reply alike, several requests per
    connection, `rpc.ping` and `rpc.service_id` answered like the transport answers them;
  * the error codes — -32700 parse, -32601 unknown method, -32602 the app refusing, -32000
    the app failing to answer;
  * the revision — FNV-1a-64 over the summary, a zero byte, and the state rendered the way
    `serde_json::Value::to_string` renders it, which is what `View::revision()` hashes.

Two things here go past the Rust transport on `main`, both on purpose. A request without an
`id` is answered (with `id: null`) rather than refused, because Blender's port always did and
a caller that forgot the field deserves an answer, not silence. And a name is owned: binding
refuses when a live process already answers at the path, and only a dead socket or a stale
file is replaced — the "bind if dead" rule of the surface SDK's design, which the Rust
transport is adopting beside this.
"""

import errno
import json
import math
import os
import socket
import socketserver
import stat
import struct
import sys
import threading
import traceback
from collections import namedtuple

# JSON-RPC error codes, the transport's own constants.
RPC_PARSE_ERROR = -32700
RPC_INVALID_REQUEST = -32600
RPC_METHOD_NOT_FOUND = -32601
RPC_INVALID_PARAMS = -32602
RPC_TRANSPORT_ERROR = -32000

# FNV-1a, 64-bit. Written out rather than taken from a hash library for the reason the Rust
# side writes it out: this value crosses a socket and turns up in logs, so it has to mean the
# same thing on both sides and in tomorrow's build.
FNV_OFFSET = 0xCBF29CE484222325
FNV_PRIME = 0x00000100000001B3
_MASK64 = (1 << 64) - 1

# The range serde_json holds a JSON integer in exactly (i64 below zero, u64 above). Past it a
# JSON number is read as an f64, so the canonical rendering renders it as one.
_I64_MIN = -(1 << 63)
_U64_MAX = (1 << 64) - 1

_encode_str = json.encoder.encode_basestring  # ensure_ascii=False: serde_json's escaping


# ── JSON values ──────────────────────────────────────────────────────────────


def jsonable(value, where="the value"):
    """`value` as plain JSON data: dicts with string keys, lists, str, int, float, bool, None.

    What a Rust caller can hold is a `serde_json::Value`, so that is what goes on the wire: a
    tuple becomes a list, an int or str subclass (an `IntEnum`, a `StrEnum`) its plain value,
    and a float that is not finite becomes `null` — `serde_json::json!(f64::NAN)` is `Null`,
    and `NaN` is not JSON at all. Anything else is the app's bug, said as a `TypeError` that
    names where it was found, rather than a reply the caller cannot parse.
    """
    if value is None or value is True or value is False:
        return value
    if isinstance(value, str):
        return str.__str__(value)
    if isinstance(value, int):
        return int(value)
    if isinstance(value, float):
        value = float(value)
        return value if math.isfinite(value) else None
    if isinstance(value, dict):
        return {_key(k, where): jsonable(v, where) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [jsonable(v, where) for v in value]
    raise TypeError("%s holds a %s, which is not JSON and cannot be sent over the socket"
                    % (where, type(value).__name__))


def _key(key, where):
    """A dict key as JSON has it — the conversions `json.dumps` makes, made up front."""
    if isinstance(key, str):
        return str.__str__(key)
    if key is True:
        return "true"
    if key is False:
        return "false"
    if key is None:
        return "null"
    if isinstance(key, int):
        return int.__repr__(int(key))
    if isinstance(key, float):
        return float.__repr__(float(key))
    raise TypeError("%s has a %s as a key; JSON keys are text" % (where, type(key).__name__))


def format_float(x):
    """One f64 the way serde_json 1.0 writes it.

    The shortest digits that read back as the same double (Python's `repr` finds the same
    ones), laid out as serde_json lays them out: plain decimal from 1e-5 up to 1e16, always
    with a fractional part (`100.0`); scientific outside that, with an explicit sign on the
    exponent and no padding (`1e+16`, `1.5e-7`). Python's own `repr` differs at both ends
    (`1e-05`, `2.5e-05`), which is why this exists. Checked against serde_json 1.0.149 over
    ~900 doubles; the vectors in `sdk/python/tests/test_revision.py` hold it there.
    """
    if not math.isfinite(x):
        return "null"
    if x == 0.0:
        return "-0.0" if math.copysign(1.0, x) < 0 else "0.0"
    sign = "-" if x < 0 else ""
    text = repr(abs(x))
    mantissa, _, exp = text.partition("e")
    exponent = int(exp) if exp else 0
    whole, _, fraction = mantissa.partition(".")
    digits = (whole + fraction).lstrip("0")
    exponent -= len(fraction)
    stripped = digits.rstrip("0")
    exponent += len(digits) - len(stripped)
    digits = stripped
    length = len(digits)
    kk = length + exponent  # 10^(kk-1) <= |x| < 10^kk
    if 0 <= exponent and kk <= 16:
        body = digits + "0" * exponent + ".0"
    elif 0 < kk <= 16:
        body = digits[:kk] + "." + digits[kk:]
    elif -5 < kk <= 0:
        body = "0." + "0" * (-kk) + digits
    else:
        e = kk - 1
        tail = ("e+%d" % e) if e >= 0 else ("e%d" % e)
        body = (digits if length == 1 else digits[0] + "." + digits[1:]) + tail
    return sign + body


def canonical_state(state):
    """The state as the revision hashes it: `serde_json::Value::to_string`, byte for byte.

    Compact, object keys sorted (serde_json's map is a BTreeMap; sorting Python strings by
    code point is sorting their UTF-8 bytes), non-ASCII kept raw, control characters escaped
    as `\\u00xx`, floats as `format_float` writes them.
    """
    out = []
    _render(jsonable(state, "the state"), out)
    return "".join(out)


def _render(value, out):
    if value is None:
        out.append("null")
    elif value is True:
        out.append("true")
    elif value is False:
        out.append("false")
    elif isinstance(value, str):
        out.append(_encode_str(value))
    elif isinstance(value, int):
        if _I64_MIN <= value <= _U64_MAX:
            out.append(int.__repr__(value))
        else:
            try:
                out.append(format_float(float(value)))
            except OverflowError:
                out.append("null")
    elif isinstance(value, float):
        out.append(format_float(value))
    elif isinstance(value, list):
        out.append("[")
        for i, item in enumerate(value):
            if i:
                out.append(",")
            _render(item, out)
        out.append("]")
    else:  # dict, already made plain by jsonable
        out.append("{")
        for i, key in enumerate(sorted(value)):
            if i:
                out.append(",")
            out.append(_encode_str(key))
            out.append(":")
            _render(value[key], out)
        out.append("}")


def revision(summary, state):
    """A short fingerprint of everything a view reports. `View::revision()` in Python.

    Not a counter: two revisions can only be compared for difference. That is all a caller
    needs — "has what I looked at changed since I looked" — and `expect_revision` on `app.act`
    is where the comparison is made atomically.
    """
    h = FNV_OFFSET
    data = (summary.encode("utf-8", "surrogatepass") + b"\x00"
            + canonical_state(state).encode("utf-8", "surrogatepass"))
    for byte in data:
        h ^= byte
        h = (h * FNV_PRIME) & _MASK64
    return "%016x" % h


# ── where sockets live ───────────────────────────────────────────────────────


def socket_dir():
    """The session's socket directory, by the chain `server::socket_dir()` walks.

    `$XDG_RUNTIME_DIR/yantrik` → `/run/yantrik` → `/tmp/yantrik-<uid>`: the first candidate
    that exists or can be created, and can be tightened to 0700. Like the Rust original it
    falls through to the last candidate when none can be prepared, which will then fail to
    bind and say where.
    """
    candidates = []
    xdg = os.environ.get("XDG_RUNTIME_DIR", "").strip()
    if xdg:
        candidates.append(os.path.join(xdg, "yantrik"))
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
    read-only mount denies (the comment on `harden()` in server.rs is the account).
    """
    mode = stat.S_IMODE(os.stat(directory).st_mode)
    if mode & 0o777 == 0o700:
        return
    os.chmod(directory, 0o700)


def socket_name(service_id):
    """The file a service id binds: `<id>.sock`. An app's service id is `app-<app id>`."""
    return "%s.sock" % service_id


def default_socket_path(app_id):
    """Where an app's surface binds, `app-<id>.sock`, as `RpcServer::default_address` has it."""
    return os.path.join(socket_dir(), socket_name("app-%s" % app_id))


# ── the server ───────────────────────────────────────────────────────────────


class RpcError(Exception):
    """A refusal or failure that travels as a JSON-RPC error object rather than a result."""

    def __init__(self, code, message):
        super().__init__(message)
        self.code = code
        self.message = message


class SocketBusy(OSError):
    """A live process already answers at the path: binding over it would take its name."""


PeerCred = namedtuple("PeerCred", "pid uid gid")
PeerCred.__doc__ = """Who opened a connection, as the kernel says it (`SO_PEERCRED`), not as
the caller says it. Read at accept, like the transport reads it."""


def peer_cred(sock):
    """The peer of an accepted unix socket, or None where the kernel will not say."""
    try:
        raw = sock.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, struct.calcsize("iII"))
        pid, uid, gid = struct.unpack("iII", raw)
    except (AttributeError, OSError, struct.error):
        return None
    return PeerCred(pid, uid, gid)


def answers(path, timeout=0.5):
    """Whether a live process accepts connections at `path` right now (a symlink is followed).

    A connect that succeeds is a live owner; one that is refused, finds nothing, or finds a
    file that is not a socket is a dead name. Nothing is sent.
    """
    probe = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    probe.settimeout(timeout)
    try:
        probe.connect(path)
        return True
    except OSError:
        return False
    finally:
        probe.close()


class Handler:
    """What the server asks when a request arrives. `surface.Surface` implements it.

    `handle_from` is told who is on the other end; a handler that only defines `handle`
    is called without it, the way the Rust trait's default drops the credentials.
    """

    service_id = "app"

    def handle(self, method, params):
        raise NotImplementedError


class Server:
    """The unix-socket server: a thread of its own, one thread per connection.

    Line-delimited both ways, like the transport. The name is owned: `start()` refuses with
    `SocketBusy` when a live process answers at the path, and replaces only a dead socket or a
    stale file — so a second copy of an app cannot silently take the first one's name, and a
    crashed run's leftovers never stop the next run. The node comes out 0600 (best effort, as
    the transport's `private_socket_file`); the directory's 0700 is what keeps other users out.

    `links` are other names for the same surface: symlinks beside the socket with a relative
    target, made before the bind like `link_names` makes them, never over a live socket of
    somebody else's, and taken away again on `stop()` if they still point here.
    """

    def __init__(self, path, handler, links=()):
        self.path = path
        self.handler = handler
        self.links = list(links)
        self.linked = []
        self._server = None
        self._thread = None
        self._inode = None

    def start(self):
        directory = os.path.dirname(self.path)
        if directory and not os.path.isdir(directory):
            try:
                os.makedirs(directory)
            except OSError as e:
                raise OSError(e.errno, "cannot create socket directory %s: %s (set "
                              "XDG_RUNTIME_DIR to a writable per-user path)"
                              % (directory, e.strerror)) from e
        _clear_dead(self.path)
        self.linked = [link for link in self.links if self._link(link)]
        try:
            self._server = _UnixServer(self.path, _Connection)
        except OSError as e:
            self._unlink_links()
            raise OSError(e.errno, "cannot bind %s: %s" % (self.path, e.strerror)) from e
        self._server.handler = self.handler  # read by _Connection.handle
        try:
            os.chmod(self.path, 0o600)
        except OSError as e:
            print("[yantrik] could not take the group and world bits off %s (%s); it stays at "
                  "the umask default, and the directory's 0700 still keeps other users out"
                  % (self.path, e), file=sys.stderr)
        self._inode = _inode(self.path)
        self._thread = threading.Thread(
            target=self._server.serve_forever,
            name="%s-rpc" % getattr(self.handler, "service_id", "app"),
            daemon=True,
        )
        self._thread.start()
        return self

    def _link(self, link):
        target = os.path.basename(self.path)
        try:
            existing = os.readlink(link)
        except OSError:
            existing = None
        if existing == target:
            return True
        if os.path.lexists(link):
            if answers(link):
                print("[yantrik] %s is another live surface's name; this one does not take it"
                      % link, file=sys.stderr)
                return False
            try:
                os.unlink(link)
            except OSError:
                return False
        try:
            os.symlink(target, link)
        except OSError as e:
            print("[yantrik] could not link %s (%s); callers holding that name cannot reach "
                  "this surface" % (link, e), file=sys.stderr)
            return False
        return True

    def stop(self):
        if self._server is not None:
            self._server.shutdown()
            self._server.server_close()
            self._server = None
        # Only what is still ours: a path now bound by someone else is theirs.
        if self._inode is not None and _inode(self.path) == self._inode:
            try:
                os.unlink(self.path)
            except OSError:
                pass
        self._inode = None
        self._unlink_links()

    def _unlink_links(self):
        target = os.path.basename(self.path)
        for link in self.linked:
            try:
                if os.readlink(link) == target:
                    os.unlink(link)
            except OSError:
                pass
        self.linked = []


def _inode(path):
    try:
        st = os.lstat(path)
    except OSError:
        return None
    return (st.st_dev, st.st_ino)


def _clear_dead(path):
    """Make room at `path`, unless somebody is alive there."""
    if not os.path.lexists(path):
        return
    if answers(path):
        raise SocketBusy(
            errno.EADDRINUSE,
            "%s is already answered by a running process; a surface does not bind over a live "
            "one. Stop that process, or give this surface another name" % path)
    try:
        os.unlink(path)
    except FileNotFoundError:
        pass


class _UnixServer(socketserver.ThreadingUnixStreamServer):
    # Connection threads must not outlive the process: a caller that hangs up mid-line would
    # otherwise hold the interpreter open in threading._shutdown.
    daemon_threads = True
    block_on_close = False


class _Connection(socketserver.StreamRequestHandler):
    """One caller. Reads lines until the caller goes away."""

    def handle(self):
        handler = getattr(self.server, "handler", None)
        peer = peer_cred(self.connection)
        for raw in self.rfile:
            line = raw.decode("utf-8", errors="replace").strip()
            if not line:
                continue
            reply = answer(handler, line, peer)
            try:
                self.wfile.write(encode(reply))
                self.wfile.flush()
            except OSError:
                return


def encode(reply):
    """One reply line. A result that is not JSON is the app's fault, said as -32000."""
    try:
        text = json.dumps(reply, ensure_ascii=False, allow_nan=False)
    except (TypeError, ValueError) as e:
        text = json.dumps(_error(reply.get("id"), RPC_TRANSPORT_ERROR,
                                 "the app answered with something that is not JSON: %s" % e),
                          ensure_ascii=False)
    return (text + "\n").encode("utf-8", "surrogatepass")


def answer(handler, line, peer=None):
    """One request line, one response object. Framing only; no policy lives here."""
    try:
        request = json.loads(line)
    except ValueError as e:
        return _error(None, RPC_PARSE_ERROR, "Parse error: %s" % e)
    if not isinstance(request, dict):
        return _error(None, RPC_PARSE_ERROR,
                      "Parse error: a request is a JSON object, not %s" % _kind(request))
    request_id = request.get("id")
    method = request.get("method")
    if method is None:
        return _error(request_id, RPC_PARSE_ERROR, "Parse error: missing field `method`")
    if not isinstance(method, str):
        return _error(request_id, RPC_PARSE_ERROR, "Parse error: `method` must be a string")
    params = request.get("params")
    if params is None:
        params = {}

    if method == "rpc.ping":
        return _result(request_id, "pong")
    if method == "rpc.service_id":
        return _result(request_id, getattr(handler, "service_id", "app"))

    try:
        if hasattr(handler, "handle_from"):
            value = handler.handle_from(method, params, peer)
        else:
            value = handler.handle(method, params)
        return _result(request_id, value)
    except RpcError as e:
        return _error(request_id, e.code, e.message)
    except Exception as e:  # noqa: BLE001 - an unhandled fault is an answer, not a crash
        traceback.print_exc(file=sys.stderr)
        return _error(request_id, RPC_TRANSPORT_ERROR,
                      "the app failed while answering %s: %s: %s"
                      % (method, type(e).__name__, e))


def _kind(value):
    if isinstance(value, list):
        return "an array"
    if isinstance(value, str):
        return "a string"
    if isinstance(value, bool):
        return "a boolean"
    if isinstance(value, (int, float)):
        return "a number"
    return "null"


def _result(request_id, value):
    return {"jsonrpc": "2.0", "id": request_id, "result": value}


def _error(request_id, code, message):
    return {"jsonrpc": "2.0", "id": request_id, "error": {"code": code, "message": message}}


# ── the client half, for spending grants and for tests ───────────────────────


def call_once(path, method, params, timeout=10.0, request_id=1):
    """One request over a socket, the way `yos` sends one; the whole reply object back."""
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    client.settimeout(timeout)
    try:
        client.connect(path)
        payload = json.dumps(
            {"jsonrpc": "2.0", "id": request_id, "method": method, "params": params},
            ensure_ascii=False) + "\n"
        client.sendall(payload.encode("utf-8"))
        buf = b""
        while not buf.endswith(b"\n"):
            chunk = client.recv(65536)
            if not chunk:
                break
            buf += chunk
        if not buf:
            raise ConnectionError("Connection closed before response")
        return json.loads(buf.decode("utf-8"))
    finally:
        client.close()
