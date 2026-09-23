"""The dispatch vectors, replayed through this package's real dispatch.

`deploy/yantrik-os/dispatch-vectors.json` is generated from the `yantrik-surface` crate
(`crates/yantrik-surface/src/vectors.rs`) by running its dispatch: what it does around the gate.

* `coerce` — one declared argument and the arguments a caller sent: what the handler reads
  (converted without loss where they convert, defaults filled in) or the exact refusal.
* `order` — one action, a machine's ceiling and mode, the grants a person allowed, and calls made
  in turn: the answer, what the handler read, and which grants were spent after each. A call its
  own arguments refuse leaves its grant unspent; a grant is bound to the arguments as sent.

Both are replayed here as a caller meets them — `check_arguments` and `as_declared` for the first,
`Surface.act` on a machine with those files and a stand-in shell holding the grants for the second
— so what is checked is what this package does, to the byte of every sentence. A missing file is
a failure inside this repository. `YANTRIK_DISPATCH_VECTORS=<path>` replays another copy.
"""

import json
import os
import unittest

import support
from yantrik_surface import Action, GrantRefused, Param, Surface, wire
from yantrik_surface.surface import as_declared, check_arguments

VECTORS = os.environ.get("YANTRIK_DISPATCH_VECTORS") or os.path.join(
    support.REPO, "deploy", "yantrik-os", "dispatch-vectors.json")


def load(case):
    if not os.path.isfile(VECTORS):
        if os.path.isdir(os.path.join(support.REPO, "crates", "yantrik-surface")):
            case.fail("%s is missing; the crate that generates it is in this tree" % VECTORS)
        case.skipTest("no dispatch vectors outside the repository")
    with open(VECTORS, encoding="utf-8") as f:
        return json.load(f)


def same(a, b):
    """Equal as JSON means it: `1` is not `1.0`, `true` is not `1`."""
    return json.dumps(a, sort_keys=True) == json.dumps(b, sort_keys=True)


def param_of(declared):
    """A `Param` rebuilt from the vector's `{name, required, type, description, enum?, items?,
    default?}`."""
    extra = {}
    if "default" in declared:
        extra["default"] = declared["default"]
    if "enum" in declared:
        extra["enum"] = declared["enum"]
    if "items" in declared:
        extra["items"] = declared["items"]["type"]
    return Param(declared["name"], declared["type"], declared.get("description", ""),
                 optional=not declared["required"], **extra)


class TestTheCoercions(unittest.TestCase):
    def test_every_row(self):
        rows = load(self)["coerce"]
        self.assertGreater(len(rows), 50)
        wrong = []
        for row in rows:
            spec = Action("act", "An action with one argument", params=[param_of(row["param"])])
            refusal = check_arguments(spec, row["args"])
            if "refusal" in row:
                if refusal != row["refusal"]:
                    wrong.append((row, refusal))
                continue
            if refusal is not None:
                wrong.append((row, refusal))
                continue
            got = as_declared(spec, row["args"])
            if not same(got, row["handler_gets"]):
                wrong.append((row, got))
        self.assertEqual(wrong, [], "\n".join("%s -> %r" % (json.dumps(r, ensure_ascii=False), g)
                                              for r, g in wrong))

    def test_the_call_as_sent_is_left_as_it_was(self):
        spec = Action("act", "An action", params=[Param("x", "integer")])
        args = {"x": "12"}
        self.assertEqual(as_declared(spec, args), {"x": 12})
        self.assertEqual(args, {"x": "12"}, "it is what a grant is bound to")


class TestTheOrder(support.MachineCase):
    def test_every_vector(self):
        vectors = load(self)["order"]
        self.assertGreater(len(vectors), 5)
        for vector in vectors:
            with self.subTest(vector["id"]):
                self.replay(vector)

    def replay(self, vector):
        self.machine.set_ceiling(vector["ceiling"])
        self.machine.set_mode(vector["mode"])
        allowed = {a["grant"]: a["args"] for a in vector["allowed"]}
        spent = []
        action = vector["action"]

        def shell(grant, app, name, args):
            if grant not in allowed or (app, name) != (vector["app"], action["name"]) \
                    or not same(args, allowed[grant]):
                raise GrantRefused("not the call `%s` was allowed for." % grant)
            if grant in spent:
                raise GrantRefused("`%s` was already used." % grant)
            spent.append(grant)

        s = Surface(vector["app"], summary="vector", spend_grant=shell)
        s.add_action(Action(action["name"], action["description"], action["permission"],
                            [param_of(p) for p in action["params"]]),
                     lambda args: {"got": args})
        for i, call in enumerate(vector["calls"]):
            params = {"action": action["name"], "args": call["args"]}
            if call["grant"]:
                params["grant"] = call["grant"]
            where = "%s, call %d" % (vector["id"], i)
            try:
                out = s.act(params)
            except wire.RpcError as e:
                self.assertEqual(e.code, wire.RPC_INVALID_PARAMS, where)
                self.assertEqual("refused", call["outcome"], "%s: refused %s" % (where, e.message))
                self.assertEqual(e.message, call["refusal"], where)
            else:
                self.assertEqual("allow", call["outcome"], "%s: allowed" % where)
                self.assertTrue(same(out["result"]["got"], call["handler_gets"]),
                                "%s: the handler read %r" % (where, out["result"]["got"]))
            self.assertEqual(sorted(spent), sorted(call["spent"]), "%s: spent" % where)


if __name__ == "__main__":
    unittest.main()
