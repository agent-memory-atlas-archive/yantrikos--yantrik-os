"""A real unix socket, real threads, real bytes: the surface as a caller meets it.

The socket-directory chain and its tightening (dir 0700, node 0600), the framing (one line in,
one line out, several per connection, with or without an `id`), `rpc.ping` and `rpc.service_id`,
the error codes, the peer's credentials reaching a handler, a name that is owned (a live socket
is not bound over; a dead one is replaced), the other names linked beside it, and a grant spent
through a stand-in shell that is itself a surface on a socket.
"""

import contextlib
import io
import json
import os
import socket
import stat
import threading

import support
from yantrik_surface import (Refusal, SocketBusy, Surface, call_once, caller, socket_dir, wire)


def hello(**kwargs):
    items = []
    s = Surface("hello", summary=lambda: "%d items" % len(items), **kwargs)

    @s.view
    def state():
        return {"items": list(items)}

    @s.action("add")
    def add(text: str) -> dict:
        """Add an item."""
        peer = caller()
        items.append(text)
        return {"pid": peer.pid if peer else None, "uid": peer.uid if peer else None}

    @s.action("wipe", grade="sensitive")
    def wipe() -> dict:
        """Empty the list. It cannot be undone."""
        items.clear()
        return {"wiped": True}

    return s


class Lines:
    """A raw connection that writes lines and reads one reply line per request."""

    def __init__(self, path):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(5)
        self.sock.connect(path)
        self.file = self.sock.makefile("rb")

    def ask(self, line):
        self.sock.sendall(line.encode("utf-8") + b"\n")
        return json.loads(self.file.readline().decode("utf-8"))

    def close(self):
        self.file.close()
        self.sock.close()


class TestTheSocketDirectory(support.MachineCase):
    def test_the_chain_starts_at_xdg_runtime_dir_and_tightens_it(self):
        path = socket_dir()
        self.assertEqual(path, self.machine.socket_dir)
        self.assertEqual(stat.S_IMODE(os.stat(path).st_mode), 0o700)
        support.quoted(self, support.RUST_SERVER,
                       'candidates.push(PathBuf::from(dir).join("yantrik"));\n        }\n    }\n'
                       '    candidates.push(PathBuf::from("/run/yantrik"));')
        support.quoted(self, support.RUST_SERVER,
                       'PathBuf::from(format!("/tmp/yantrik-{uid}"))')

    def test_an_empty_xdg_runtime_dir_is_no_candidate(self):
        os.environ["XDG_RUNTIME_DIR"] = "  "
        self.assertIn(socket_dir(), ("/run/yantrik", "/tmp/yantrik-%d" % os.getuid()))

    def test_asking_twice_touches_nothing(self):
        path = socket_dir()
        os.chmod(path, 0o700)
        wire._harden(path)
        self.assertEqual(socket_dir(), path)

    def test_a_loose_directory_is_tightened(self):
        path = socket_dir()
        os.chmod(path, 0o777)
        self.assertEqual(socket_dir(), path)
        self.assertEqual(stat.S_IMODE(os.stat(path).st_mode), 0o700)


