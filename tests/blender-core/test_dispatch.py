"""The dispatch layer: grades, ceiling, revision guard, and the refusal vocabulary.

This is the half that has to be indistinguishable from `yantrik-app-runtime::control`:
the same order of checks, the same sentences to the punctuation, the same error codes, the
same envelopes. The sentences are asserted in full — not `assertIn` on a fragment — because
a caller on the other end (yos, yos-mcp, a harness, a conformance probe) reads them as the
app's own words, and a paraphrase is a different promise.
"""

import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.abspath(
    os.path.join(os.path.dirname(os.path.abspath(__file__)),
                 "..", "..", "apps", "blender", "addon")))

import fake_bpy  # noqa: E402
from yantrik_surface import wire  # noqa: E402
from yantrik_surface.bridge import BridgeTimeout, DirectBridge  # noqa: E402
from yantrik_surface.scene import Scene  # noqa: E402
from yantrik_surface.surface import ACTIONS, LADDER, Surface  # noqa: E402

ALL_ACTION_NAMES = [a.name for a in ACTIONS]


def make_surface(ceiling=None, bridge=None):
    """A Surface over a fake bpy. `ceiling` writes a settings file; None means no file at
    all, which is a machine that has never opened Settings — the default, `sensitive`."""
    fake = fake_bpy.make_bpy()
    tmp = tempfile.NamedTemporaryFile("w", suffix=".yaml", delete=False)
    if ceiling is not None:
        tmp.write("other_key: 1\ntool_permission: %s\n" % ceiling)
    tmp.close()
    surface = Surface(Scene(fake), bridge or DirectBridge(), app_id="blender",
                      settings_path=tmp.name)
    return surface, fake, tmp.name


def refusal(asserts, fn):
    """Run fn, expect an RpcError of -32602, return the message."""
    with asserts.assertRaises(wire.RpcError) as caught:
        fn()
    asserts.assertEqual(caught.exception.code, wire.RPC_INVALID_PARAMS)
    return caught.exception.message


class TestTheActionTable(unittest.TestCase):
    def test_every_action_the_brief_names_is_here_and_graded(self):
        expected = {
            "new_scene": "standard",
            "add_primitive": "standard",
            "delete_object": "standard",
            "transform": "standard",
            "set_material": "standard",
            "set_camera": "standard",
            "set_light": "standard",
            "import_model": "standard",
            "set_render": "standard",
            "render": "sensitive",
            "save": "sensitive",
            "open": "sensitive",
            "run_python": "dangerous",
            "screenshot": "standard",
        }
        self.assertEqual({a.name: a.permission for a in ACTIONS}, expected)

    def test_every_grade_is_on_the_ladder(self):
        for action in ACTIONS:
            self.assertIn(action.permission, LADDER, action.name)

    def test_every_action_says_what_it_is_for(self):
        for action in ACTIONS:
            self.assertGreater(len(action.description), 20, action.name)

    def test_delete_object_admits_what_cannot_be_undone(self):
        delete = next(a for a in ACTIONS if a.name == "delete_object")
        self.assertIn("not recoverable", delete.description)

    def test_run_python_is_dangerous_and_says_what_it_reaches(self):
        run = next(a for a in ACTIONS if a.name == "run_python")
        self.assertEqual(run.permission, "dangerous")
        self.assertIn("anything", run.description.lower())


