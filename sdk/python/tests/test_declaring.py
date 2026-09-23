"""Declaring a surface: what a signature publishes, and the mistakes refused at declaration
time — where the author is, rather than at the first call, where a mind is."""

import unittest
from typing import Annotated, Literal, Optional, Union

import support  # noqa: F401 - puts the package on the path
from yantrik_surface import Action, Param, Refusal, Surface


def published(fn, **kwargs):
    s = Surface("t")
    s.action(**kwargs)(fn)
    return s.actions[0].schema()


class TestFromASignature(unittest.TestCase):
    def test_hints_defaults_and_descriptions(self):
        def add(text: Annotated[str, "what to add"], count: int = 1,
                where: Optional[Literal["top", "bottom"]] = None) -> dict:
            """Add an item.

            Everything after the first paragraph is for the author, not the card."""

        schema = published(add, grade="standard")
        self.assertEqual(schema["description"], "Add an item.")
        props = schema["parameters"]["properties"]
        self.assertEqual(props["text"], {"type": "string", "description": "what to add"})
        self.assertEqual(props["count"], {"type": "integer", "description": "", "default": 1})
        self.assertEqual(props["where"], {"type": "string", "description": "",
                                          "enum": ["top", "bottom"]})
        self.assertEqual(schema["parameters"]["required"], ["text"])

    def test_optional_and_annotated_nest_either_way(self):
        def f(a: Optional[Annotated[int, "outer optional"]] = None,
              b: Annotated[Optional[int], "inner optional"] = None,
              c: int | None = None) -> dict:
            """F."""

        props = published(f)["parameters"]["properties"]
        self.assertEqual(props["a"], {"type": "integer", "description": "outer optional"})
        self.assertEqual(props["b"], {"type": "integer", "description": "inner optional"})
        self.assertEqual(props["c"], {"type": "integer", "description": ""})

    def test_descriptions_can_be_given_beside_the_signature(self):
        def add(text: str) -> dict:
            """Add."""

        schema = published(add, params={"text": "what to add"}, description="Add a thing.")
        self.assertEqual(schema["description"], "Add a thing.")
        self.assertEqual(schema["parameters"]["properties"]["text"]["description"], "what to add")

    def test_no_hint_reads_the_default(self):
        def f(a, b=2, c=0.5, d=True, e="x", g=None) -> dict:
            """F."""

        props = published(f)["parameters"]["properties"]
        self.assertEqual({k: v["type"] for k, v in props.items()},
                         {"a": "string", "b": "integer", "c": "number", "d": "boolean",
                          "e": "string", "g": "string"})

    def test_the_bare_decorator_uses_the_function_name(self):
        s = Surface("t")

        @s.action
        def tidy() -> dict:
            """Tidy."""
            return {}

        self.assertEqual(s.actions[0].name, "tidy")
        self.assertEqual(s.actions[0].permission, "standard")

    def test_string_annotations_resolve(self):
        def f(count: "int" = 1) -> "dict":
            """F."""

        self.assertEqual(published(f)["parameters"]["properties"]["count"]["type"], "integer")


class TestMistakesAreRefusedAtDeclaration(unittest.TestCase):
    def refused(self, fn, error=TypeError, **kwargs):
        with self.assertRaises(error) as caught:
            published(fn, **kwargs)
        return str(caught.exception)

    def test_an_action_needs_a_purpose(self):
        def quiet(x: str) -> dict:
            pass

        self.assertIn("no description", self.refused(quiet))

    def test_a_grade_must_be_on_the_ladder(self):
        def f() -> dict:
            """F."""

        self.assertIn("safe < standard < sensitive < dangerous",
                      self.refused(f, ValueError, grade="lethal"))

    def test_settles_is_on_return_or_later(self):
        def f() -> dict:
            """F."""

        self.refused(f, ValueError, settles="eventually")
        self.refused(f, ValueError, expected_seconds=2.5)
        self.refused(f, ValueError, expected_seconds=-1)
        self.refused(f, ValueError, expected_seconds=True)

    def test_arguments_are_named_one_by_one(self):
        def star(*things) -> dict:
            """F."""

        def kw(**things) -> dict:
            """F."""

        def pos(a, /) -> dict:
            """F."""

        self.assertIn("named one by one", self.refused(star))
        self.assertIn("named one by one", self.refused(kw))
        self.assertIn("positional-only", self.refused(pos))

    def test_a_type_a_caller_cannot_send_is_refused(self):
        def f(when: object) -> dict:
            """F."""

        def g(x: Union[int, str]) -> dict:
            """G."""

        self.assertIn("cannot send", self.refused(f))
        self.assertIn("one type", self.refused(g))

    def test_describing_an_argument_that_does_not_exist(self):
        def f(a: str) -> dict:
            """F."""

        self.assertIn("`b`", self.refused(f, params={"b": "nope"}))

    def test_a_name_published_twice(self):
        s = Surface("t")
        s.add_action(Action("a", "A."), lambda args: {})
        with self.assertRaises(ValueError):
            s.add_action(Action("a", "A again."), lambda args: {})

    def test_params_are_checked_as_they_are_built(self):
        with self.assertRaises(ValueError):
            Param("x", "float")
        with self.assertRaises(ValueError):
            Param("x", enum=[])
        with self.assertRaises(ValueError):
            Param("x", enum=["a", 1])
        with self.assertRaises(ValueError):
            Param("x", "string", items="string")
        with self.assertRaises(ValueError):
            Action("a", "A.", params=[Param("x"), Param("x")])
        # As the crate's `declaration_problems` has it: only a string can be one of a list.
        with self.assertRaises(ValueError) as caught:
            Param("x", "integer", enum=["1", "2"])
        self.assertIn("only a string can be one of a list", str(caught.exception))

    def test_an_enum_of_numbers_is_refused_from_a_signature_too(self):
        def f(size: Literal[1, 2, 3]) -> dict:
            """F."""

        self.assertIn("only a string", self.refused(f, ValueError))

    def test_an_app_id_is_one_word(self):
        for bad in ("", "two words", "a/b", None):
            with self.assertRaises(ValueError):
                Surface(bad)


class TestRegrade(unittest.TestCase):
    def test_a_typo_keeps_the_grade_it_had(self):
        s = Surface("studio")

        @s.action("generate")
        def generate(prompt: str) -> dict:
            """Make an image."""

        fragment = ('"`{permission}` is not a level this OS defines ({}), so `{action}` kept '
                    'the grade it had"')
        support.quoted(self, support.RUST_CONTROL, fragment)
        with self.assertRaises(Refusal) as caught:
            s.regrade("generate", "spicy")
        self.assertEqual(str(caught.exception), support.render(
            fragment.strip('"'), "safe < standard < sensitive < dangerous", permission="spicy",
            action="generate"))
        self.assertEqual(s.published_grade("generate"), "standard")

    def test_an_action_it_does_not_have(self):
        s = Surface("studio")
        fragment = '"this app has no action `{action}`; it offers: {}"'
        support.quoted(self, support.RUST_CONTROL, fragment)
        with self.assertRaises(Refusal) as caught:
            s.regrade("nope", "safe")
        self.assertEqual(str(caught.exception),
                         support.render(fragment.strip('"'), "", action="nope"))


if __name__ == "__main__":
    unittest.main()
