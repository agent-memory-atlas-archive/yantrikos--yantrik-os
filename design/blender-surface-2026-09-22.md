# Blender answers to the desktop: a mind can build, render and save a scene through graded actions

The product bet is that people leave coding tools for Blender and video, and that this OS's
advantage there is not a better renderer — it is the control surface. A mind should build and
render a scene from a sentence the same way it already builds a slide deck, with every action
graded and visible, and a person asked about anything that cannot be taken back. A film-verdict
from a reviewer said this must not be shown until a *real* Blender surface, persistence, a
render path and a verification harness exist. This is the first real version of all four.

Blender is not ours. It is somebody else's program, like the browser. What makes it an app of
this desktop rather than a window a mind can only photograph is a Python addon inside it
(`apps/blender/addon/yantrik_surface/`) that binds
`$XDG_RUNTIME_DIR/yantrik/app-blender.sock` and answers `app.describe` and `app.act` in the
same newline-delimited JSON-RPC every other app speaks. `yos describe blender` reads a scene
the way `yos describe notes` reads a note; `yos act blender render output=/tmp/x.png` draws
it; `yos-mcp` exposes both to a mind with the grades attached.

## Why this shape

**An addon inside Blender, not a wrapper app beside it.** A separate Rust app could start
Blender and pipe scripts into its stdin, but it could never *read* the scene honestly: object
lists, the dirty flag, the active camera, whether a render actually wrote a file — all of that
lives inside Blender's process. The surface's whole promise is that it says what is true, and
the only place that can see the truth is inside. So the surface is an addon, started by a
bootstrap (`blender --python .../bootstrap.py`), which is also how a person's Blender becomes
the desktop's Blender without installing anything into Blender's own addon directories.

**A port of the runtime's dispatch, not a client of it.** `surface.py` is
`yantrik-app-runtime::control`'s dispatch translated to Python: same order of checks (unknown
action → grade off the ladder → above the ceiling → missing argument → unexpected argument →
STALE → handler), same refusal sentences, same error codes (-32602 for every app refusal,
-32000 when the app fails to answer), same envelope keys. A Python addon inside somebody
else's program cannot link a Rust crate, so the two implementations are held together by a
shared vector instead: `wire.py` recomputes the FNV-1a revision hash the Rust
`View::revision()` computes, and both test suites assert the *same hex* for the *same*
summary and state (`revision_vector_shared_with_the_python_port` in
`crates/yantrik-ipc-contracts/src/control_surface.rs`, `test_wire.py` beside the addon). If
either side changes what it hashes, one of the two tests fails with the vector in the
message.

**Threading: socket on one thread, scene on Blender's.** `bpy` is not thread-safe, so the
socket is served on a thread of its own and every read or change of the scene crosses a
bridge onto Blender's main thread and is waited for — the same hop the Rust apps make with
`slint::invoke_from_event_loop`. Who pumps the bridge differs by mode, because the two modes
have different main threads: windowed, a `bpy.app.timers` callback pumps between Blender's
own event-loop turns; under `blender -b` there is no event loop, so the bootstrap's `start()`
becomes the main loop. The guard, the handler and the post-action snapshot cross as ONE job,
so no other caller can move the scene between "is this revision current" and "here is what
your action did".

**Dirtiness is tracked, because Blender's own flag lies headless.** `bpy.data.is_dirty` is
stuck True in `blender -b` — verified on the live 4.0.2 run behind this document (True at
start, after save, after open). An addon that believed it would report every headless scene
as unsaved and refuse every `open` as dirty-guarded, including straight after the `save` its
own refusal recommends. So the addon tracks dirtiness per action: mutating actions set the
flag, `new_scene`/`save`/`open` clear it, and a *refused* action leaves it exactly as it was
— a refusal changed nothing, and claiming otherwise would be the same lie the rest of the
surface refuses to tell. Where there is a window, Blender's `is_dirty` is OR'd in, because a
person at the keyboard can edit behind the surface's back and there the flag is trustworthy.