class TestDescribe(unittest.TestCase):
    def test_the_envelope_has_the_keys_of_every_other_app(self):
        surface, _, path = make_surface()
        self.addCleanup(os.unlink, path)
        out = surface.describe_json()
        self.assertEqual(set(out), {"app", "summary", "state", "revision", "actions"})
        self.assertEqual(out["app"], "blender")
        self.assertEqual(len(out["revision"]), 16)
        self.assertEqual([a["name"] for a in out["actions"]], ALL_ACTION_NAMES)

    def test_the_action_schema_is_the_published_shape(self):
        surface, _, path = make_surface()
        self.addCleanup(os.unlink, path)
        schema = {a["name"]: a for a in surface.describe_json()["actions"]}
        render = schema["render"]
        self.assertEqual(set(render), {"name", "description", "permission", "settles",
                                       "parameters"})
        self.assertEqual(render["permission"], "sensitive")
        self.assertEqual(render["settles"], "on return")
        self.assertEqual(render["parameters"]["required"], ["output"])
        self.assertEqual(set(render["parameters"]["properties"]["output"]),
                         {"type", "description"})
        # Every parameter of every action carries a description — an empty string if there
        # is nothing to add, but the key is there, because a caller that reads it must not
        # have to ask whether an absent key means "no description" or "old app".
        for action in surface.describe_json()["actions"]:
            for prop in action["parameters"]["properties"].values():
                self.assertIn("description", prop)
                self.assertIsInstance(prop["description"], str)

    def test_new_scene_takes_no_arguments_and_says_so(self):
        surface, _, path = make_surface()
        self.addCleanup(os.unlink, path)
        schema = {a["name"]: a for a in surface.describe_json()["actions"]}
        self.assertEqual(schema["new_scene"]["parameters"]["properties"], {})
        self.assertEqual(schema["new_scene"]["parameters"]["required"], [])


class TestDispatchOrder(unittest.TestCase):
    def setUp(self):
        self.surface, self.fake, self.settings = make_surface()
        self.addCleanup(os.unlink, self.settings)

    def act(self, action, **args):
        return self.surface.act({"action": action, "args": args})

    def test_an_empty_action_is_refused_before_anything_else(self):
        for params in ({}, {"action": ""}, {"action": "   "}, {"action": 5}):
            message = refusal(self, lambda p=params: self.surface.act(p))
            self.assertEqual(message, "act needs a non-empty `action`")

    def test_unknown_action_lists_the_whole_vocabulary(self):
        message = refusal(self, lambda: self.act("make_coffee"))
        self.assertEqual(
            message,
            "unknown action `make_coffee`; this app offers: " + ", ".join(ALL_ACTION_NAMES))

    def test_unknown_method_names_the_two_methods_it_serves(self):
        with self.assertRaises(wire.RpcError) as caught:
            self.surface.handle("app.vibe_check", {})
        self.assertEqual(caught.exception.code, wire.RPC_METHOD_NOT_FOUND)
        self.assertEqual(
            caught.exception.message,
            "unknown method `app.vibe_check`; this app serves app.describe, app.act")

    def test_missing_required_argument(self):
        self.assertEqual(refusal(self, lambda: self.act("render")),
                         "`render` needs argument `output`")
        self.assertEqual(refusal(self, lambda: self.act("add_primitive")),
                         "`add_primitive` needs argument `kind`")

    def test_unexpected_argument_lists_the_ones_it_takes(self):
        message = refusal(self, lambda: self.act("render", output="/tmp/x.png", quality=5))
        self.assertEqual(message,
                         "`render` has no argument `quality`; it takes: output")

    def test_unexpected_argument_to_an_action_with_none(self):
        message = refusal(self, lambda: self.act("new_scene", please=True))
        self.assertEqual(message,
                         "`new_scene` takes no arguments, but `please` was given")

    def test_the_ceiling_is_checked_before_the_arguments(self):
        # Order pinned by the runtime: a dangerous action with a missing argument is refused
        # on the grade, because the grade is the more important fact about the call.
        message = refusal(self, lambda: self.act("run_python"))
        self.assertTrue(message.startswith("CEILING: blender.run_python is graded "
                                           "`dangerous`, above this machine's `sensitive` "
                                           "ceiling"), message)

    def test_a_stale_revision_is_refused_with_the_current_state_of_the_world(self):
        message = refusal(self, lambda: self.surface.act({
            "action": "add_primitive",
            "args": {"kind": "cube"},
            "expect_revision": "deadbeefdeadbeef",
        }))
        summary, state = self.surface.scene.snapshot()
        current = wire.revision(summary, state)
        self.assertEqual(
            message,
            "STALE: this app is at revision %s and you acted on deadbeefdeadbeef. "
            "It now reports: %s. Read it again before deciding." % (current, summary))

    def test_a_current_revision_is_accepted(self):
        described = self.surface.describe_json()
        out = self.surface.act({
            "action": "add_primitive",
            "args": {"kind": "cube"},
            "expect_revision": described["revision"],
        })
        self.assertTrue(out["accepted"])
        self.assertEqual(out["result"]["type"], "MESH")

    def test_a_handler_refusal_comes_out_as_the_apps_own_sentence(self):
        message = refusal(self, lambda: self.act("delete_object", name="Ghost"))
        self.assertEqual(message, "there is no object `Ghost` in this scene")

    def test_the_failure_is_said_twice_once_in_the_error_once_in_the_state(self):
        refusal(self, lambda: self.act("delete_object", name="Ghost"))
        state = self.surface.describe_json()["state"]
        self.assertEqual(state["notice"], "there is no object `Ghost` in this scene")
        # And a success clears it: a notice that outlives the fault is a lie by staleness.
        self.act("add_primitive", kind="cube")
        state = self.surface.describe_json()["state"]
        self.assertEqual(state["notice"], "")


