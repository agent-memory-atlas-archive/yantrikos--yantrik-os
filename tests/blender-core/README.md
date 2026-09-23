# blender-core

The Blender control surface's own tests: the addon in `apps/blender/addon/yantrik_blender`,
driven against a fake `bpy`, with no Blender installed and no display. The addon is built on
the Python surface SDK (`sdk/python/yantrik_surface`), so the wire, the dispatch order, the
gate and the refusal wording pinned here are the SDK's, met through Blender; the SDK's own
conformance suite is `sdk/python/tests`.

Unlike the other `tests/*-core` directories this one is Python, not a Rust crate. The code
under test is Python living inside somebody else's program; the crate shape would have
meant testing a transcript of the addon rather than the addon.

Run it:

```sh
python3 -m unittest discover -s tests/blender-core
```

## What each file pins

* **`fake_bpy.py`** — the stand-in module. Models what the addon observes (objects,
  materials with a Principled BSDF, lights, cameras, the operators it calls, an RNA-style
  engine enum that raises on unknown ids) and nothing else. `render.render` writes a real
  file, so the addon's "did the render actually produce a PNG" check runs against a real
  filesystem.
* **`test_wire.py`** — the socket and the hash. Includes the cross-language revision
  vector: the hex asserted here is the hex
  `crates/yantrik-ipc-contracts/src/control_surface.rs` asserts for the identical summary
  and state, so the Python port of `View::revision()` and the Rust original cannot drift
  without one of the two tests failing. Also a real roundtrip over a real unix socket:
  framing, `rpc.ping`, error codes, 0600 on the node, stale-socket replacement.
* **`test_scene.py`** — every action against the fake: happy paths, refusal sentences in
  full, the validate-before-touch discipline (a refused action left nothing behind), the
  look-at quaternion against a known-good vector, and the honest-failure checks (a render
  that wrote nothing is not reported as a render).
* **`test_startup.py`** — how the surface comes up in a window: the splash preference is
  off before the socket and the pump exist (so the first-run Quick Setup never sits over the
  viewport the surface drives), untouched headless, and a `bpy` without the preference does
  not take the surface down. Runs the real `_Session` with its socket in a private dir.
* **`test_dispatch.py`** — the surface layer, which must be indistinguishable from
  `yantrik-app-runtime::control`: dispatch order, exact refusal wording (unknown action,
  missing/unexpected arguments, CEILING, STALE), the action table's grades, the ceiling
  read from a settings file, envelope keys, action ids, the notice that says a failure
  twice, and bridge-timeout mapping to the transport's `-32000`.

## What these tests cannot say

A green run says the addon's logic is right. It cannot say Blender agrees — that the real
`bpy.ops` accept these calls and the real scene behaves like the fake. That half is the
headless verification in `design/blender-surface-2026-09-22.md`: a real `blender -b` run
driven through the real `yos`, ending in a rendered PNG measured on disk.