class TestTheWire(support.MachineCase):
    def setUp(self):
        super().setUp()
        self.s = hello(aliases=("greetings",))
        self.server = self.s.serve_in_thread()
        self.addCleanup(self.s.stop)
        self.path = self.server.path

    def test_it_binds_app_id_sock_privately(self):
        self.assertEqual(self.path, os.path.join(self.machine.socket_dir, "app-hello.sock"))
        self.assertTrue(stat.S_ISSOCK(os.stat(self.path).st_mode))
        self.assertEqual(stat.S_IMODE(os.stat(self.path).st_mode), 0o600)

    def test_ping_and_service_id_are_the_transports(self):
        self.assertEqual(call_once(self.path, "rpc.ping", {})["result"], "pong")
        self.assertEqual(call_once(self.path, "rpc.service_id", {})["result"], "app-hello")
        support.quoted(self, support.RUST_SERVER,
                       '"rpc.ping" => {\n            return RpcResponse::success(req.id, '
                       'serde_json::json!("pong"));')

    def test_describe_and_act_round_trip(self):
        described = call_once(self.path, "app.describe", {})["result"]
        self.assertEqual(described["protocol"], 1)
        reply = call_once(self.path, "app.act", {
            "action": "add", "args": {"text": "milk"},
            "expect_revision": described["revision"]})
        self.assertEqual(reply["id"], 1)
        self.assertEqual(reply["result"]["state"], {"items": ["milk"]})
        # The kernel's account of who called, not the caller's.
        self.assertEqual(reply["result"]["result"]["pid"], os.getpid())
        self.assertEqual(reply["result"]["result"]["uid"], os.getuid())

    def test_several_requests_on_one_connection_with_and_without_an_id(self):
        conn = Lines(self.path)
        try:
            first = conn.ask('{"jsonrpc":"2.0","id":"a","method":"rpc.ping"}')
            self.assertEqual((first["id"], first["result"]), ("a", "pong"))
            second = conn.ask('{"jsonrpc":"2.0","method":"app.describe","params":{}}')
            self.assertIsNone(second["id"])
            self.assertEqual(second["result"]["app"], "hello")
            conn.sock.sendall(b"\n\n")  # blank lines are not requests
            third = conn.ask('{"method":"app.act","params":{"action":"add","args":{"text":"x"}},'
                             '"id":7}')
            self.assertEqual(third["id"], 7)
            self.assertTrue(third["result"]["accepted"])
        finally:
            conn.close()

    def test_the_error_codes(self):
        conn = Lines(self.path)
        try:
            parse = conn.ask("not json at all")
            self.assertEqual(parse["error"]["code"], -32700)
            self.assertIsNone(parse["id"])
            self.assertTrue(parse["error"]["message"].startswith("Parse error: "))
            self.assertEqual(conn.ask("[1,2]")["error"]["code"], -32700)
            self.assertEqual(conn.ask('{"id":3}')["error"]["code"], -32700)
            unknown = conn.ask('{"id":4,"method":"app.explode"}')
            self.assertEqual(unknown["error"]["code"], -32601)
            refused = conn.ask('{"id":5,"method":"app.act","params":{"action":"nope"}}')
            self.assertEqual(refused["error"]["code"], -32602)
            self.assertTrue(refused["error"]["message"].startswith("unknown action `nope`"))
            # And the connection is still good after all of that.
            self.assertEqual(conn.ask('{"id":6,"method":"rpc.ping"}')["result"], "pong")
        finally:
            conn.close()
        support.quoted(self, support.RUST_SERVER, 'format!("Parse error: {}", e)')

    def test_a_crashing_handler_is_an_answer_not_a_closed_socket(self):
        @self.s.action("boom")
        def boom() -> dict:
            """Fail."""
            raise ValueError("stub fault")

        with contextlib.redirect_stderr(io.StringIO()):
            reply = call_once(self.path, "app.act", {"action": "boom"})
        self.assertEqual(reply["error"]["code"], -32000)
        self.assertIn("ValueError: stub fault", reply["error"]["message"])
        self.assertEqual(call_once(self.path, "rpc.ping", {})["result"], "pong")

    def test_two_callers_at_once(self):
        replies = []
        threads = [threading.Thread(target=lambda: replies.append(
            call_once(self.path, "app.act", {"action": "add", "args": {"text": "t"}})))
            for _ in range(6)]
        for t in threads:
            t.start()
        for t in threads:
            t.join(10)
        self.assertEqual(len(replies), 6)
        ids = sorted(r["result"]["action_id"] for r in replies)
        self.assertEqual(len(set(ids)), 6, "every dispatch has its own name")

    def test_other_names_are_relative_links_to_the_socket(self):
        link = os.path.join(self.machine.socket_dir, "app-greetings.sock")
        self.assertTrue(os.path.islink(link))
        self.assertEqual(os.readlink(link), "app-hello.sock")
        self.assertEqual(call_once(link, "app.describe", {})["result"]["app"], "hello")

    def test_stop_takes_the_socket_and_its_names_away(self):
        link = os.path.join(self.machine.socket_dir, "app-greetings.sock")
        self.s.stop()
        self.assertFalse(os.path.lexists(self.path))
        self.assertFalse(os.path.lexists(link))