**The launcher route checks both halves.** `Launch::Blender` in `wire/dock.rs` opens Blender
through the shell's one launcher — registry, reaper, session environment — with the addon as
its argument. Availability refuses in words that name the missing half: no `blender` binary
("blender"), or binary without addon ("the Yantrik addon for Blender
(share/blender/bootstrap.py)"), because the fix is different for each. A Blender started
without `--python` would still be Blender, but a window a mind can only photograph, and the
route refuses to quietly open the lesser thing.

## The surface

`describe` reports: scene name, file path (or null with `unsaved scene` in the summary),
objects (name, type, location, dimensions — capped at 50 with an uncapped `objects_total`),
active camera, render settings (engine, resolution, samples, output), the tracked `unsaved`
flag, `last_render` ({path, seconds, bytes} of the last render this process did), the last
refusal as `notice`, and `background`. The summary is one line:
`Blender — "monkey.blend", 4 objects, EEVEE 1920x1080, unsaved`.

Every action, with the grade it ships at and the purpose a person reads on a card and a mind
reads in `describe`:

| action | grade | purpose (abridged; the full sentence is in `surface.py`) |
|---|---|---|
| `new_scene()` | standard | Throw the current scene away and start an empty one. Anything unsaved in it is lost, and in a background Blender there is no undo to argue with. |
| `add_primitive(kind, name?, location?, scale?)` | standard | Add a mesh primitive to the scene, where you say, at the size you say. |
| `delete_object(name)` | standard | Delete an object from the scene. Blender's own undo can bring it back in a window; past that undo it is not recoverable, and a background Blender has no undo at all. |
| `transform(name, location?, rotation?, scale?)` | standard | Move, rotate or scale an object; rotation in degrees. |
| `set_material(name, color?, metallic?, roughness?)` | standard | Give an object a material: base colour, metallic, roughness. |
| `set_camera(location?, look_at?)` | standard | Place the scene's camera and point it at something. A scene without a camera gets one. |
| `set_light(kind, energy?, location?)` | standard | Add a light of a kind, or change the scene's existing one of that kind. |
| `import_model(path)` | standard | Import a model file (.obj, .stl, .glb) into the scene. |
| `set_render(engine?, resolution?, samples?)` | standard | Change how the scene will be rendered. |
| `render(output)` | sensitive | Render the scene to a PNG and report the path, the seconds it took and the size of the file. |
| `save(path)` | sensitive | Save this Blender file to a path. It overwrites whatever is already there. |
| `open(path)` | sensitive | Open a .blend in place of the current scene. Refused while the current scene has unsaved changes. |
| `run_python(code)` | dangerous | Run arbitrary Python inside this Blender, with `bpy` in scope. … Anything it does before an error is still done — it is not recoverable. |
| `screenshot(output)` | standard | Save what the 3D viewport shows as a PNG. A background Blender draws nothing, and `render` is the honest answer there. |

Why these grades: reads and in-scene edits a person can undo in the app are `standard`;
things that outlive the turn or touch the filesystem — a render, a save, an open that
replaces the scene — are `sensitive`; `run_python` is `dangerous` because arbitrary code can
reach anything this user can reach, and a mind must not get it without a card. The card
sentences matter as much as the grades: the shell and the bridge both scan the published
purpose for the unrecoverable phrases ("not recoverable", "no undo", …), so `delete_object`
and `new_scene` stop at an approval card in *every* mode, and `run_python`'s purpose now ends
"it is not recoverable" so its card carries the red warning line too.

Honesty disciplines the actions keep, all pinned by tests and several verified live below:

- **Validate, then touch.** Every argument is parsed and every precondition checked before
  anything in the scene is changed, so a refusal cannot leave the scene half-mutated with a
  sentence explaining only the half that failed (`set_render` checks the samples-vs-Workbench
  conflict *before* switching the engine; the test asserts the engine is still CYCLES after
  the refusal).
- **Check the claim against the world.** A render is not reported until the file exists and
  is non-empty — the bytes come from the filesystem, not from the engine's optimism. A delete
  is not reported until the object is gone from the list. An import is not reported until the
  count of objects that appeared is known.
- **Say what you cannot do, and what to do instead.** A background Blender refuses
  `screenshot` naming `render` as the honest alternative (a real `blender -b` carries a
  phantom window with a viewport in it — found in the live run — so the refusal keys off
  `app.background`, not off the window list).
- **The failure is said twice**: once as the refusal, once as `notice` in the state, until
  the next success clears it.

## What is real in this PR, and what is not yet

Real: the addon and its wire (socket, framing, revision, ceiling, guard — a port of the
runtime, pinned against it by the shared vector); the launcher route, availability check and
`.desktop` entry; the SURFACES name-table row; release staging of the addon under
`/opt/yantrik/share/blender/`; 106 unit tests over a fake `bpy`; a conformance probe; the
live headless *and* windowed runs below, through the real `yos` and the real `yos-mcp`.

Not yet:

- **No animation, no modifiers, no geometry nodes, no compositor.** The surface builds and
  renders still scenes. `transform` places keyframe-less objects; a film needs timelines and
  that is a later PR's surface, not this one's.
- **`import_model` takes .obj/.stl/.glb only** — the formats whose importers ship inside
  Blender itself. No FBX (its importer is a separate licence).
- **One socket, one Blender.** The addon binds `app-blender.sock`; a second Blender would
  take the socket over from the first. Multi-instance (per-scene socket names) is not in this
  version, and the conformance probe deliberately refuses to open a second instance.
- **No EEVEE-Next detection beyond the engine id list.** 4.2 renamed the engine; the addon
  tries both ids and reports whichever took, but only 4.0.2 was exercised live here.
- **The conformance probe has not run on the live VM.** It needs the release staged under
  `/opt/yantrik` and the charter for this build forbade installing anything there; what the
  probe would measure was measured directly instead (below), in both modes.

## How it is verified

### 1. Unit tests, no Blender and no display needed

```
$ python3 -m unittest discover -s tests/blender-core -v
…
Ran 106 tests in 3.173s

OK
```

`tests/blender-core` drives the whole dispatch layer against `fake_bpy.py`, which models what
the addon *observes* — objects, materials with a Principled BSDF, lights, cameras, an
RNA-style engine enum that raises on unknown ids — and writes a real file for `render`, so
the "did a PNG actually appear" check runs against a real filesystem. The tests pin every
refusal sentence in full (not fragments — a paraphrase is a different promise), the
dispatch order, the grades, the ceiling read from a settings file, STALE, the envelope keys,
the look-at quaternion against a known-good vector, and the revision hash against the Rust
test's hex. CI now runs them (a step was added to `.github/workflows/ci.yml`; a guard nobody
runs is a comment).

