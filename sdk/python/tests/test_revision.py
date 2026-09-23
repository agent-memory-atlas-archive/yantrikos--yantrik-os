"""The revision: FNV-1a-64 over the summary, a zero byte, and the state as serde_json renders it.

Two kinds of vector. The first is the one the Rust crate pins in
`revision_vector_shared_with_the_python_port` (control_surface.rs) — read out of that file here,
so the two cannot drift without a test failing. The rest were produced by serde_json 1.0.149
(the version in Cargo.lock) running `View::revision()`'s own code over the same values: floats
at both ends of serde_json's layout, integers past what it holds exactly, escapes, key order.
They pin the canonical rendering, which is where a port gets it wrong.
"""

import re
import unittest

import support
from yantrik_surface import revision, wire

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

# Generated with serde_json 1.0.149: summary, the state as Python builds it, what
# `Value::to_string` wrote, and the revision `View::revision()` computed.
SERDE_VECTORS = [
    ("bits",
     [0.0, -0.0, 1.0, -1.0, 0.1, 0.3, 2.5, 100.0, 1e15, 1e16, 1.5e16, 1e17, 1e21, 1e22, 1e23,
      1.2345678901234568e20, 1.2345e20, 12345678.9, 0.0001, 0.00001],
     "[0.0,-0.0,1.0,-1.0,0.1,0.3,2.5,100.0,1000000000000000.0,1e+16,1.5e+16,1e+17,1e+21,"
     "1e+22,1e+23,1.2345678901234568e+20,1.2345e+20,12345678.9,0.0001,0.00001]",
     "d41819b88a97d405"),
    ("bits",
     [0.000025, 1e-6, 1.5e-6, 1e-7, 1.234e-7, 5e-324, 2.2250738585072014e-308,
      1.7976931348623157e308, 1.5e300, 3.14159, 0.000123, 9007199254740992.0,
      4503599627370496.0, 1e100, 1.1e-100, 6.02214076e23, 299792458.0, 0.5, 0.25, 1 / 3],
     "[0.000025,1e-6,1.5e-6,1e-7,1.234e-7,5e-324,2.2250738585072014e-308,"
     "1.7976931348623157e+308,1.5e+300,3.14159,0.000123,9007199254740992.0,4503599627370496.0,"
     "1e+100,1.1e-100,6.02214076e+23,299792458.0,0.5,0.25,0.3333333333333333]",
     "0d03bc26f2294f7e"),
    ("ints",
     {"max_u64": 18446744073709551615, "min_i64": -9223372036854775808,
      "past_u64": 18446744073709551616, "zero": 0, "neg": -1},
     '{"max_u64":18446744073709551615,"min_i64":-9223372036854775808,"neg":-1,'
     '"past_u64":1.8446744073709552e+19,"zero":0}',
     "d13f5931bae691b0"),
    ("text é — \U0001F600",
     {"s": "tab\there \x1f \x7f é \U0001F600 \"q\" \\ /", "nl": "a\nb\r\b\f"},
     '{"nl":"a\\nb\\r\\b\\f","s":"tab\\there \\u001f \x7f é \U0001F600 \\"q\\" \\\\ /"}',
     "46a46d5814253ffe"),
    ("keys",
     {"é": 1, "e": 2, "z": 3, "Z": 4, "ä": 5, "a b": 6, "": 7, "10": 8, "9": 9},
     '{"":7,"10":8,"9":9,"Z":4,"a b":6,"e":2,"z":3,"ä":5,"é":1}',
     "fe95ab9a5b8c0ce1"),
    ("nested",
     {"list": [1, 2.0, [True, False, None], {"y": 1, "x": []}], "obj": {"b": {}, "a": [{}]}},
     '{"list":[1,2.0,[true,false,null],{"x":[],"y":1}],"obj":{"a":[{}],"b":{}}}',
     "90e16044aebd4f15"),
    ("", {}, "{}", "d884b5186b651423"),
    ("scalar", [1, "two", 3.5], '[1,"two",3.5]', "25876ba5bab7ddd8"),
]


class TestTheSharedVector(unittest.TestCase):
    def test_the_hex_is_the_one_the_rust_crate_asserts(self):
        source = support.rust(support.RUST_CONTRACTS)
        if source is None:
            self.skipTest("control_surface.rs is not in this tree")
        pinned = re.search(r'fn revision_vector_shared_with_the_python_port\(\).*?'
                           r'assert_eq!\(view\.revision\(\), "([0-9a-f]{16})"\)', source, re.S)
        self.assertIsNotNone(pinned, "the shared vector test is gone from control_surface.rs")
        self.assertEqual(pinned.group(1), "6d6dd36469ee8664")

    def test_this_port_computes_it(self):
        self.assertEqual(revision(SHARED_SUMMARY, SHARED_STATE), "6d6dd36469ee8664")


class TestSerdeVectors(unittest.TestCase):
    def test_the_rendering_is_serde_jsons_byte_for_byte(self):
        for summary, state, rendered, _ in SERDE_VECTORS:
            with self.subTest(summary=summary, rendered=rendered[:40]):
                self.assertEqual(wire.canonical_state(state), rendered)

    def test_the_revision_is_view_revision(self):
        for summary, state, _, expected in SERDE_VECTORS:
            with self.subTest(summary=summary):
                self.assertEqual(revision(summary, state), expected)


class TestRevisionProperties(unittest.TestCase):
    def test_key_order_is_not_part_of_a_state(self):
        self.assertEqual(revision("s", {"x": 1, "y": 2}), revision("s", {"y": 2, "x": 1}))

    def test_it_changes_with_the_state_and_with_the_summary(self):
        self.assertNotEqual(revision("s", {"x": 1}), revision("s", {"x": 2}))
        self.assertNotEqual(revision("s", {"x": 1}), revision("t", {"x": 1}))

    def test_summary_and_state_cannot_run_into_each_other(self):
        self.assertNotEqual(revision("ab", {"c": 1}), revision("a", {"bc": 1}))

    def test_it_is_sixteen_lowercase_hex_digits(self):
        self.assertRegex(revision("", {}), r"^[0-9a-f]{16}$")

    def test_what_json_cannot_hold_is_what_serde_json_would_hold(self):
        # A tuple is an array; a float that is not finite is null (`json!(f64::NAN)`); an
        # IntEnum is its number.
        import enum

        class Level(enum.IntEnum):
            HIGH = 3

        self.assertEqual(wire.canonical_state({"t": (1, 2), "n": float("nan"),
                                               "i": float("inf"), "e": Level.HIGH}),
                         '{"e":3,"i":null,"n":null,"t":[1,2]}')

    def test_a_value_that_is_not_json_is_named(self):
        with self.assertRaises(TypeError) as caught:
            wire.canonical_state({"when": object()})
        self.assertIn("object", str(caught.exception))


if __name__ == "__main__":
    unittest.main()
