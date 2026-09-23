"""The vocabulary: what this app offers, what each action costs, and how refusals are worded.

This file is a port of `yantrik-app-runtime::control`'s dispatch — same order of checks,
same sentences, same error codes — because the point of the surface is that a caller cannot
tell this app from one written against the runtime. If the two ever disagree, the runtime
is right and this file is wrong; the pinned vectors in `tests/blender-core` exist to catch
that drift.

The order of dispatch, exactly as `Registry::act` runs it:

  1. unknown action;
  2. an action graded off the ladder (a bug, said as CEILING);
  3. an action above this machine's ceiling (a policy, said as CEILING);
  4. a missing required argument;
  5. an argument the action does not take;
  6. STALE — the caller acted on a revision the app has moved past;
  7. the handler itself; anything it raises as `Refusal` is a refusal in the app's words.

Checks 1-5 need only the action table, so they run on the RPC thread. Checks 6-7 need the
scene, so they cross the bridge — and the guard, the handler and the post-snapshot cross
as ONE job, the same atomic turn the runtime makes on the main thread, so no other caller
can move the scene between "is this revision current" and "here is what your action did".

The ceiling is read per call from `tool_permission` in ~/.config/yantrik/settings.yaml,
defaulting to `sensitive` exactly like the runtime: a missing or malformed setting is a
machine that has not said `dangerous` is allowed, not one that has.
"""

import os
import threading

from . import wire
from .bridge import BridgeTimeout
from .scene import Refusal

LADDER = ("safe", "standard", "sensitive", "dangerous")
DEFAULT_CEILING = "sensitive"
SETTINGS_PATH = os.path.join(os.path.expanduser("~"), ".config", "yantrik", "settings.yaml")

# How long the main thread gets per kind of turn. A render is the outlier by design: the
# honest timeout for "draw this scene" is "however long the scene takes", and 30 minutes is
# where guessing stops being useful. Everything else fails loudly rather than hanging a
# caller.
DESCRIBE_TIMEOUT = 10.0
DEFAULT_TIMEOUT = 30.0


class Param:
    """One argument in an action's schema. `control_surface::Param` in Python."""

    def __init__(self, name, type="string", description="", optional=False):
        self.name = name
        self.type = type
        self.description = description
        self.optional = optional


class ActionSpec:
    """One row of the action table: name, grade, purpose, arguments, patience.

    `description` is the purpose sentence — the one line a person reads on an approval card
    and a mind reads in `yos describe`, so it says what the action *does to the scene*, in
    this app's own words, including what cannot be taken back.
    """

    def __init__(self, name, description, permission="standard", params=(),
                 timeout=DEFAULT_TIMEOUT):
        self.name = name
        self.description = description
        self.permission = permission
        self.params = list(params)
        self.timeout = timeout
        self.deferred = False  # nothing here settles later; even a render answers on return

    def schema(self):
        """The published shape. `Action::schema()` key for key: every parameter carries a
        description (the empty string when there is nothing to add — an absent key has been
        read as a missing field by a caller before), and `required` lists the non-optional
        ones in declaration order."""
        properties = {}
        for p in self.params:
            properties[p.name] = {"type": p.type, "description": p.description}
        return {
            "name": self.name,
            "description": self.description,
            "permission": self.permission,
            "settles": "later" if self.deferred else "on return",
            "parameters": {
                "type": "object",
                "properties": properties,
                "required": [p.name for p in self.params if not p.optional],
            },
        }


def _action(name, purpose, permission, params, timeout=DEFAULT_TIMEOUT):
    return ActionSpec(name, purpose, permission, params, timeout)