class TestEnvelopeOnSuccess(unittest.TestCase):
    def test_act_answers_with_the_post_action_world(self):
        surface, _, path = make_surface()
        self.addCleanup(os.unlink, path)
        out = surface.act({"action": "add_primitive", "args": {"kind": "monkey"}})
        self.assertEqual(set(out), {"app", "action_id", "accepted", "settled", "result",
                                    "revision", "summary", "state"})
        self.assertEqual(out["app"], "blender")
        self.assertEqual(out["action_id"], "app-blender#1")
        self.assertTrue(out["accepted"])
        self.assertTrue(out["settled"])
        self.assertEqual(out["result"]["object"], "Suzanne.001")
        self.assertEqual(out["state"]["objects_total"], 1)
        self.assertIn("1 object", out["summary"])
        # The revision in the answer is the revision of the state in the answer.
        self.assertEqual(out["revision"], wire.revision(out["summary"], out["state"]))
        # And the next action is numbered after this one.
        second = surface.act({"action": "new_scene", "args": {}})
        self.assertEqual(second["action_id"], "app-blender#2")


class TestCeiling(unittest.TestCase):
    def surfaces_with(self, ceiling):
        surface, _, path = make_surface(ceiling=ceiling)
        self.addCleanup(os.unlink, path)
        return surface

    def test_no_settings_file_is_the_default_ceiling(self):
        surface, _, path = make_surface(ceiling=None)
        os.unlink(path)  # the file was made empty; remove it to be a machine with none
        self.assertEqual(surface.configured_ceiling(), "sensitive")

    def test_an_explicit_ceiling_is_read(self):
        self.assertEqual(self.surfaces_with("dangerous").configured_ceiling(), "dangerous")
        self.assertEqual(self.surfaces_with("standard").configured_ceiling(), "standard")

    def test_a_quoted_or_padded_value_is_still_read(self):
        self.assertEqual(self.surfaces_with('"dangerous"').configured_ceiling(), "dangerous")

    def test_a_value_off_the_ladder_falls_back_to_the_default(self):
        self.assertEqual(self.surfaces_with("godmode").configured_ceiling(), "sensitive")

    def test_dangerous_runs_when_the_ceiling_says_it_may(self):
        surface = self.surfaces_with("dangerous")
        out = surface.act({"action": "run_python",
                           "args": {"code": "print(2 + 2)"}})
        self.assertTrue(out["accepted"])
        self.assertIn("4", out["result"]["printed"])

    def test_sensitive_actions_are_refused_under_a_standard_ceiling(self):
        surface = self.surfaces_with("standard")
        message = refusal(self, lambda: surface.act(
            {"action": "render", "args": {"output": "/tmp/x.png"}}))
        self.assertEqual(
            message,
            "CEILING: blender.render is graded `sensitive`, above this machine's "
            "`standard` ceiling (`tool_permission` in ~/.config/yantrik/settings.yaml), "
            "so it was not run. An action at that grade needs a person to authorise it "
            "directly — raise the ceiling in Settings if that is the intent.")

    def test_run_python_under_the_default_ceiling_gets_the_full_sentence(self):
        surface, _, path = make_surface()
        message = refusal(self, lambda: surface.act(
            {"action": "run_python", "args": {"code": "import os"}}))
        self.assertEqual(
            message,
            "CEILING: blender.run_python is graded `dangerous`, above this machine's "
            "`sensitive` ceiling (`tool_permission` in ~/.config/yantrik/settings.yaml), "
            "so it was not run. An action at that grade needs a person to authorise it "
            "directly — raise the ceiling in Settings if that is the intent.")

    def test_a_grade_off_the_ladder_is_a_ceiling_bug_said_as_one(self):
        surface, _, path = make_surface(ceiling="dangerous")
        spec = next(a for a in surface.actions if a.name == "new_scene")
        original = spec.permission
        spec.permission = "lethal"
        try:
            message = refusal(self, lambda: surface.act(
                {"action": "new_scene", "args": {}}))
            self.assertEqual(
                message,
                "CEILING: blender.new_scene is graded `lethal`, which is not a level this "
                "OS defines (safe < standard < sensitive < dangerous), so it was not run.")
        finally:
            spec.permission = original