class TestOwnedNames(support.MachineCase):
    def test_a_live_surface_is_not_bound_over(self):
        first = hello()
        first.serve_in_thread()
        self.addCleanup(first.stop)
        second = hello()
        with self.assertRaises(SocketBusy) as caught:
            second.serve_in_thread()
        self.assertIsInstance(caught.exception, OSError)
        self.assertIn("already answered by a running process", str(caught.exception))
        # The first is untouched and still answers.
        self.assertEqual(call_once(first.server.path, "rpc.ping", {})["result"], "pong")

    def test_a_dead_socket_or_a_stale_file_is_replaced(self):
        os.makedirs(self.machine.socket_dir, exist_ok=True)
        path = os.path.join(self.machine.socket_dir, "app-hello.sock")
        dead = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        dead.bind(path)
        dead.close()  # the node stays; nobody listens behind it
        s = hello()
        s.serve_in_thread()
        self.addCleanup(s.stop)
        self.assertEqual(call_once(path, "rpc.ping", {})["result"], "pong")
        s.stop()
        with open(path, "w") as f:
            f.write("stale")
        s.serve_in_thread()
        self.assertEqual(call_once(path, "rpc.ping", {})["result"], "pong")

    def test_an_other_name_held_by_a_live_surface_is_left_alone(self):
        other = Surface("greetings")
        other.serve_in_thread()
        self.addCleanup(other.stop)
        s = hello(aliases=("greetings",))
        with contextlib.redirect_stderr(io.StringIO()) as said:
            s.serve_in_thread()
        self.addCleanup(s.stop)
        self.assertIn("is another live surface's name; this one does not take it",
                      said.getvalue())
        link = os.path.join(self.machine.socket_dir, "app-greetings.sock")
        self.assertFalse(os.path.islink(link))
        self.assertEqual(call_once(link, "app.describe", {})["result"]["app"], "greetings")

    def test_stop_leaves_a_path_someone_else_now_holds(self):
        s = hello()
        server = s.serve_in_thread()
        path = server.path
        os.unlink(path)  # somebody removed ours and bound their own
        theirs = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        theirs.bind(path)
        theirs.listen(1)
        self.addCleanup(theirs.close)
        s.stop()
        self.assertTrue(os.path.exists(path), "stop removed a socket it did not own")


class FakeShell:
    """The shell's grant store, stood up as a surface on `app-shell.sock` — what the dispatch
    calls `consume_approval` on. A grant `g-N` holds once, for exactly the call it names."""

    def __init__(self):
        self.approved = {}
        self.spent = []
        self.surface = Surface("shell")

        @self.surface.action("consume_approval", grade="standard")
        def consume_approval(request_id: str, app: str, action: str, args_json: dict) -> dict:
            """Spend a granted approval."""
            if request_id not in self.approved:
                raise Refusal("no approval request `%s`." % request_id)
            if request_id in self.spent:
                raise Refusal("`%s` was already used." % request_id)
            if self.approved[request_id] != (app, action, args_json):
                raise Refusal("`%s` was approved for another call." % request_id)
            self.spent.append(request_id)
            return {"spent": request_id}


class TestAGrantSpentOverTheSocket(support.MachineCase):
    mode = "ask"

    def test_the_dispatch_spends_it_through_app_shell(self):
        shell = FakeShell()
        shell.surface.serve_in_thread()
        self.addCleanup(shell.surface.stop)
        s = hello()
        self.assertTrue(self.refusal(lambda: s.act({"action": "wipe"})).startswith("GRANT:"))
        shell.approved["g-1"] = ("hello", "wipe", {})
        out = s.act({"action": "wipe", "grant": "g-1"})
        self.assertTrue(out["accepted"])
        self.assertEqual(shell.spent, ["g-1"])
        message = self.refusal(lambda: s.act({"action": "wipe", "grant": "g-1"}))
        self.assertEqual(message, "GRANT: `g-1` does not authorise hello.wipe — `g-1` was "
                                  "already used. Nothing was run; a grant covers one action, "
                                  "once, with the arguments the person was shown.")

    def test_no_shell_to_ask_is_a_refusal_not_a_run(self):
        s = hello()
        message = self.refusal(lambda: s.act({"action": "wipe", "grant": "g-9"}))
        self.assertTrue(message.startswith("GRANT: `g-9` does not authorise hello.wipe — "
                                           "Connection failed ("), message)
        self.assertIn("Nothing was run", message)
