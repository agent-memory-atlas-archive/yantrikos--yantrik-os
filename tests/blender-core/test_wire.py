"""The wire: the revision hash, the socket chain, and a real roundtrip over a real socket.

The revision test is the one that crosses a language boundary: the hex it asserts is the hex
`crates/yantrik-ipc-contracts/src/control_surface.rs` asserts for the identical vector, in
`revision_vector_shared_with_the_python_port`. Same input, same hash, two implementations —
if either side changes what it hashes, one of the two tests fails, and the failure names this
vector.
"""

import json
import os
import socket
import stat
import sys
import tempfile
import threading
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.abspath(
    os.path.join(os.path.dirname(os.path.abspath(__file__)),
                 "..", "..", "apps", "blender", "addon")))
sys.path.insert(0, os.path.abspath(
    os.path.join(os.path.dirname(os.path.abspath(__file__)),
                 "..", "..", "sdk", "python")))

from yantrik_surface import wire  # noqa: E402

# The vector shared with the Rust test. Any change here must be made there in the same
# commit, and the new hex computed by running either test.
SHARED_SUMMARY = 'Blender — "monkey.blend", 3 objects, Cycles 1920x1080'
SHARED_STATE = {
    "scene": "Scene",
    "file": "/tmp/monkey.blend",
    "unsaved": False,
    "objects": [
        {"name": "Suzanne", "type": "MESH", "location": [0.0, 0.0, 0.0],
         "dimensions": [2.0, 2.0, 2.0]}
    ],
    "objects_total": 3,
    "camera": {"name": "Camera", "location": [4.0, -4.0, 3.0]},
    "render": {"engine": "cycles", "resolution": "1920x1080", "samples": 32,
               "output": "/tmp/monkey.png"},
    "last_render": None,
    "notice": "",
    "background": True,
}
SHARED_REVISION = "6d6dd36469ee8664"


class TestRevision(unittest.TestCase):
    def test_matches_the_rust_hash_for_the_pinned_vector(self):
        self.assertEqual(wire.revision(SHARED_SUMMARY, SHARED_STATE), SHARED_REVISION)

    def test_is_stable_across_key_order(self):
        # The Rust test of the same name, mirrored: insertion order is not part of a state.
        a = {"x": 1, "y": 2}
        b = {"y": 2, "x": 1}
        self.assertEqual(wire.revision("s", a), wire.revision("s", b))

    def test_changes_with_state(self):
        self.assertNotEqual(wire.revision("s", {"x": 1}), wire.revision("s", {"x": 2}))

    def test_summary_and_state_cannot_collide_into_each_other(self):
        # The zero byte between them: a summary ending mid-word must not read as a different
        # split of the same bytes.
        self.assertNotEqual(wire.revision("ab", {"c": 1}), wire.revision("a", {"bc": 1}))

    def test_canonical_state_is_compact_sorted_utf8(self):
        text = wire.canonical_state({"b": "é", "a": [1, None, True]})
        self.assertEqual(text, '{"a":[1,null,true],"b":"é"}')


class TestSocketDir(unittest.TestCase):
    def test_follows_xdg_runtime_dir_and_hardens(self):
        with tempfile.TemporaryDirectory() as tmp:
            old = os.environ.get("XDG_RUNTIME_DIR")
            os.environ["XDG_RUNTIME_DIR"] = tmp
            try:
                directory = wire.socket_dir()
            finally:
                if old is None:
                    del os.environ["XDG_RUNTIME_DIR"]
                else:
                    os.environ["XDG_RUNTIME_DIR"] = old
            self.assertEqual(directory, os.path.join(tmp, "yantrik"))
            mode = stat.S_IMODE(os.stat(directory).st_mode)
            self.assertEqual(mode, 0o700)

    def test_default_socket_path_is_app_prefixed(self):
        with tempfile.TemporaryDirectory() as tmp:
            old = os.environ.get("XDG_RUNTIME_DIR")
            os.environ["XDG_RUNTIME_DIR"] = tmp
            try:
                path = wire.default_socket_path("blender")
            finally:
                if old is None:
                    del os.environ["XDG_RUNTIME_DIR"]
                else:
                    os.environ["XDG_RUNTIME_DIR"] = old
            self.assertEqual(os.path.basename(path), "app-blender.sock")

    def test_hardening_an_already_private_directory_is_a_no_op(self):
        # Asking twice must not chmod twice: the early return is what a read-only mount or a
        # sandbox survives.
        with tempfile.TemporaryDirectory() as tmp:
            os.chmod(tmp, 0o700)
            wire._harden(tmp)  # must not raise


