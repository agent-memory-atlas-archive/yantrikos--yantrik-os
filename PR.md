# PR title

Blender answers to the desktop: a mind can build, render and save a scene through graded actions

# PR body

## What

Blender becomes an app of this desktop — not a Slint app, and not a fork: a Python addon
inside somebody else's program (`apps/blender/addon/yantrik_surface/`) that binds
`$XDG_RUNTIME_DIR/yantrik/app-blender.sock` and speaks the same JSON-RPC line protocol every
other app speaks. `yos describe blender` reads the scene — objects, camera, render settings,
the unsaved flag, the last render; `yos act blender …` builds it: 14 actions from
`add_primitive` to `render`, each with a grade, a purpose in the app's own words, and
refusals that say what is true. The launcher opens it with the addon attached
(`blender --python <bootstrap>`), the release stages the addon under
`/opt/yantrik/share/blender/`, and a conformance probe holds the whole claim against the
world with a rendered PNG as the witness at the centre.

This is the first real version of what a film-verdict reviewer said must exist before the
3D/film story is shown to anyone: a real Blender surface, persistence, a render path, and a
verification harness.

## Why

The product bet: people leave coding tools for Blender/3D and video, and this OS's advantage
is not a better renderer — it is the control surface. A mind should build and render a scene
from a sentence the way it already builds a slide deck, with every action graded and visible
and anything unrecoverable stopping at a card only a person can answer. A wrapper app beside
Blender could never keep that promise, because the truth (the scene, the dirty flag, whether
a file was actually written) lives inside Blender's process — so the surface is an addon,
and its dispatch is a byte-pinned port of `yantrik-app-runtime::control` rather than an
opinion of its own: same check order, same sentences, same error codes, and one revision
vector asserted in two languages so the port cannot drift from the original without a test
failing in one of them.

## Files

- `apps/blender/bootstrap.py` — what the launcher runs: `blender --python <this>`.
- `apps/blender/addon/yantrik_surface/` — `wire.py` (socket, framing, revision hash),
  `bridge.py` (the hop onto Blender's main thread), `scene.py` (the scene and the actions),
  `surface.py` (the ported dispatch: grades, ceiling, STALE, vocabulary), `__init__.py`
  (session lifecycle, timers pump windowed / foreground loop headless, addon register hooks).
- `crates/yantrik-ui/src/wire/dock.rs` — `Launch::Blender` route, both-halves availability,
  `blender_bootstrap()` discovery, spawn with the addon as the argument.
- `crates/yantrik-app-runtime/src/control.rs` — the `blender` row in SURFACES (the name
  table from PR #64; the route test fails without it).
- `crates/yantrik-ipc-contracts/src/control_surface.rs` — the shared revision vector test.
- `apps/desktop-files/yantrik-blender.desktop` — the window's entry, running the same pair.
- `deploy/yantrik-os/build-release.sh` — stages bootstrap + addon under `share/blender/`.
- `tests/blender-core/` — 106 tests over `fake_bpy.py`: every action's validation, every
  refusal sentence in full, the wire, the bridge, the pinned hash. New CI step runs them.
- `tests/conformance/probes/blender.py` — the probe: three machine shapes (not installed /
  already open / closed), read-only against a stranger's scene, render bytes checked
  against disk, kills only what it started (by pattern that matches the headless fallback
  too), left-as-found asserted as "nothing answers".
- `design/blender-surface-2026-09-22.md` — the design doc, with every command and output
  below in full.

## How verified

- `python3 -m unittest discover -s tests/blender-core` — 106 tests, OK (no Blender, no
  display needed; CI now runs them).
- `cargo test --release -p yantrik-ipc-contracts` (incl.
  `revision_vector_shared_with_the_python_port`), `-p yantrik-app-runtime`, and
  `-p yantrik-ui --bin yantrik-ui` — 297 passed, 0 failed.
- CI's Python selftests, for regression: `yos-mcp-selftest.py`, `yos-selftest.py`,
  `harnesses/tests` (176 tests) — all green.
- One real headless run in WSL (Blender 4.0.2), through the real `deploy/yantrik-os/yos`:
  `blender -b --python apps/blender/bootstrap.py` → `describe` → `add_primitive
  kind=monkey` → `set_material` → `set_camera` → `render output=/tmp/monkey.png` →
  **1,025,106 bytes, `file` says PNG 1920×1080 RGBA**, 8.21 s, and `last_render` in the
  state agrees with the disk. Persistence: `save` wrote a 924,672-byte `BLENDER-v400` file,
  `new_scene` emptied the scene, `open` brought it back at the *same revision hex* as the
  save. Guards live: `run_python` refused by CEILING (nothing ran), STALE refused with the
  current world in the sentence, headless `screenshot` refused naming `render`, a missing
  object refused and the failure said twice (refusal + `notice`). `import_model` with a real
  cube `.obj` (1 object added) and `delete_object` verified live too. The Blender was
  stopped by its recorded pid — never by pattern.
- One real windowed run (WSLg): the dock's exact command `blender --python <bootstrap>`,
  surface up "(14 actions, windowed)", actions served through the `bpy.app.timers` pump,
  and `screenshot` capturing the actual viewport — 1,345,556-byte PNG in 0.58 s.
- The MCP bridge live: an `os_describe blender` call through `yos-mcp` (with `YOS_BIN` at
  this tree's `yos`) returned the action table with the grades attached —
  `render [sensitive]`, `save/open [sensitive]`, `run_python [dangerous]` — which is what
  `os_act blender render` reaches a mind with.

## Not verified

- The conformance probe has not run on the live VM: it needs the release staged under
  `/opt/yantrik`, and this build's charter forbade installing anything there. What it would
  measure was measured directly instead, in both modes.
- The dock route was not clicked end-to-end (no running desktop shell in this environment);
  the exact command it builds was run directly, and its resolution/availability logic is
  covered by the dock unit tests.
- Only Blender 4.0.2 was exercised; the EEVEE_NEXT rename is carried in the engine table
  but not run against a 4.2+ build.
- No animation/timeline surface — still scenes only, by scope, stated in the design doc.

## What a skeptic will say

"It's a Python script with a prompt in front." The answer is the surface, and it is
measurable: a grade refusal that fires *before* arguments are read, a revision guard that
refuses stale reads and quotes the present, an `unsaved` flag tracked honestly because
Blender's own lies headless (verified: stuck True), a render that is only reported once the
file exists on disk with the bytes the answer claims, `delete_object`/`new_scene`/
`run_python` purposes carrying the unrecoverable phrases the shell draws its red card line
from, and the same `yos-mcp` bridge every other app goes through — `os_act blender render`
with `sensitive` attached, `run_python` with `dangerous`, which asks a person in every mode.
The port-drift worry is answered by a vector asserted in two languages and 106 tests that
pin refusal sentences in full. The rest — EEVEE-on-WSL-GPU vs a GPU-less VM, 4.0.2 vs 4.5,
no dock click — is listed above rather than buried, and the probe is written for the
machine this is not.
