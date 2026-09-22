# Three new apps, used rather than tested — 22 Sep 2026

Written by Claude (Opus 5) driving VM 520 (`192.168.4.44`, Debian 13, labwc/Wayland, 4 cores, **no
GPU**) over its control surfaces. Shell under test: `v0.1.0-312-g7ada2be`, then `-g2688a10` once
Studio landed. Issues filed: #93, #94, #95, #96, #97, #99; #53 closed on a re-measurement; PR #101
fixes a bug this session found in code merged an hour earlier.

Arcade (#90), Blender (#91) and Studio (#98) all merged today, and all three had been exercised
only by their own tests. This is what happened when someone used them to make something.

**Method.** `yos describe` / `yos act` over the app sockets, from an ssh session. `grim` for
screenshots. Every number below came back from the machine; where a figure is a sum or an average
the window is stated.

## Top table

| app | verdict | single worst thing | issue |
|---|---|---|---|
| **Arcade** | **works** | the spec grammar is discoverable only by being refused | #94 |
| **Blender** | **works headless** | a render blocks its own surface, so `describe` times out | #95 |
| **Studio** | **works, after a fix** | `set_backend` killed the process, every time | #101 |
| **Image Viewer** | works | one open window is listed twice | #93 |

## What was made

**Arcade.** A character (`Tuk`, teal `#2fd4c4`, violet accent) and a game
(*Tuk's Teal Morning* — meadow, 7 berries, 3 patrols, 3 lives, playful). `build` produced one
654,544-byte HTML file. `verify` passed six of seven headless gates — boots, clean console, a
frame renders, input moves the player, a bot wins, a bot loses — and failed the seventh:

```
failed at frame_budget: average frame 354.2 ms under software rendering, budget 250 ms
```

**Blender.** Suzanne, lit, framed, and rendered through the addon's socket:

```
yos act blender new_scene / add_primitive kind=monkey / set_material / set_light / set_camera
yos act blender set_render engine=workbench resolution=960x540
yos act blender render output=/tmp/suzanne.png
  → {"path": "/tmp/suzanne.png", "seconds": 1.53, "bytes": 368298}
```

Then `yos act image-viewer open path=…` and the desktop showed it: 960 × 540, 359.7 KB, PNG. A
program nobody in this repository wrote, driven through the same two verbs as everything else, with
the result opened by an app that is ours. That is the protocol's whole claim, demonstrated.

**Studio.** Two pictures from *"a teal fox asleep on a windowsill at dawn, soft light"* at 768×512,
seeds 7 and 8 — the documented "seeds run on" behaviour, confirmed. Each landed with a sidecar
naming prompt, negative, seed, backend, model, seconds, size, steps and cfg. No GPU and no key on
this machine, so the `fake` backend drew them; the app said so in its own summary rather than
pretending.

## Findings

### Arcade's grammar is not readable, only refusable — #94

Four specs were rejected before one was accepted. Every refusal was correct and named the field.
None of the vocabularies is written anywhere a caller can read first: `expression` is
`cheerful|sleepy|fierce|surprised`, `music` is `calm|bouncy|tense|playful`, `tail` is
`none|stump|long|curl`, and the `proportions` keys are `head_body`, `limb_length`, `width`. What
`describe` offers instead is:

```
spec: string — The character JSON; the validator refuses with a sentence naming the field
```

Every other surface on this desktop states its enums inline (`direction: string — left | right`).
Arcade is the one app whose contract lives only in the validator, so a mind has to brute-force it,
paying a round trip per guess.

### A render blocks Blender's surface — #95

`render` is declared `settles on return` and runs on the thread that answers the socket. With a
GPU that is invisible. Here:

| engine | result |
|---|---|
| EEVEE, 16 samples, 960×540 | **55 s for the first sample alone**; `describe` got no reply and the CLI gave up at its 10 s timeout |
| Workbench, same size | **1.53 s**, `describe` answering either side |

A mind cannot tell a long render from a dead app — `describe` timing out looks the same — so the
reasonable recovery, restarting it, throws the render away. And nothing can be cancelled while the
only thread is inside Blender's render loop.

### Blender's window cannot open here — #96

```
GHOST: failed to initialize display for back-end(s): ['X11']
```

Debian's `blender` 4.3.2 ships the X11 GHOST back-end only, and this session is Wayland-only.
Headless works, which is how everything above was rendered, but the point of the surface is that
the person and the mind look at the same screen. `xwayland` fixes it and labwc has to restart to
pick it up, so it does not rescue a running session.

### `frame_budget` fails a working game on a slow machine — #97

The constant's own comment names the machine it was calibrated on:

```rust
/// … headless Chromium in WSL renders through swiftshader (software) …
pub const FRAME_BUDGET_MS: f64 = 250.0;
```

"Software rendering" is not one speed. This GPU-less VM is 1.4× slower than that WSL box, so a
meadow with seven berries reports as a broken game and `verify` returns `passed: false` overall —
the signal a release gate would act on. The gate wants to be relative to the machine (a reference
frame time, measured once) rather than an absolute number.

### One Images window, two taskbar buttons — #93

With exactly one viewer process alive, `describe shell` lists it twice for as long as it is open:

```
-- open, t+2s --    [('images', 'Images'), ('image', 'Images')]
-- open, t+25s --   [('images', 'Images'), ('image', 'Images')]
== killed ==
-- closed, t+3s --  [('image', 'Images')]      ← inside the 9 s COMPOSITOR_TTL, correct
-- closed, t+8s --  []
```

`merge_windows` pairs the launch registry against the compositor's snapshot by app id, and says so
in its own doc comment. The launcher registers this app as `images`; `APP_NAMES`, `STEM_TO_ID` and
the icon map call it `image`. It is the only row in `ROUTES` that no `APP_NAMES` row matches. The
name is historical — `images` was already the shell's own screen 11, and the app's route was added
later under the screen's name.

*(An earlier version of #93 also claimed the closed window's entry was never reaped. That was
wrong: I had sampled 4 s after the kill and `COMPOSITOR_TTL` is 9 s by design. Re-measured above.)*

### `regrade` had never worked — #101

The one worth the session on its own.

Studio's `set_backend` is graded `sensitive` because it decides where every later prompt goes, and
it regrades `generate` the moment it lands, so a caller cannot point the app at a hosted service
and generate in the same breath under the old local grade. Calling it on the machine:

```
app.act action=set_backend id=app-studio#2 ceiling=sensitive caller_uid=1000
thread 'main' panicked at control.rs:533:34: RefCell already borrowed
```

`on_ui_thread` runs every handler from inside `REGISTRY.borrow()`; `regrade` took `borrow_mut()`.
Calling it from a handler is the only way it is ever meant to be used, so that was every use of
it. It passed review and CI because both its tests called it from open code, with nothing holding
the registry — they exercised the function and never the path.

Worse than a crash: `set_backend` writes the config **before** it regrades. So the sequence was
config written naming a hosted service, panic, app gone — and it came back pointed at that service
with `generate` still graded `standard`. **The state the primitive exists to prevent is exactly
the state its failure produced.** The config left on the machine afterwards:

```json
{ "backend": { "kind": "openai-images", "base_url": "https://api.openai.com/v1",
               "model": "gpt-image-1", "api_key_env": "OPENAI_API_KEY" } }
```

No key was ever involved — only the variable name, which is the design and held up.

After #101, on the same machine, with that config still in place:

```
started                          kind: openai-images | generate graded: sensitive
set_backend kind=fake            kind: fake          | generate graded: standard   (alive)
set_backend kind=openai-images   kind: openai-images | generate graded: sensitive
describe now says                act: generate(…)  [sensitive
panics in the log                0
```

### The deploy list nobody checks — #99

`deploy.sh` has a hand-written `APPS=` list and `yantrik-arcade` is not on it; `yantrik-studio`
will not be either. The loop is `if [ -f … ]; then cp; fi`, so a missing binary is not an error —
the deploy reports success and the app is not there. The ISO is unaffected, because it builds
`--workspace` and ships the release directory whole. `deploy.sh` is the path used to push a working
tree at a running machine, which is how these apps get tested before release, so the stale list
bites exactly where the work happens.

## What this says about the protocol

Three apps written independently — two by agents from a charter, one addon for a program we do not
own — and all three answered `describe` and `act` the same way, with grades, deferral and
read-back. Nothing in this session needed an app-specific client.

The failures were not protocol failures. They were a name that one table spelled differently, an
enum that lived only in a validator, a synchronous call on the answering thread, a constant
calibrated on one machine, and a `RefCell` borrowed twice. Every one of them is the kind of thing
that only shows up when someone tries to make something, which is the argument for doing this
again after the next three apps land.

## Idle cost, re-measured

#53 said an idle desktop burned 52.7% of four cores and 4.0 GB across 41 processes. On
`v0.1.0-312`, idle with Terminal open at an empty prompt, `/proc` deltas over 60 s:

```
   7.13%  yantrik-ui
   2.70%  pi
   1.50%  openclaw-gateway
   1.02%  python
   0.97%  labwc
---- total 16.3% of one core = 4.1% of 4 cores
---- 181 processes on the machine, 1.84 GB RSS in total
```

`yantrik-terminal` is not in the list; #29 removed it and it was most of the total, as #53
predicted. Closed, with the ask for a budget asserted in CI moved to #26 — because nothing here
stops it drifting back, and nothing measures it.
