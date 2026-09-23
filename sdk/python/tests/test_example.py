"""The example, driven by the OS's own client: `examples/hello_surface.py` (at the top of the
repository, beside its Rust twin `examples/hello-surface`) served on a socket and
`deploy/yantrik-os/yos` run against it exactly as the README tells a person to — describe, act,
a refusal, and a sensitive act in ask mode, where `yos` raises the card, waits for the person,
and acts again with the grant, which the surface spends through the shell. The shell here is a
stand-in built with this package (tests/fake_shell.py) run as a program named yantrik-ui, so
the shell-peer rule in yos and in the dispatch is met, not patched; it answers the three approval
actions yos and the dispatch call. Everything else is the real thing over real sockets.
"""

import importlib.util
import json
import os
import subprocess
import sys
import time
import unittest

import support


def load_example():
    spec = importlib.util.spec_from_file_location("hello_surface_example", support.EXAMPLE)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class TestTheExample(support.MachineCase):
    mode = "ask"

    def setUp(self):
        super().setUp()
        self.example = load_example()
        self.surface = self.example.surface
        self.surface.serve_in_thread()
        self.addCleanup(self.surface.stop)

    def test_it_is_a_complete_surface(self):
        out = self.surface.act({"action": "add", "args": {"text": "milk", "count": 2}})
        self.assertEqual(out["state"]["items"], ["milk", "milk"])
        self.assertEqual(out["summary"], "Hello — 2 items")
        grades = {a["name"]: (a["permission"], a["settles"])
                  for a in self.surface.describe_json()["actions"]}
        self.assertEqual(grades, {"add": ("standard", "on return"),
                                  "clear": ("sensitive", "later")})


@unittest.skipUnless(os.path.isfile(support.YOS), "deploy/yantrik-os/yos is not in this tree")
class TestWithYos(TestTheExample):
    def yos(self, *args, ok=True):
        done = subprocess.run([sys.executable, support.YOS, *args], env=self.machine.env(),
                              capture_output=True, text=True, timeout=60)
        if ok:
            self.assertEqual(done.returncode, 0, done.stderr)
        return done

    def test_yos_describe_hello(self):
        out = self.yos("describe", "hello").stdout
        self.assertIn("Hello — 0 items", out)
        self.assertIn("act: add(text, count?)  [standard, settles on return]", out)
        self.assertIn("text: string — what to put on the list", out)
        self.assertIn("act: clear()  [sensitive, settles later]", out)

    def test_yos_act_hello_add(self):
        out = self.yos("act", "hello", "add", "text=milk").stdout
        self.assertIn("Hello — 1 item", out)
        self.assertIn("accepted: True, settled: True", out)
        # `count=2` is read as the integer the action publishes.
        out = self.yos("act", "hello", "add", "text=eggs", "count=2").stdout
        self.assertIn("Hello — 3 items", out)
        self.assertEqual(self.example.items, ["milk", "eggs", "eggs"])

    def test_a_refusal_reaches_the_caller_in_the_apps_words(self):
        done = self.yos("act", "hello", "add", "text=x", "count=lots", ok=False)
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("`add` argument `count` must be an integer, and a string arrived",
                      done.stderr)

    def test_a_sensitive_act_in_ask_mode_goes_through_the_card_and_the_grant(self):
        # The shell is a stand-in whose program is a `yantrik-ui` binary, so both `yos` and
        # this package's dispatch find the desktop's shell behind `app-shell.sock`.
        shell = support.ShellStandIn(self.machine).start(self)
        self.surface.act({"action": "add", "args": {"text": "milk"}})
        started = time.monotonic()
        out = self.yos("act", "hello", "clear").stdout
        self.assertIn("asking", out)
        self.assertIn("accepted: True, settled: False", out)
        seen = shell.state()
        self.assertEqual(seen["spent"], ["appr-1"], "the grant was spent, once, by the dispatch")
        self.assertEqual(seen["requests"]["appr-1"], ["hello", "clear", {}])
        # Settles later: the list empties after the answer.
        deadline = started + 10
        while self.example.items and time.monotonic() < deadline:
            time.sleep(0.05)
        self.assertEqual(self.example.items, [])

    def test_run_as_a_program_it_answers_until_sigterm_and_unbinds(self):
        # A second process, `python3 hello_surface.py` as the README says; this test's own copy
        # is serving too, so the program's copy must refuse to take the name — and once this
        # copy stops, it serves.
        self.surface.stop()
        program = subprocess.Popen([sys.executable, support.EXAMPLE], env=self.machine.env(),
                                   stderr=subprocess.PIPE, text=True)
        path = os.path.join(self.machine.socket_dir, "app-hello.sock")
        try:
            deadline = time.monotonic() + 10
            while not os.path.exists(path) and time.monotonic() < deadline:
                time.sleep(0.05)
            out = self.yos("act", "hello", "add", "text=milk").stdout
            self.assertIn("Hello — 1 item", out)
        finally:
            program.terminate()
            _, err = program.communicate(timeout=10)
        self.assertEqual(program.returncode, 0, err)
        self.assertIn("hello answering on %s (2 actions)" % path, err)
        self.assertFalse(os.path.exists(path), "the socket outlived the program")

    def test_a_second_copy_does_not_take_the_first_ones_name(self):
        program = subprocess.run([sys.executable, support.EXAMPLE], env=self.machine.env(),
                                 capture_output=True, text=True, timeout=30)
        self.assertNotEqual(program.returncode, 0)
        self.assertIn("another instance owns", program.stderr)
        self.assertIn("it answered rpc.ping as `app-hello`", program.stderr)
        self.assertEqual(self.yos("describe", "hello").returncode, 0)

    def test_yos_check_finds_nothing_wrong(self):
        # The OS's own conformance checker, as an author runs it in their CI: every check it
        # makes passes, and it never ran a handler to find out.
        done = self.yos("check", "hello", "--json")
        report = json.loads(done.stdout)
        rows = report["surfaces"][0]["checks"]
        self.assertTrue(report["ok"], [r for r in rows if r["status"] == "fail"])
        passed = {r["check"] for r in rows if r["status"] == "pass"}
        for check in ("ping", "describe", "protocol", "schema", "grades", "params", "secrets",
                      "revision", "steady", "method", "empty", "unknown", "missing",
                      "undeclared", "types", "stale"):
            self.assertIn(check, passed, rows)
        self.assertEqual(self.example.items, [], "yos check ran a handler")

    def test_without_a_shell_to_ask_nothing_runs(self):
        self.surface.act({"action": "add", "args": {"text": "milk"}})
        done = self.yos("act", "hello", "clear", ok=False)
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("needs the person's Allow", done.stderr)
        self.assertEqual(self.example.items, ["milk"])


if __name__ == "__main__":
    unittest.main()