ACTIONS = [
    _action("new_scene",
            "Throw the current scene away and start an empty one. Anything unsaved in it is "
            "lost, and in a background Blender there is no undo to argue with.",
            "standard", [], timeout=60.0),
    _action("add_primitive",
            "Add a mesh primitive to the scene, where you say, at the size you say.",
            "standard", [
                Param("kind", description="cube, sphere, cylinder, plane or monkey"),
                Param("name", description="what to call it; Blender names it otherwise",
                      optional=True),
                Param("location", description="centre point, three metres like `1,2,3`",
                      optional=True),
                Param("scale", description="three factors like `1,1,1`, or one for all axes",
                      optional=True),
            ]),
    _action("delete_object",
            "Delete an object from the scene. Blender's own undo can bring it back in a "
            "window; past that undo it is not recoverable, and a background Blender has no "
            "undo at all.",
            "standard", [
                Param("name", description="the object's exact name, as describe lists it"),
            ]),
    _action("transform",
            "Move, rotate or scale an object. Give at least one of the three; rotation is "
            "in degrees.",
            "standard", [
                Param("name", description="the object's exact name, as describe lists it"),
                Param("location", description="three metres like `1,2,3`", optional=True),
                Param("rotation", description="three degrees like `0,0,90`", optional=True),
                Param("scale", description="three factors like `2,2,2`", optional=True),
            ]),
    _action("set_material",
            "Give an object a material: a base colour, how metallic it is, how rough it is. "
            "Give at least one of the three. Cycles and EEVEE shade it; Workbench draws the "
            "colour flat, and the answer says so.",
            "standard", [
                Param("name", description="the object's exact name, as describe lists it"),
                Param("color", description="`#rrggbb`, or `r,g,b` with each from 0 to 1",
                      optional=True),
                Param("metallic", type="number", description="0 to 1", optional=True),
                Param("roughness", type="number", description="0 to 1", optional=True),
            ]),
    _action("set_camera",
            "Place the scene's camera and point it at something. A scene without a camera "
            "gets one.",
            "standard", [
                Param("location", description="three metres like `4,-4,3`", optional=True),
                Param("look_at", description="the point to aim at, like `0,0,0`",
                      optional=True),
            ]),
    _action("set_light",
            "Add a light of a kind, or change the scene's existing one of that kind.",
            "standard", [
                Param("kind", description="point, sun, spot or area"),
                Param("energy", type="number", description="brightness in the engine's own "
                      "unit; must not be negative", optional=True),
                Param("location", description="three metres like `2,2,4`", optional=True),
            ]),
    _action("import_model",
            "Import a model file into the scene. It takes .obj, .stl and .glb.",
            "standard", [
                Param("path", description="the file to import"),
            ], timeout=300.0),
    _action("set_render",
            "Change how the scene will be rendered: the engine, the size of the image, the "
            "samples per pixel. Give at least one of the three.",
            "standard", [
                Param("engine", description="cycles, eevee or workbench", optional=True),
                Param("resolution", description="like `1920x1080`", optional=True),
                Param("samples", type="integer", description="per pixel; Cycles and EEVEE "
                      "have samples, Workbench does not", optional=True),
            ]),
    _action("render",
            "Render the scene to a PNG and report the path, the seconds it took and the "
            "size of the file. With Cycles on a big scene this can take minutes.",
            "sensitive", [
                Param("output", description="where to write the PNG"),
            ], timeout=1800.0),
    _action("save",
            "Save this Blender file to a path. It overwrites whatever is already there.",
            "sensitive", [
                Param("path", description="where to write the .blend"),
            ], timeout=120.0),
    _action("open",
            "Open a .blend file in place of the current scene. Refused while the current "
            "scene has unsaved changes — save it or start a new one first.",
            "sensitive", [
                Param("path", description="the .blend file to open"),
            ], timeout=120.0),
    _action("run_python",
            "Run arbitrary Python inside this Blender, with `bpy` in scope. It can do "
            "anything a person at the keyboard can do, including deleting files this user "
            "can reach. Anything it does before an error is still done — it is not "
            "recoverable.",
            "dangerous", [
                Param("code", description="the Python to run"),
            ], timeout=300.0),
    _action("screenshot",
            "Save what the 3D viewport shows as a PNG. Needs a window with a viewport "
            "open; a background Blender draws nothing, and `render` is the honest answer "
            "there.",
            "standard", [
                Param("output", description="where to write the PNG"),
            ], timeout=60.0),
]