class TestBridgeTimeout(unittest.TestCase):
    def test_a_main_thread_that_never_turns_up_is_a_transport_error(self):
        class StalledBridge(DirectBridge):
            def submit(self, fn, timeout=None):
                raise BridgeTimeout()

        surface, _, path = make_surface(bridge=StalledBridge())
        with self.assertRaises(wire.RpcError) as caught:
            surface.describe_json()
        self.assertEqual(caught.exception.code, wire.RPC_TRANSPORT_ERROR)
        self.assertEqual(caught.exception.message, "app did not answer within 10s")
        with self.assertRaises(wire.RpcError) as caught:
            surface.act({"action": "add_primitive", "args": {"kind": "cube"}})
        self.assertEqual(caught.exception.message, "app did not answer within 30s")
        with self.assertRaises(wire.RpcError) as caught:
            surface.act({"action": "render", "args": {"output": "/tmp/x.png"}})
        self.assertEqual(caught.exception.message, "app did not answer within 1800s")


class TestQueuedBridge(unittest.TestCase):
    """The real bridge, with a pump thread standing in for Blender's main thread."""

    def test_submit_crosses_threads_and_comes_back(self):
        import threading
        import time

        from yantrik_surface.bridge import QueuedBridge

        bridge = QueuedBridge()
        results = []

        def pump():
            while True:
                bridge.pump()
                time.sleep(0.005)

        worker = threading.Thread(target=pump, daemon=True)
        worker.start()
        try:
            results.append(bridge.submit(lambda: "from the main thread", timeout=5))
            results.append(bridge.submit(lambda: 2 + 2, timeout=5))
        finally:
            bridge.wake()
        self.assertEqual(results, ["from the main thread", 4])

    def test_a_job_that_raises_gives_the_submitter_the_exception(self):
        import threading
        import time

        from yantrik_surface.bridge import QueuedBridge
        from yantrik_surface.scene import Refusal

        bridge = QueuedBridge()
        outcome = {}

        def submit():
            try:
                bridge.submit(lambda: (_ for _ in ()).throw(Refusal("no")), timeout=5)
            except Refusal as e:
                outcome["raised"] = str(e)

        submitter = threading.Thread(target=submit)
        submitter.start()
        # Pump only once the job is actually queued, or the pump and the submit race and the
        # test flaps: a pump that ran too early finds nothing and the submit times out.
        for _ in range(500):
            with bridge._lock:
                queued = bool(bridge._queue)
            if queued:
                break
            time.sleep(0.01)
        self.assertTrue(queued, "the job never reached the queue")
        bridge.pump()
        submitter.join(timeout=5)
        self.assertEqual(outcome.get("raised"), "no")

    def test_wake_releases_a_waiter_with_the_truth(self):
        import threading

        from yantrik_surface.bridge import QueuedBridge

        bridge = QueuedBridge()
        outcome = {}

        def submit():
            try:
                bridge.submit(lambda: "never runs", timeout=30)
            except RuntimeError as e:
                outcome["error"] = str(e)

        submitter = threading.Thread(target=submit)
        submitter.start()
        # Wait for the job to be queued and the submitter to be inside its wait, so what is
        # tested is a waiter being released, not a submit finding the door already closed.
        import time as _time
        for _ in range(500):
            with bridge._lock:
                queued = bool(bridge._queue)
            if queued:
                break
            _time.sleep(0.01)
        self.assertTrue(queued, "the job never reached the queue")
        bridge.wake()
        submitter.join(timeout=5)
        self.assertEqual(outcome.get("error"), "the app is closing; it took no action")


if __name__ == "__main__":
    unittest.main()