### 2. The Rust side

```
$ cargo test --release -p yantrik-ipc-contracts revision_vector
test control_surface::tests::revision_vector_shared_with_the_python_port ... ok
$ cargo test --release -p yantrik-app-runtime control        # 31 passed
$ cargo test --release -p yantrik-ui --bin yantrik-ui        # 297 passed; 0 failed
```

including `every_launchable_name_reaches_a_surface` (the route resolves `blender` to a
surface in the apps' own name table) and `the_release_script_excludes_every_shelved_binary`
(the staging block added to `build-release.sh` does not disturb what the dock tests read out
of it). `bash -n` parses the release script; nothing added carries CRLF.

The CI-run Python selftests, for regression:

```
$ python3 deploy/yantrik-os/yos-mcp-selftest.py     → all checks passed
$ python3 deploy/yantrik-os/yos-selftest.py         → all checks passed
$ python3 -m unittest discover -s harnesses/tests   → Ran 176 tests … OK
```

### 3. One real headless run, through the real `yos` (Blender 4.0.2, WSL Ubuntu)

Exactly what was run, in order:

```
$ setsid blender -b --python apps/blender/bootstrap.py > /tmp/blender-headless.log 2>&1 &
$ cat /tmp/blender-headless.log
[yantrik] control surface on /mnt/wslg/runtime-dir/yantrik/app-blender.sock (14 actions, background)

$ python3 deploy/yantrik-os/yos describe blender
Blender — unsaved scene, 3 objects, EEVEE 1920x1080
revision: a6c86dbcc40c0fe5
{ "scene": "Scene", "file": null, "unsaved": false,
  "objects": [{"name": "Cube", …}, {"name": "Light", …}, {"name": "Camera", …}],
  "objects_total": 3, "camera": {"name": "Camera", "location": [7.359, -6.926, 4.958]},
  "render": {"engine": "eevee", "resolution": "1920x1080", "samples": 64, "output": "/tmp/"},
  "last_render": null, "notice": "", "background": true }
  … 14 actions with grades and purposes …

$ python3 deploy/yantrik-os/yos act blender add_primitive kind=monkey
accepted: True, settled: True        → {"object": "Suzanne", "type": "MESH", …}

$ python3 deploy/yantrik-os/yos act blender set_material name=Suzanne color=#ff8800 metallic=0.2 roughness=0.4
accepted: True, settled: True        → {"material": "Suzanne Material",
                                         "color": [1.0, 0.533, 0.0, 1.0],
                                         "metallic": 0.2, "roughness": 0.4}

$ python3 deploy/yantrik-os/yos act blender set_camera location=4,-4,3 look_at=0,0,0
accepted: True, settled: True        → {"camera": "Camera", "location": [4.0, -4.0, 3.0], …}

$ python3 deploy/yantrik-os/yos act blender render output=/tmp/monkey.png
accepted: True, settled: True
{ "path": "/tmp/monkey.png", "seconds": 8.21, "bytes": 1025106 }

$ file /tmp/monkey.png
/tmp/monkey.png: PNG image data, 1920 x 1080, 8-bit/color RGBA, non-interlaced
$ ls -la /tmp/monkey.png
-rw-r--r-- 1 yantrik yantrik 1025106 Sep 22 14:32 /tmp/monkey.png     # > 10 KB, and real
```

The render engine was EEVEE on WSLg's D3D12 GPU (`/dev/dxg`); Cycles on CPU is the fallback
anywhere else, and the render refusal names that trade when an engine cannot draw.

The state read back what the run did — `describe` afterwards reported `"unsaved": true` (the
tracked flag: a real headless `is_dirty` is stuck True and would have said so even on a
saved scene), the moved camera, and
`"last_render": {"path": "/tmp/monkey.png", "seconds": 8.21, "bytes": 1025106}`.

Persistence, the other half of the film verdict — save, throw away, bring back:

```
$ python3 deploy/yantrik-os/yos act blender save path=/tmp/monkey.blend
accepted: True → {"saved": "/tmp/monkey.blend"}    revision: 2ae29a56d2a13b92
$ ls -la /tmp/monkey.blend && head -c 12 /tmp/monkey.blend | od -c
-rw-r--r-- 1 yantrik yantrik 924672 …              B L E N D E R - v 4 0 0
$ python3 deploy/yantrik-os/yos act blender new_scene
accepted: True → 0 objects                          revision: c41f56c367bf1090
$ python3 deploy/yantrik-os/yos act blender open path=/tmp/monkey.blend
accepted: True → {"opened": "/tmp/monkey.blend", "scene": "Scene"}
$ python3 deploy/yantrik-os/yos describe blender | head -2
Blender — "monkey.blend", 4 objects, EEVEE 1920x1080
revision: 2ae29a56d2a13b92
```

The revision after `open` is the revision after `save`, hex for hex: the scene that came
back is the scene that was written. (And `open` worked after `new_scene` — the exact refusal
the tracked dirty flag exists to not give: the old guard named `new_scene` as the way out
and then refused the open `new_scene` had just made possible.)

The guards, live, through the real `yos`:

```
$ python3 deploy/yantrik-os/yos act blender run_python "code=print(1)"
yos: blender.app.act refused: CEILING: blender.run_python is graded `dangerous`, above this
machine's `sensitive` ceiling (`tool_permission` in ~/.config/yantrik/settings.yaml), so it
was not run. An action at that grade needs a person to authorise it directly — raise the
ceiling in Settings if that is the intent.                       (exit 1; nothing ran)

$ python3 deploy/yantrik-os/yos act blender delete_object name=Suzanne expect_revision=deadbeefdeadbeef
yos: blender.app.act refused: STALE: this app is at revision 8fcd2b5f516261ad and you acted
on deadbeefdeadbeef. It now reports: Blender — unsaved scene, 4 objects, EEVEE 1920x1080.
Read it again before deciding.                                   (exit 1; scene untouched)

$ python3 deploy/yantrik-os/yos act blender screenshot output=/tmp/shot.png
yos: blender.app.act refused: there is no 3D viewport to screenshot — a background Blender
draws nothing; `render` draws the scene without a viewport

$ python3 deploy/yantrik-os/yos act blender delete_object name=Ghost
yos: blender.app.act refused: there is no object `Ghost` in this scene
$ python3 deploy/yantrik-os/yos describe blender | grep notice
  "notice": "there is no object `Ghost` in this scene",          (the failure, said twice)
```

`import_model` and `delete_object` against the real importer, in a second headless run:

```
$ python3 deploy/yantrik-os/yos act blender import_model path=/tmp/cube.obj   # an 8-vertex cube
accepted: True → {"imported": "/tmp/cube.obj", "objects_added": 1}
$ python3 deploy/yantrik-os/yos act blender delete_object name=cube
accepted: True → {"deleted": "cube"}                         (3 objects again)
$ python3 deploy/yantrik-os/yos act blender delete_object name=cube
yos: blender.app.act refused: there is no object `cube` in this scene
```

Stopping: the Blender was killed **by the pid recorded at launch** — never by pattern; a
pattern kill is what ended the previous attempt at this charter. A SIGTERM'd Blender leaves
its socket node behind (no Rust app handles the signal either; the next bind replaces a
stale node, and `yos ls` falls through sockets that refuse connections — it showed
`(nothing)` with the node still on disk). The node from these runs was removed by hand.

### 4. One real windowed run (WSLg)

The dock's exact command, with a display:

```
$ setsid blender --python apps/blender/bootstrap.py &
[yantrik] control surface on /mnt/wslg/runtime-dir/yantrik/app-blender.sock (14 actions, windowed)

$ python3 deploy/yantrik-os/yos describe blender        # "background": false
$ python3 deploy/yantrik-os/yos act blender add_primitive kind=monkey name=Suzanne
accepted: True → {"object": "Suzanne", …}               # through the bpy.app.timers pump
$ python3 deploy/yantrik-os/yos act blender screenshot output=/tmp/viewport.png
accepted: True → {"path": "/tmp/viewport.png", "seconds": 0.58, "bytes": 1345556}
$ file /tmp/viewport.png
/tmp/viewport.png: PNG image data, 1920 x 1080, 8-bit/color RGBA, non-interlaced
```

This is the half the headless run cannot say: the `bpy.app.timers` pump answers between
Blender's own event-loop turns, and `screenshot` really captures the viewport — the picture
a mind looks at. (WSLg printed EGL_BAD_MATCH warnings at startup; the window survived them
and the capture worked. The conformance probe treats a windowed capture as advisory for
exactly this reason: a VM without GL will honestly refuse.)

### 5. The MCP bridge, live

`yos-mcp` with `YOS_BIN` pointed at this tree's `yos`, an `os_describe blender` call over a
real MCP handshake, while the headless Blender was up:

```
act: delete_object(name)  [standard, settles on return]
act: render(output)  [sensitive, settles on return]
act: save(path)  [sensitive, settles on return]
act: open(path)  [sensitive, settles on return]
act: run_python(code)  [dangerous, settles on return]
```

The bridge reads these grades from the surface itself — nothing about Blender is compiled
into it — so `os_act blender render` reaches a mind with `sensitive` attached, and
`os_act blender run_python` with `dangerous`, which the bridge's own decision table turns
into an ask-the-person in every mode.

### 6. The conformance probe

`tests/conformance/probes/blender.py` is written in the style of the others and is picked up
by the runner's glob. It handles the three shapes a machine can present — Blender not
installed (the shell's refusal is the finding, recorded like container-manager records a
machine with no runtime), Blender already open with someone's scene in it (read-only checks
only; it does not edit a scene it did not create, and does not open a second instance that
would take the socket over), and Blender closed (opens it through the shell, exercises
everything, kills only what it started, with a headless fallback for a machine with no GL).
The render witness is at its centre: the answer's bytes are checked against the file, not
the file against the answer. It has **not been run on the live VM** — it needs the release
staged under `/opt/yantrik`, and this build's charter forbade installing anything there. One
fix went in after the previous session wrote it: its process pattern is now the bootstrap's
path, because the headless fallback runs `blender -b --python …` and the old pattern
(`blender --python`) would not have matched the probe's own process at cleanup; and its
left-as-found check now asserts the honest thing — *nothing answers* on the socket — since a
SIGTERM'd app leaves its node behind, here and in every Rust app.

