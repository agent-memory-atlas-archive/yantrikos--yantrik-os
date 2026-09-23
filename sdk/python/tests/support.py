"""What the SDK's tests share: the path to the package, a private machine, and the Rust source.

Every test runs against a machine of its own — `HOME` and `XDG_RUNTIME_DIR` in a temporary
directory — so the ceiling, the mode and the socket directory are the test's, never the
developer's. And the refusal sentences are not copied into these tests by hand: `rust()` reads
the Rust source they are ported from, and `render()` fills a Rust format string the way
`format!` would, so the expected sentence IS the Rust one. When the Rust wording changes, the
fragment is no longer found and the test says which.
"""

import json
import os
import re
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
PACKAGE_ROOT = os.path.abspath(os.path.join(HERE, ".."))
REPO = os.path.abspath(os.path.join(PACKAGE_ROOT, "..", ".."))
if PACKAGE_ROOT not in sys.path:
    sys.path.insert(0, PACKAGE_ROOT)

import yantrik_surface  # noqa: E402,F401
from yantrik_surface import wire  # noqa: E402

RUST_CONTROL = "crates/yantrik-app-runtime/src/control.rs"
RUST_GATE = "crates/yantrik-ipc-transport/src/gate.rs"
RUST_SERVER = "crates/yantrik-ipc-transport/src/server.rs"
RUST_CONTRACTS = "crates/yantrik-ipc-contracts/src/control_surface.rs"
# The `yantrik-surface` crate (piece B of the SDK design): typed arguments. Quoted when present.
RUST_SURFACE_ARGS = "crates/yantrik-surface/src/args.rs"
YOS = os.path.join(REPO, "deploy", "yantrik-os", "yos")
EXAMPLE = os.path.join(PACKAGE_ROOT, "examples", "hello_surface.py")

_sources = {}


def rust(path):
    """A Rust source file with its string continuations joined (`\\` at a line end drops the
    newline and the next line's leading whitespace, as rustc does), or None outside the repo."""
    if path not in _sources:
        full = os.path.join(REPO, path)
        if not os.path.isfile(full):
            _sources[path] = None
        else:
            with open(full, encoding="utf-8") as f:
                _sources[path] = re.sub(r"\\\n\s*", "", f.read())
    return _sources[path]


def quoted(case, path, fragment, skip=True):
    """Fail unless `fragment` is in the Rust source at `path`. When the source is not here (the
    package tested on its own, outside this repository; or a crate not landed yet) the test is
    skipped — or, with `skip=False`, the quote alone is let go and the rest of the test runs."""
    source = rust(path)
    if source is None:
        if not skip:
            return
        case.skipTest("%s is not in this tree, so the wording cannot be checked against it" % path)
    case.assertTrue(fragment in source,
                    "the Rust wording this port quotes is no longer in %s — it changed there, so "
                    "change the port and this test with it: %r" % (path, fragment))


def render(fragment, *positional, **named):
    """Fill a Rust format string: `{}` from `positional` in order, `{name}` from `named`."""
    values = iter(positional)

    def fill(match):
        key = match.group(1)
        return str(next(values) if key == "" else named[key])

    return re.sub(r"\{(\w*)\}", fill, fragment)


class Machine:
    """A private HOME (settings, mode) and runtime dir (sockets) for one test."""

    def __init__(self, ceiling=None, mode=None):
        self.tmp = tempfile.TemporaryDirectory(prefix="yantrik-sdk-")
        self.home = os.path.join(self.tmp.name, "home")
        self.runtime = os.path.join(self.tmp.name, "run")
        os.makedirs(os.path.join(self.home, ".config", "yantrik"))
        os.makedirs(self.runtime)
        self.settings = os.path.join(self.home, ".config", "yantrik", "settings.yaml")
        self.mode_file = os.path.join(self.home, ".config", "yantrik", "mind-mode.json")
        self.saved = {}
        if ceiling is not None:
            self.set_ceiling(ceiling)
        if mode is not None:
            self.set_mode(mode)

    def set_ceiling(self, ceiling):
        with open(self.settings, "w", encoding="utf-8") as f:
            f.write("theme: dark\ntool_permission: %s\n" % ceiling)

    def set_mode(self, mode, rules=()):
        doc = mode if isinstance(mode, dict) else {
            "mode": mode, "session_rules": [{"app": a, "action": x} for a, x in rules]}
        with open(self.mode_file, "w", encoding="utf-8") as f:
            json.dump(doc, f)

    def __enter__(self):
        for key, value in (("HOME", self.home), ("XDG_RUNTIME_DIR", self.runtime)):
            self.saved[key] = os.environ.get(key)
            os.environ[key] = value
        return self

    def __exit__(self, *exc):
        for key, value in self.saved.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value
        self.tmp.cleanup()
        return False

    @property
    def socket_dir(self):
        return os.path.join(self.runtime, "yantrik")

    def env(self):
        """The environment a subprocess on this machine gets."""
        env = dict(os.environ)
        env["HOME"] = self.home
        env["XDG_RUNTIME_DIR"] = self.runtime
        env["PYTHONPATH"] = PACKAGE_ROOT + os.pathsep + env.get("PYTHONPATH", "")
        return env


class MachineCase(unittest.TestCase):
    """A test on a machine of its own: the ceiling open, the mode `bypass`, unless a test pins
    them — the tests about everything except the gate do what the runtime's tests do."""

    ceiling = "dangerous"
    mode = "bypass"

    def setUp(self):
        self.machine = Machine(self.ceiling, self.mode)
        self.machine.__enter__()
        self.addCleanup(self.machine.__exit__, None, None, None)

    def refusal(self, fn, code=wire.RPC_INVALID_PARAMS):
        """Run fn, expect a JSON-RPC error with `code`, return its message."""
        with self.assertRaises(wire.RpcError) as caught:
            fn()
        self.assertEqual(caught.exception.code, code, caught.exception.message)
        return caught.exception.message
