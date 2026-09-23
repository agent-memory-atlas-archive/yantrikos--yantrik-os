"""The addon's policy and wire, replayed against the vectors the Rust gate writes.

`deploy/yantrik-os/surface-vectors.json` is `yantrik_ipc_transport::gate::decide` written out
for every combination of grade, ceiling, mode, session rule, grant and whether the action's own
description says it cannot be undone — with the exact sentence each refusal is made in — plus the
revision hash over a handful of views. The Rust side fails when the file is not what `decide`
produces; this fails when the addon's port is not what the file says. Neither can move alone.

The sentences are compared in full: a caller reads them as the app's own words, and `yos act`
branches on them.
"""

import json
import os
import sys
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.abspath(os.path.join(HERE, "..", "..", "apps", "blender", "addon")))

import fake_bpy  # noqa: E402,F401  (the addon's package imports bpy)
from yantrik_surface import wire  # noqa: E402
from yantrik_surface import surface  # noqa: E402

VECTORS = os.path.abspath(os.path.join(HERE, "..", "..", "deploy", "yantrik-os",
                                       "surface-vectors.json"))


def load():
    with open(VECTORS, encoding="utf-8") as f:
        return json.load(f)


class TestTheDecision(unittest.TestCase):
    def test_every_decision_vector_is_decided_the_same_way(self):
        doc = load()
        vectors = doc["decide"]
        # An emptied file must not pass by testing nothing.
        self.assertGreaterEqual(len(vectors), 640)
        drifted = []
        for v in vectors:
            rules = {(v["app"], v["action"])} if v["session_rule"] else set()
            got = surface.decide(v["app"], v["action"], v["grade"], v["purpose"], v["ceiling"],
                                 v["mode"], rules, v["grant"])
            want = None if v["outcome"] == "allow" else v["refusal"]
            if got != want:
                drifted.append("%s:\n  want %r\n  got  %r" % (v["id"], want, got))
        self.assertEqual(drifted, [], "\n".join(drifted[:6]))

    def test_the_ladder_modes_floor_and_phrases_are_the_gates(self):
        doc = load()
        self.assertEqual(list(surface.LADDER), doc["ladder"])
        self.assertEqual(sorted(surface.MODES.items()), sorted(tuple(m) for m in doc["modes"]))
        self.assertEqual(surface.SOCKET_FLOOR, doc["socket_floor"])
        self.assertEqual(list(surface.UNRECOVERABLE_PHRASES), doc["phrases"])
        self.assertEqual(surface.PROTOCOL, doc["protocol"])

    def test_the_sentence_is_read_the_way_the_gate_reads_it(self):
        for v in load()["purposes"]:
            self.assertEqual(surface.unrecoverable(v["purpose"]), v["unrecoverable"], v["purpose"])


class TestTheRevision(unittest.TestCase):
    def test_every_revision_vector_hashes_the_same(self):
        for v in load()["revision"]:
            self.assertEqual(wire.revision(v["summary"], v["state"]), v["revision"], v["summary"])

    @unittest.expectedFailure
    def test_floats_the_ports_render_differently(self):
        """Known: `json.dumps` writes 1e-05 and 1e+16 where serde_json writes 1e-5 and 1e16, so a
        state carrying such a float hashes differently here. The protocol says render as
        serde_json does (docs/surface-protocol.md, "revision"); the Python SDK takes this on.
        When it is fixed this test passes, and `expectedFailure` has to come off."""
        for v in load()["revision_float_edges"]:
            self.assertEqual(wire.canonical_state(v["state"]), v["rendered"])
            self.assertEqual(wire.revision(v["summary"], v["state"]), v["revision"])


if __name__ == "__main__":
    unittest.main()
