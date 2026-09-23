"""The two envelopes, key for key: what `describe_json` and `act_json` build in the Rust
contracts, plus `protocol: 1` on describe, and the action schema `Action::schema()` publishes
with the richer parameter types."""

import unittest
from typing import Literal, Optional

import support
from yantrik_surface import PROTOCOL, Action, Param, Surface, revision


def hello():
    items = []
    s = Surface("hello", summary=lambda: "%d items" % len(items))

    @s.view
    def state():
        return {"items": list(items)}

    @s.action("add", grade="standard")
    def add(text: str, count: int = 1) -> dict:
        """Add an item to the list."""
        items.extend([text] * count)
        return {"added": count}

    @s.action("clear", grade="sensitive", settles="later", expected_seconds=5)
    def clear() -> dict:
        """Empty the list. It cannot be undone."""
        items.clear()
        return {"cleared": True}

    return s, items


class TestDescribe(support.MachineCase):
    def test_the_envelope_has_the_runtimes_keys_and_the_protocol(self):
        s, _ = hello()
        out = s.describe_json()
        self.assertEqual(set(out), {"app", "protocol", "summary", "state", "revision", "actions"})
        self.assertEqual(out["app"], "hello")
        self.assertEqual(out["protocol"], PROTOCOL)
        self.assertEqual(PROTOCOL, 1)
        self.assertEqual(out["summary"], "0 items")
        self.assertEqual(out["state"], {"items": []})
        self.assertEqual(out["revision"], revision("0 items", {"items": []}))
        self.assertEqual([a["name"] for a in out["actions"]], ["add", "clear"])

    def test_the_rust_contract_builds_the_same_keys(self):
        # `describe_json` in control_surface.rs; `protocol` is the one key this adds.
        support.quoted(self, support.RUST_CONTRACTS, '"app": app_id,\n        "summary": '
                       'view.summary,\n        "state": view.state,\n        "revision": '
                       'view.revision(),\n        "actions": actions.iter()')

    def test_an_action_schema_is_action_schema_key_for_key(self):
        s, _ = hello()
        add, clear = s.describe_json()["actions"]
        self.assertEqual(set(add), {"name", "description", "permission", "settles", "parameters"})
        self.assertEqual(add["permission"], "standard")
        self.assertEqual(add["settles"], "on return")
        self.assertEqual(add["description"], "Add an item to the list.")
        self.assertEqual(add["parameters"], {
            "type": "object",
            "properties": {
                "text": {"type": "string", "description": ""},
                "count": {"type": "integer", "description": "", "default": 1},
            },
            "required": ["text"],
        })
        # Declared duration, and settling later, are published when the action says so.
        self.assertEqual(clear["settles"], "later")
        self.assertEqual(clear["permission"], "sensitive")
        self.assertEqual(clear["expected_seconds"], 5)
        self.assertEqual(clear["parameters"], {"type": "object", "properties": {}, "required": []})

    def test_the_rust_schema_has_these_keys(self):
        support.quoted(self, support.RUST_CONTRACTS,
                       'serde_json::json!({\n            "name": self.name,\n            '
                       '"description": self.description,\n            "permission": '
                       'self.permission,\n            "settles": if self.deferred { "later" } '
                       'else { "on return" },')

    def test_nothing_published_is_said_the_runtimes_way(self):
        s = Surface("bare")
        out = s.describe_json()
        self.assertEqual(out["summary"], "bare (no description published)")
        self.assertEqual(out["state"], {})
        support.quoted(self, support.RUST_CONTROL,
                       'View::new(format!("{} (no description published)", self.app_id))')

    def test_a_regrade_is_what_describe_publishes(self):
        s, _ = hello()
        self.assertEqual(s.regrade("add", "sensitive"), "sensitive")
        self.assertEqual(s.describe_json()["actions"][0]["permission"], "sensitive")
        self.assertEqual(s.published_grade("add"), "sensitive")
        self.assertIsNone(s.published_grade("nope"))