class Surface:
    """The RPC handler: `app.describe` and `app.act`, and nothing else.

    One Surface per serving Blender. The scene it reads and acts on is `scene.Scene`; the
    handover to Blender's main thread is the bridge it was given; the policy — grades,
    ceiling, revision guard, wording — is all here, and all of it ported.
    """

    def __init__(self, scene, bridge, app_id="blender", settings_path=None):
        self.scene = scene
        self.bridge = bridge
        self.app_id = app_id
        self.service_id = "app-%s" % app_id
        self.actions = ACTIONS
        self._by_name = {a.name: a for a in ACTIONS}
        self._settings_path = settings_path or SETTINGS_PATH
        self._counter_lock = threading.Lock()
        self._action_counter = 0

    # ── the wire handler ─────────────────────────────────────────────────────

    def handle(self, method, params):
        if method == "app.describe":
            return self.describe_json()
        if method == "app.act":
            return self.act(params if isinstance(params, dict) else {})
        raise wire.RpcError(
            wire.RPC_METHOD_NOT_FOUND,
            "unknown method `%s`; this app serves app.describe, app.act" % method)

    # ── describe ─────────────────────────────────────────────────────────────

    def describe_json(self):
        summary, state = self._turn(lambda: self.scene.snapshot(), DESCRIBE_TIMEOUT)
        return {
            "app": self.app_id,
            "summary": summary,
            "state": state,
            "revision": wire.revision(summary, state),
            "actions": [a.schema() for a in self.actions],
        }

    # ── act ──────────────────────────────────────────────────────────────────

    def act(self, params):
        name = params.get("action")
        name = name.strip() if isinstance(name, str) else ""
        if not name:
            raise wire.RpcError(wire.RPC_INVALID_PARAMS, "act needs a non-empty `action`")

        args = params.get("args") or {}
        if not isinstance(args, dict):
            raise wire.RpcError(wire.RPC_INVALID_PARAMS, "`args` must be an object")

        expect_revision = params.get("expect_revision")
        if expect_revision is not None and not isinstance(expect_revision, str):
            expect_revision = str(expect_revision)

        # 1. Unknown action.
        spec = self._by_name.get(name)
        if spec is None:
            known = ", ".join(a.name for a in self.actions)
            self._refuse("unknown action `%s`; this app offers: %s" % (name, known))

        # 2. A grade off the ladder is this file's bug, said in the runtime's words.
        if spec.permission not in LADDER:
            self._refuse(
                "CEILING: %s.%s is graded `%s`, which is not a level this OS defines "
                "(safe < standard < sensitive < dangerous), so it was not run."
                % (self.app_id, name, spec.permission))

        # 3. Above the machine's ceiling: policy, read fresh per call.
        ceiling = self.configured_ceiling()
        if LADDER.index(spec.permission) > LADDER.index(ceiling):
            self._refuse(
                "CEILING: %s.%s is graded `%s`, above this machine's `%s` ceiling "
                "(`tool_permission` in ~/.config/yantrik/settings.yaml), so it was not run. "
                "An action at that grade needs a person to authorise it directly — raise "
                "the ceiling in Settings if that is the intent."
                % (self.app_id, name, spec.permission, ceiling))

        # 4. Missing required arguments, in schema order.
        for p in spec.params:
            if not p.optional and p.name not in args:
                self._refuse("`%s` needs argument `%s`" % (name, p.name))

        # 5. Arguments the action does not take. (Sorted, because the runtime's args map is
        # a serde_json BTreeMap and reports the first unexpected key in sorted order.)
        known_params = {p.name for p in spec.params}
        for key in sorted(args):
            if key not in known_params:
                if spec.params:
                    self._refuse("`%s` has no argument `%s`; it takes: %s"
                                 % (name, key, ", ".join(p.name for p in spec.params)))
                else:
                    self._refuse("`%s` takes no arguments, but `%s` was given"
                                 % (name, key))

        # 6 + 7. The guard, the handler and the post-snapshot, as ONE main-thread turn.
        def turn():
            summary, state = self.scene.snapshot()
            current = wire.revision(summary, state)
            if expect_revision is not None and expect_revision != current:
                return ("stale", current, summary)
            try:
                result = self.scene.run(name, args)
            except Refusal as r:
                self.scene.notice = str(r)
                return ("refused", str(r))
            self.scene.notice = ""
            summary, state = self.scene.snapshot()
            return ("ok", result, summary, state)

        outcome = self._turn(turn, spec.timeout)

        if outcome[0] == "stale":
            _, current, summary = outcome
            self._refuse(
                "STALE: this app is at revision %s and you acted on %s. It now reports: %s. "
                "Read it again before deciding." % (current, expect_revision, summary))
        if outcome[0] == "refused":
            self._refuse(outcome[1])

        _, result, summary, state = outcome
        return {
            "app": self.app_id,
            "action_id": self._next_action_id(),
            "accepted": True,
            "settled": not spec.deferred,
            "result": result,
            "revision": wire.revision(summary, state),
            "summary": summary,
            "state": state,
        }

    # ── pieces ───────────────────────────────────────────────────────────────

    def _refuse(self, sentence):
        """Every refusal in this dispatch travels as -32602, the runtime's choice: a
        refused action is invalid params from the wire's point of view, and `yos` reads the
        message as the refusal it is."""
        raise wire.RpcError(wire.RPC_INVALID_PARAMS, sentence)

    def _turn(self, fn, timeout):
        """Run `fn` on Blender's main thread, or say the app did not answer."""
        try:
            return self.bridge.submit(fn, timeout=timeout)
        except BridgeTimeout:
            raise wire.RpcError(
                wire.RPC_TRANSPORT_ERROR,
                "app did not answer within %ds" % int(timeout))

    def _next_action_id(self):
        with self._counter_lock:
            self._action_counter += 1
            return "%s#%d" % (self.service_id, self._action_counter)

    def configured_ceiling(self):
        """`ceiling_from()` in Python: the first `tool_permission:` line wins; a value off
        the ladder, a missing file or an unreadable one all fall back to `sensitive`."""
        try:
            with open(self._settings_path, "r", encoding="utf-8") as f:
                for line in f:
                    line = line.strip()
                    if line.startswith("tool_permission:"):
                        value = line.split(":", 1)[1].strip().strip("\"'").strip()
                        return value if value in LADDER else DEFAULT_CEILING
        except OSError:
            pass
        return DEFAULT_CEILING