class StubSurface:
    service_id = "app-blender"

    def __init__(self):
        self.calls = []

    def handle(self, method, params):
        self.calls.append((method, params))
        if method == "app.describe":
            return {"app": "blender", "summary": "stub"}
        if method == "app.boom":
            raise ValueError("stub fault")
        raise wire.RpcError(wire.RPC_METHOD_NOT_FOUND,
                            "unknown method `%s`; this app serves app.describe, app.act"
                            % method)


class TestServerRoundtrip(unittest.TestCase):
    """A real unix socket, a real thread, real bytes — the framing tested as deployed."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.path = os.path.join(self.tmp.name, "app-blender.sock")
        self.surface = StubSurface()
        self.server = wire.Server(self.path, self.surface)

    def tearDown(self):
        self.server.stop()
        self.tmp.cleanup()

    def test_serves_requests_and_unlinks_on_stop(self):
        self.server.start()
        self.assertTrue(os.path.exists(self.path))
        mode = stat.S_IMODE(os.stat(self.path).st_mode)
        self.assertEqual(mode, 0o600, "a socket that drives a scene is not world-open")

        self.assertEqual(wire.call_once(self.path, "rpc.ping", {})["result"], "pong")
        self.assertEqual(wire.call_once(self.path, "rpc.service_id", {})["result"],
                         "app-blender")

        reply = wire.call_once(self.path, "app.describe", {"x": 1})
        self.assertEqual(reply["result"], {"app": "blender", "summary": "stub"})
        self.assertEqual(self.surface.calls[-1], ("app.describe", {"x": 1}))

        self.server.stop()
        self.assertFalse(os.path.exists(self.path))

    def test_a_stale_socket_is_replaced_not_refused(self):
        # A crashed run leaves the node behind; bind must not fail over it.
        with open(self.path, "w") as f:
            f.write("stale")
        self.server.start()
        self.assertEqual(wire.call_once(self.path, "rpc.ping", {})["result"], "pong")

    def test_unknown_method_is_method_not_found_with_the_house_sentence(self):
        self.server.start()
        reply = wire.call_once(self.path, "app.explode", {})
        self.assertEqual(reply["error"]["code"], wire.RPC_METHOD_NOT_FOUND)
        self.assertEqual(reply["error"]["message"],
                         "unknown method `app.explode`; this app serves app.describe, "
                         "app.act")

    def test_garbage_in_is_a_parse_error_with_a_null_id(self):
        self.server.start()
        client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        client.settimeout(5)
        client.connect(self.path)
        client.sendall(b"not json at all\n")
        reply = json.loads(client.recv(65536).decode())
        client.close()
        self.assertIsNone(reply["id"])
        self.assertEqual(reply["error"]["code"], wire.RPC_PARSE_ERROR)
        self.assertTrue(reply["error"]["message"].startswith("Parse error: "))

    def test_an_unhandled_fault_is_a_transport_error_not_a_closed_socket(self):
        self.server.start()
        reply = wire.call_once(self.path, "app.boom", {})
        self.assertEqual(reply["error"]["code"], wire.RPC_TRANSPORT_ERROR)
        self.assertIn("ValueError", reply["error"]["message"])
        # And the server is still serving.
        self.assertEqual(wire.call_once(self.path, "rpc.ping", {})["result"], "pong")

    def test_two_callers_are_served_without_blocking_each_other(self):
        self.server.start()
        replies = []

        def call():
            replies.append(wire.call_once(self.path, "app.describe", {})["result"])

        threads = [threading.Thread(target=call) for _ in range(4)]
        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=10)
        self.assertEqual(len(replies), 4)


if __name__ == "__main__":
    unittest.main()