class TestParameterSchemas(unittest.TestCase):
    def test_every_type_publishes_as_json_schema(self):
        s = Surface("types")

        @s.action("everything")
        def everything(text: str, whole: int, real: float, flag: bool,
                       colour: Literal["red", "green"], tags: list[str], blob: dict,
                       size: Literal["s", "m", "l"] = "m", note: Optional[str] = None,
                       scale: float = 1.5, loose: list = None) -> dict:
            """Take one of everything."""
            return {}

        props = s.actions[0].schema()["parameters"]["properties"]
        self.assertEqual(props["text"], {"type": "string", "description": ""})
        self.assertEqual(props["whole"], {"type": "integer", "description": ""})
        self.assertEqual(props["real"], {"type": "number", "description": ""})
        self.assertEqual(props["flag"], {"type": "boolean", "description": ""})
        self.assertEqual(props["colour"],
                         {"type": "string", "description": "", "enum": ["red", "green"]})
        self.assertEqual(props["tags"],
                         {"type": "array", "description": "", "items": {"type": "string"}})
        self.assertEqual(props["blob"], {"type": "object", "description": ""})
        self.assertEqual(props["size"], {"type": "string", "description": "",
                                         "enum": ["s", "m", "l"], "default": "m"})
        self.assertEqual(props["note"], {"type": "string", "description": ""})
        self.assertEqual(props["scale"], {"type": "number", "description": "", "default": 1.5})
        self.assertEqual(props["loose"], {"type": "array", "description": ""})
        self.assertEqual(s.actions[0].schema()["parameters"]["required"],
                         ["text", "whole", "real", "flag", "colour", "tags", "blob"])

    def test_the_crates_own_example_publishes_the_same_json(self):
        # `the_richer_types_publish_as_json_schema`, the `yantrik-surface` work's test in the Rust
        # contracts (piece B), value for value.
        spec = Action("export", "Export the document", params=[
            Param("page", "integer"),
            Param("format", enum=["pdf", "png"], default="pdf"),
            Param("tags", "array", items="string", optional=True),
            Param("options", "object", optional=True),
            Param("dpi", "integer", "Dots per inch", default=150),
        ], expected_seconds=20).schema()
        props = spec["parameters"]["properties"]
        self.assertEqual(props["page"], {"type": "integer", "description": ""})
        self.assertEqual(props["format"], {"type": "string", "description": "",
                                           "enum": ["pdf", "png"], "default": "pdf"})
        self.assertEqual(props["tags"], {"type": "array", "description": "",
                                         "items": {"type": "string"}})
        self.assertEqual(props["options"], {"type": "object", "description": ""})
        self.assertEqual(props["dpi"], {"type": "integer", "description": "Dots per inch",
                                        "default": 150})
        self.assertEqual(spec["parameters"]["required"], ["page"])
        self.assertEqual(spec["expected_seconds"], 20)

    def test_an_explicit_table_publishes_the_same_way(self):
        spec = Action("paint", "Paint the thing.", "standard", [
            Param("colour", enum=["red", "green"], description="which"),
            Param("coats", "integer", "how many", default=1),
            Param("where", "array", items="number", optional=True),
        ])
        self.assertEqual(spec.schema()["parameters"], {
            "type": "object",
            "properties": {
                "colour": {"type": "string", "description": "which", "enum": ["red", "green"]},
                "coats": {"type": "integer", "description": "how many", "default": 1},
                "where": {"type": "array", "description": "", "items": {"type": "number"}},
            },
            "required": ["colour"],
        })


class TestAct(support.MachineCase):
    def test_the_envelope_has_the_runtimes_keys(self):
        s, items = hello()
        out = s.act({"action": "add", "args": {"text": "milk", "count": 2}})
        self.assertEqual(set(out), {"app", "action_id", "accepted", "settled", "result",
                                    "revision", "summary", "state"})
        self.assertEqual(out["app"], "hello")
        self.assertEqual(out["action_id"], "app-hello#1")
        self.assertIs(out["accepted"], True)
        self.assertIs(out["settled"], True)
        self.assertEqual(out["result"], {"added": 2})
        self.assertEqual(out["summary"], "2 items")
        self.assertEqual(out["state"], {"items": ["milk", "milk"]})
        # The revision in the answer is the revision of the state in the answer.
        self.assertEqual(out["revision"], revision(out["summary"], out["state"]))
        self.assertEqual(out["revision"], s.describe_json()["revision"])

    def test_the_rust_contract_builds_the_same_keys(self):
        support.quoted(self, support.RUST_CONTRACTS,
                       '"app": app_id,\n        "action_id": action_id,\n        "accepted": '
                       'true,\n        "settled": settled,\n        "result": result,\n        '
                       '"revision": view.revision(),\n        "summary": view.summary,\n        '
                       '"state": view.state,')

    def test_settles_later_answers_settled_false(self):
        s, _ = hello()
        out = s.act({"action": "clear"})
        self.assertIs(out["accepted"], True)
        self.assertIs(out["settled"], False)

    def test_action_ids_are_numbered_per_surface_and_refusals_take_one(self):
        # The runtime names a dispatch before it looks at anything else, so a refused call
        # uses a number too; what matters is that ids are unique and increasing.
        s, _ = hello()
        self.assertEqual(s.act({"action": "add", "args": {"text": "a"}})["action_id"],
                         "app-hello#1")
        self.refusal(lambda: s.act({"action": "nope"}))
        self.assertEqual(s.act({"action": "add", "args": {"text": "b"}})["action_id"],
                         "app-hello#3")
        support.quoted(self, support.RUST_CONTROL, 'format!("{service_id}#{}", '
                       'NEXT.fetch_add(1, Ordering::Relaxed))')

    def test_a_result_that_is_not_json_is_the_apps_bug_not_a_reply(self):
        s = Surface("odd")

        @s.action("when")
        def when() -> dict:
            """Say when."""
            return {"at": object()}

        with self.assertRaises(TypeError):
            s.act({"action": "when"})


if __name__ == "__main__":
    unittest.main()