## What a skeptic will say

**"It's a Python script with a prompt in front."** No: it is a surface, and the difference is
everything the script cannot do. A prompt-in-front runs whatever it is handed and reports
whatever it likes. This app refuses by grade before arguments are even read (`run_python`
under the default ceiling, live output above); it refuses stale reads (STALE, with the
current world in the sentence); it publishes a revision the caller can pin and be refused by;
it reports `unsaved` from a flag it tracks honestly rather than Blender's headless lie; it
checks its own claims against the filesystem (a render that wrote nothing is not a render);
and the same bridge that serves `notes` serves this — `yos-mcp` exposes `os_act blender
render` with `sensitive` attached and `run_python` with `dangerous`, which stops at an
approval card only a person can answer, in every mode. The card sentences come from the
app's own published purposes: `delete_object` says "not recoverable", `new_scene` says "no
undo", `run_python` now ends "it is not recoverable" — the shell's red warning line is drawn
from those words, not from a guess.

**"A port of the dispatch is a second copy that will drift."** It is a second copy, and the
drift is caught rather than hoped against: the revision hash is one vector asserted in two
languages, the refusal sentences are asserted in full in 106 tests, and the conformance
probe checks the published grades as a whole map — a surface that quietly regraded `render`
to `standard` would fail by commission, not pass by omission.

**"Your render was EEVEE on a WSL GPU; the VM has none."** True, and the surface says so
rather than hanging: the render refusal names the CPU fallback ("Cycles on CPU renders
anywhere"), and the probe treats a window that cannot survive GL by falling back to
`blender -b`, where every socket action works and `screenshot`'s refusal is itself one of
the checks.

**"Blender 4.0.2 is not 4.5."** The addon was written against 4.x and tested on 4.0.2; the
engine-id list already carries the EEVEE→EEVEE_NEXT rename that happened in 4.2, and the
import paths try the new `wm.obj_import`/`wm.stl_import` operators before the legacy ones.
What has not been exercised on newer versions is stated here rather than implied otherwise.

**"You never opened it from the dock."** Not end-to-end: that needs a running desktop shell,
which this environment does not have. What was tested directly is the exact command the
route builds (`blender --python <bootstrap>`, run above), the route's resolution and
availability logic (dock unit tests, 297 green), and the release staging path both
candidates of `blender_bootstrap()` look at. The first dock click on a deployed machine is
the conformance probe's job, and it is written.
