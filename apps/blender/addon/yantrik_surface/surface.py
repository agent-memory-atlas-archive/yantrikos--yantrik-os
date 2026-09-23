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
  3a. a grant, when the call carries one, is spent through the shell (said as GRANT when it
     does not hold) — once the ceiling has passed, never before, as the runtime's RPC thread
     spends it (#154);
  3b. an action above what the desktop's mind mode runs unasked, with no grant and no session
     rule (a policy, said as GRANT — issue #116), or one whose own description says it cannot
     be undone, in any mode but bypass (`gate::decide`; `decide` below replays
     deploy/yantrik-os/surface-vectors.json in tests/blender-core/test_vectors.py);
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

The mode is read per call from `mind-mode.json` beside it, which the shell writes, defaulting
to `ask` exactly like the runtime. This app is where issue #116 was found: `blender.render`
through the MCP bridge raised a card, and through `yos act` it ran in 1.72 s with nobody asked,
because the mode lived only in the bridge. It lives in every dispatch now, this one included.
"""

import json
import os
import socket
import threading
import time

from . import wire
from .bridge import BridgeTimeout
from .scene import Refusal

LADDER = ("safe", "standard", "sensitive", "dangerous")
DEFAULT_CEILING = "sensitive"
# The surface protocol this describe speaks (docs/surface-protocol.md), as `control_surface::PROTOCOL`.
PROTOCOL = 1
# `gate::UNRECOVERABLE_PHRASES`: the wording that makes an action's own description a promise that
# it cannot be taken back. Same seven, same order; the vectors carry the list and a test compares.
UNRECOVERABLE_PHRASES = ("not recoverable", "cannot be undone", "can't be undone", "irreversible",
                         "permanently", "permanent", "no undo")
SETTINGS_PATH = os.path.join(os.path.expanduser("~"), ".config", "yantrik", "settings.yaml")

# The mode, as `control::MODES` / `MODE_FILE` / `DEFAULT_MODE` / `SOCKET_FLOOR` have it: what each
# mode runs without a grant, the file the shell publishes it in, what an unreadable file means,
# and the grade every mode runs unasked on a socket (the desktop's own processes call `standard`
# actions to work at all — see the runtime's `SOCKET_FLOOR` for why plan's `safe` is the
# bridge's to enforce, not the dispatch's).
MODES = {"plan": "safe", "ask": "standard", "auto": "sensitive", "bypass": "dangerous"}
MODE_FILE = "mind-mode.json"
DEFAULT_MODE = "ask"
SOCKET_FLOOR = "standard"
# One hop to the shell's UI thread and back, as the runtime's `GRANT_ROUNDTRIP`.
GRANT_ROUNDTRIP = 5.0

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
            "Add a light of a kind, or change the scene's existing one of that kind. Cycles "
            "and EEVEE light the scene with it; Workbench lights the scene itself, and the "
            "answer says so.",
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

    def __init__(self, scene, bridge, app_id="blender", settings_path=None, mode_path=None,
                 spend_grant=None):
        self.scene = scene
        self.bridge = bridge
        self.app_id = app_id
        self.service_id = "app-%s" % app_id
        self.actions = ACTIONS
        self._by_name = {a.name: a for a in ACTIONS}
        self._settings_path = settings_path or SETTINGS_PATH
        self._mode_path = mode_path or os.path.join(
            os.path.dirname(self._settings_path), MODE_FILE)
        # How a grant is spent: through the shell's `consume_approval`, unless a test hands in
        # a stand-in for the shell's store.
        self._spend_grant = spend_grant or spend_through_shell
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
            "protocol": PROTOCOL,
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

        grant = params.get("grant")
        grant = grant.strip() if isinstance(grant, str) else ""

        # 1. Unknown action.
        spec = self._by_name.get(name)
        if spec is None:
            known = ", ".join(a.name for a in self.actions)
            self._refuse("unknown action `%s`; this app offers: %s" % (name, known))

        # 2 + 3. A grade off the ladder (this file's bug) or above the machine's ceiling (policy,
        # read fresh per call), said in the runtime's words.
        ceiling = self.configured_ceiling()
        refusal = within_ceiling(self.app_id, name, spec.permission, ceiling)
        if refusal:
            self._refuse(refusal)

        # 3a. A grant, spent once the ceiling has passed and before the main thread is reached
        # — as the runtime's RPC thread spends it (`Authority::spend` in
        # `yantrik_ipc_transport::gate`). Not earlier: a grant spent on an act the ceiling then
        # refused was a person's Allow used up on nothing (#154). Not later: a grant checked
        # after the dispatch had begun would be a window in which one grant covers two calls.
        granted = False
        if grant:
            try:
                self._spend_grant(grant, self.app_id, name, args)
            except GrantRefused as why:
                self._refuse(
                    "GRANT: `%s` does not authorise %s.%s — %s Nothing was run; a grant covers "
                    "one action, once, with the arguments the person was shown."
                    % (grant, self.app_id, name, why))
            granted = True

        # 3b. Above what the mode runs unasked, or said by its own description to be beyond
        # undoing, with no grant and no session rule that covers it: the refusal that says how to
        # get one. After the ceiling — nothing reaches past that — and before the arguments, as
        # the ceiling is.
        mode, rules = self.configured_mode()
        refusal = decide(self.app_id, name, spec.permission, spec.description, ceiling, mode,
                         rules, granted)
        if refusal:
            self._refuse(refusal)

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

    def configured_mode(self, now=None):
        """`configured_mode()` / `mode_from()` in Python: `(mode, session_rules)`.

        Anything unreadable is `ask`, never something looser. A bypass whose deadline has
        passed reads as the mode before it, so a shell that died mid-bypass does not leave
        this app trusting it past the minute the person was promised.
        """
        try:
            with open(self._mode_path, "r", encoding="utf-8") as f:
                text = f.read()
        except OSError:
            return DEFAULT_MODE, set()
        return mode_from(text, time.time() if now is None else now)

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


# ── the mode, and the grant ──────────────────────────────────────────────────


class GrantRefused(Exception):
    """The shell would not spend a grant; the message is the shell's own sentence."""


def mode_from(text, now):
    """`control::mode_from` in Python: what the shell wrote, read the way every app reads it."""
    try:
        doc = json.loads(text)
    except ValueError:
        return DEFAULT_MODE, set()
    if not isinstance(doc, dict):
        return DEFAULT_MODE, set()
    mode = doc.get("mode") if isinstance(doc.get("mode"), str) else ""
    if mode not in MODES:
        mode = DEFAULT_MODE
    if mode == "bypass":
        until = doc.get("bypass_expires_unix")
        if isinstance(until, int) and not isinstance(until, bool) and now >= until:
            previous = doc.get("previous")
            mode = previous if previous in MODES and previous != "bypass" else DEFAULT_MODE
    rules = set()
    for rule in doc.get("session_rules") or []:
        if isinstance(rule, dict) and isinstance(rule.get("app"), str) \
                and isinstance(rule.get("action"), str):
            rules.add((rule["app"], rule["action"]))
    return mode, rules


def unrecoverable(purpose):
    """`gate::unrecoverable` in Python: does the action's own sentence say it cannot be undone?"""
    lower = (purpose or "").lower()
    return any(phrase in lower for phrase in UNRECOVERABLE_PHRASES)


def within_ceiling(app, action, graded, ceiling):
    """`gate::within_ceiling` in Python: the CEILING refusal, or None."""
    if graded not in LADDER:
        return ("CEILING: %s.%s is graded `%s`, which is not a level this OS defines "
                "(safe < standard < sensitive < dangerous), so it was not run."
                % (app, action, graded))
    cap = ceiling if ceiling in LADDER else DEFAULT_CEILING
    if LADDER.index(graded) > LADDER.index(cap):
        return ("CEILING: %s.%s is graded `%s`, above this machine's `%s` ceiling "
                "(`tool_permission` in ~/.config/yantrik/settings.yaml), so it was not run. "
                "An action at that grade needs a person to authorise it directly — raise "
                "the ceiling in Settings if that is the intent."
                % (app, action, graded, ceiling))
    return None


def decide(app, action, graded, purpose, ceiling, mode, rules, granted):
    """`gate::decide` in Python: the refusal for this call, or None when it may run.

    The ceiling, then the mode — with the grant, the session rules and the action's own
    description. Replayed against every vector in deploy/yantrik-os/surface-vectors.json.
    """
    refusal = within_ceiling(app, action, graded, ceiling)
    if refusal:
        return refusal
    if granted:
        return None
    level = LADDER.index(graded)
    irreversible = level > 0 and unrecoverable(purpose)
    allows = LADDER.index(MODES.get(mode, "standard"))
    asks = allows < len(LADDER) - 1 and (
        irreversible or level > max(allows, LADDER.index(SOCKET_FLOOR)))
    if not asks:
        return None
    # A session rule covers its own action — never one that cannot be undone, and nothing in
    # plan mode, which raises no card and so has no standing answers.
    if allows > 0 and not irreversible and (app, action) in rules:
        return None
    return grant_refusal(app, action, graded, mode, irreversible)


_HOW = ("Ask the shell for approval first (`request_approval` with this app, action and these "
        "exact arguments, poll `approval_status`, then send the granted request_id as `grant` on "
        "app.act — `yos act` does all of that for you), or have the person at the machine press "
        "Allow when the card appears.")
_PLAN = ("Say what you would do and let the person decide; they switch the mode from the chip in "
         "the status bar.")
_FINAL = "its own description says it cannot be undone"


def grant_refusal(app, action, graded, mode, irreversible=False):
    """`gate::grant_refusal` in Python, to the punctuation."""
    if mode == "plan" and not irreversible:
        return ("GRANT: %s.%s is graded `%s` and this machine is in plan mode, which raises no "
                "card for anything above `%s` — so it was not run. %s"
                % (app, action, graded, SOCKET_FLOOR, _PLAN))
    if mode == "plan":
        return ("GRANT: %s.%s is graded `%s` and %s, and this machine is in plan mode, which "
                "raises no card for that — so it was not run. %s"
                % (app, action, graded, _FINAL, _PLAN))
    if irreversible:
        return ("GRANT: %s.%s is graded `%s` and %s, and this machine is in %s mode, which asks "
                "before anything that cannot be undone — so it was not run. %s"
                % (app, action, graded, _FINAL, mode, _HOW))
    allowed = LADDER[max(LADDER.index(MODES.get(mode, "standard")), LADDER.index(SOCKET_FLOOR))]
    return ("GRANT: %s.%s is graded `%s` and this machine is in %s mode, which runs nothing "
            "above `%s` without asking — so it was not run. %s"
            % (app, action, graded, mode, allowed, _HOW))


def spend_through_shell(grant, app, action, args):
    """Burn `grant` for exactly `app.action(args)` through the shell's `consume_approval`.

    The runtime's `spend_grant`: the check is the shell's — granted, unspent, unexpired, bound
    to this app, this action and these arguments — and a refusal carries the shell's sentence.
    """
    try:
        reply = wire.call_once(wire.default_socket_path("shell"), "app.act", {
            "action": "consume_approval",
            "args": {"request_id": grant, "app": app, "action": action, "args_json": args},
        }, timeout=GRANT_ROUNDTRIP)
    except (OSError, ValueError) as e:
        raise GrantRefused("the shell could not be asked to spend it (%s)." % e)
    if "error" in reply:
        raise GrantRefused((reply["error"] or {}).get("message", "the shell refused it."))
