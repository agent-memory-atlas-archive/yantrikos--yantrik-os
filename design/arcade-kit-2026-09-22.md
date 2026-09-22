# Arcade: a game construction kit with a control surface

2026-09-22 · branch `feature/arcade-kit`

Arcade is a Yantrik OS app that turns small JSON specs into playable 3D games. A mind
writes two kinds of bounded spec — a `character` and a `game` — and a deterministic
compiler turns them into one self-contained HTML file: no network, no external
textures, nothing to install, a pinned vendored Three.js inside. The genre for this
slice is the 3D arena collector: you move, you collect N items, you avoid hazards,
you win at N and you lose on three hits.

The division of labour is the whole design. **The mind never writes game code.** It
writes facts in two bounded grammars. Everything else — the loop, the input, the
collision, the canvas scaling, the palette handling, the juice (particles, screen
shake, tweens), the sound, the character's scene graph and light rig and animation —
belongs to the compiler and the engine it embeds, which are repo code with tests, not
generated text. A bounded grammar can be refused with a sentence naming the field;
a deterministic compiler can be held to hard gates. Free-form generation can be held
to neither.

## What a mind writes

Two grammars, each with a schema, a validator and a vocabulary. Unknown fields are
refused rather than dropped, so a typo is a sentence and not a silent default.

`character`:

```json
{
  "name": "Pip",
  "archetype": "critter",
  "proportions": { "head_body": 1.15, "limb_length": 0.9, "width": 0.95 },
  "ears": "pointy",
  "tail": "curl",
  "palette": { "base": "#44cc88", "belly": "#f2ead8", "accent": "#ff9f43", "nose": "#2f3640", "eye": "#2f3640" },
  "expression": "cheerful",
  "stance": "bouncy"
}
```

`archetype` ∈ {blob, critter, brute, sprite}, `ears` ∈ {none, round, pointy, long},
`tail` ∈ {none, stump, long, curl}, `expression` ∈ {cheerful, sleepy, fierce,
surprised}, `stance` ∈ {upright, crouched, bouncy}; proportions are bounded
multipliers (head:body 0.5–1.6, limbs 0.4–1.6, width 0.5–1.6) and the palette is five
hex colours by role. Everything but `name` has a default, so a minimal character is
one line and a detailed one is this.

`game`:

```json
{
  "title": "Pip's Meadow Run",
  "arena": { "size": 20, "theme": "meadow" },
  "player": { "character": "Pip", "speed": 8 },
  "collectible": { "kind": "berry", "count": 8 },
  "hazards": [
    { "kind": "wanderer", "speed": 2.5, "count": 2 },
    { "kind": "patrol", "speed": 3.5, "count": 1 }
  ],
  "lives": 3,
  "music": "bouncy"
}
```

`player.character` is either an inline character spec or the *name* of a saved one;
the library resolves names before the compiler ever sees the spec, so a game file
carries its cast with it. `theme` ∈ {meadow, dusk, candy, volcano, ice}, `kind`
(collectible) ∈ {berry, coin, crystal, star}, hazard kinds ∈ {chaser, wanderer,
patrol}, `music` ∈ {calm, bouncy, tense, playful}. An optional five-colour arena
palette overrides the theme's.

Refusals name the field the way the JSON spells it. Three real ones, verbatim from
this session:

```
arcade.app.act refused: `palette.base` must be a hex colour like "#44cc88", but was "green".
arcade.app.act refused: `hazards[0].speed` (9) must be below `player.speed` (6): a chaser
                        the player cannot outrun makes the game unwinnable.
arcade.app.act refused: `music` must be one of calm, bouncy, tense, playful, but this spec says "jazz".
```

The vocabularies are checked on the raw JSON before the typed parse, because serde's
own "unknown variant" sentence does not say which field the variant was in — and the
kit's promise is a refusal that does. The numeric rules exist for the verifier's
sake: a chaser faster than the player makes WIN unreachable, so the grammar refuses
the spec rather than let a gate fail a game it should never have accepted. The same
reasoning bounds item density (about 8 m² per item) and the hazard total (12).

## What the compiler owns

`compile()` assembles exactly three things: the pinned vendored Three.js build
(`apps/arcade/vendor/three-r149.min.js`, r149 UMD, MIT licence beside it in
`THREE-LICENSE`), the engine (`apps/arcade/src/engine.js`, included from source so
tests and shipped bytes cannot drift), and the resolved spec turned into the
engine's runtime JSON. One `<doctype html>` document, three inline `<script>` blocks,
no `src=`, no `<link>`, nothing fetched. A built game is ~640 KB, most of it Three.js.

Two properties are tested, not asserted in prose:

- **Determinism.** No timestamps and no randomness in the assembly; the engine seeds
  every placement from a hash of the spec itself. `the_same_spec_compiles_byte_identical`
  compiles twice and compares bytes.
- **Escaping.** The one sequence that can end a `<script>` early is `</`, and `<\/` is
  a legal JSON escape for the same string, so `json_for_script` rewrites it. A title
  of `</script><b>boo</b>` lands in the document as inert data, and the `<title>`
  element escapes it independently. `a_script_tag_in_the_title_cannot_escape_the_json`
  counts exactly three script closers in a hostile build.

The engine is where the game lives: a themed square arena with walls and props,
seeded item scatter with a minimum gap, three hazard behaviours (chaser steers at
you, wanderer walks its own path, patrol runs a fixed line), contact shadows,
a particle burst on pickup and hit, screen shake, a two-second mercy blink after a
hit, zzfx-class WebAudio synthesis for pickup/hit/win/lose plus a pentatonic music
scheduler per mood, and a character built from primitives — named parts assembled
per archetype, walked with a limb swing, idled with a bob, blinked every few seconds.

The engine publishes one contract the verifier drives:

```
window.__arcade = { version, title, spec, state(), setBot('win'|'lose'|null), reset() }
```

`state()` returns `{status, collected, target, lives, x, z, frameMs, frames, bot,
errors}`. `status` flips to `playing` only after the first rendered frame, which is
what makes "boots" a real gate. `setBot` steers the *ordinary input vector* — the
bots press the same keys a person would, through the same handler — so a bot winning
proves the game is winnable by its own controls, not by a back door. `errors` collects
`window.onerror` for the console gate.

## The library

`~/.local/share/yantrik/arcade` (or `YANTRIK_ARCADE_DIR`): `characters/<slug>.json`,
`games/<slug>/{spec.json,index.html,verify.json,screenshot.png}`. `delete` moves the
game directory to the freedesktop Trash — `~/.local/share/Trash` with a proper
`.trashinfo` — because a recoverable delete is a standard action and a gone-forever
delete is not. The Trash entry keeps its original path so Files can put it back:

```
[Trash Info]
Path=/home/yantrik/.local/share/yantrik/arcade/games/throwaway-trial
DeletionDate=2026-09-22T13:38:27
```

Rebuilding clears a stale `verify.json`: a game that changed since its last verdict
reports as unverified, never as verified-when-it-wasn't.

## The control surface

Seven actions, all `standard` risk — nothing here touches anything outside the
library, and even `delete` goes to the Trash. The three slow ones declare `defers`
because a headless verification under software rendering takes minutes and the UI
roundtrip budget is three seconds: they validate inline (an unknown or unbuilt game
is refused immediately, with its sentence), then run on a named worker thread that
posts its verdict back onto the UI thread. `describe` reports running jobs, so a
caller can poll for the landing instead of guessing.

| action | args | settles | purpose |
| --- | --- | --- | --- |
| `new_character` | `spec` | on return | save a character spec; the validator refuses with a sentence naming the field |
| `new_game` | `spec` | on return | save a game spec; `player.character` may name a saved character or inline one |
| `build` | `game` | on return | compile a saved game into its one HTML file |
| `play` | `game` | later | open a built game in the desktop Browser |
| `verify` | `game` | later | the seven headless gates |
| `screenshot` | `game` | later | a headless PNG of a built game |
| `delete` | `game` | on return | move a game to the Trash, where Files can bring it back |

Buttons in the window and actions on the surface are one code path
(`run_action`); an agent and a person cannot get different answers about whether
something worked. The window is a workbench: the library's games and characters as
two lists, the spec editor, and the selected game's name; a notice banner carries
every outcome including refusals.

`play` reuses the desktop's browser route rather than inventing a second browser: it
asks the shell's control surface to open the Browser app, waits for the debugging
port that route already carries (`--remote-debugging-port=9222`), and opens the game
as a new tab over CDP.

Registration is in the four places the other apps live: `SURFACES` in
`crates/yantrik-app-runtime/src/control.rs`, `ROUTES` and `PURPOSES` in
`crates/yantrik-ui/src/wire/dock.rs`, `APP_NAMES`/`STEM_TO_ID` in
`crates/yantrik-ui/src/windows.rs`, and `apps/desktop-files/yantrik-arcade.desktop`.
The repo's own cross-checks (`app_names_agree_everywhere`,
`every_launchable_name_reaches_a_surface`) fail the build if those disagree; both
pass. `build-release.sh` needed no change: it discovers binaries from the workspace
build and globs desktop files.

## The verifier, and why it is not Playwright

The charter's gate list is: boots, zero console errors, a non-blank canvas frame,
input moves the player, a scripted bot reaches WIN, another reaches LOSE, frame time
under budget. Something has to drive a real browser through all of it.

The options, checked on this machine:

- **Playwright (Python)** — not installed: `python3 -c "import playwright"` fails
  with ModuleNotFoundError, and pulling it plus its browser download is a network
  install the brief did not ask for.
- **Playwright (npm)** — same story, plus a Node toolchain in a Rust repo.
- **chromium** — not installed; **google-chrome 152.0.7977.82** is, at
  `/usr/bin/google-chrome`.

So the verifier is **a small CDP driver in Rust on top of the Chrome already
present** — the thing that installs cleanly in WSL is *nothing*, because the browser
is already there. `verify.rs` launches Chrome
(`--headless=new --use-gl=angle --use-angle=swiftshader --enable-unsafe-swiftshader
--no-sandbox --disable-dev-shm-usage --mute-audio --hide-scrollbars
--window-size=1280,720 --remote-debugging-port=0 --user-data-dir=<temp>`), reads the
port from the `DevToolsActivePort` file, lists targets over HTTP with `ureq`, and
speaks CDP over a websocket with `tungstenite` — both already workspace dependencies.
No new dependency but `png` for decoding the screenshot. `chrome_binary()` probes six
names by `--version` so the verifier is not hardwired to one install.

The gate sequence, in the charter's order:

1. **boots** — `state().status == "playing"` with `frames > 0` within 20 s. The
   engine only says `playing` after its first rendered frame, so this one gate covers
   Three.js loading, WebGL context creation, spec parsing and arena construction.
2. **no_console_errors** — judged *last*, from CDP events (`Runtime.consoleAPICalled`
   of type error, `Runtime.exceptionThrown`, `Log.entryAdded` at error level) plus the
   engine's own `window.onerror` list, so it covers the whole session.
3. **frame_renders** — a real `Page.captureScreenshot`, decoded as a PNG, counted for
   distinct colours; ≥ 8 or the frame is "nearly blank". A failed WebGL context, a
   canvas that never drew, and a black-screen crash all come back one or two colours.
4. **input_moves_player** — a real dispatched `Input.dispatchKeyEvent` W held for
   1.4 s; the player's `(x, z)` must move more than half a metre.
5. **bot_reaches_win** — `reset(); setBot('win')`; the greedy bot must collect every
   item inside a budget scaled by the target count.
6. **bot_reaches_lose** — `reset(); setBot('lose')`; the suicidal bot must burn every
   life within 60 s.
7. **frame_budget** — after `setBot(null)`, the engine's rolling average frame time
   must sit under 250 ms. That budget is for software rendering; a real GPU has
   several times the headroom.

If the game does not boot, the report lists every gate with the same reason rather
than silently passing the ones that never ran.

The session logic is separated from launching (`verify_session` takes a websocket
URL) so the whole gate sequence has a unit test with no browser in it: a fake CDP
server on a local socket answers the scripted conversation, and the same test with
`plant_error: true` injects one console error to prove exactly the console gate sinks
and only that gate.

## The real run

Two character specs and two games, driven end to end through the control surface with
`yos` against the app window running under WSLg. The full transcript is
`/tmp/arcade-run/real-run.log`; the commands below are what it contains.

```
$ yos act arcade new_character spec='{ …pip.json… }'
{ "saved": "saved character \"Pip\"", "slug": "pip", … }

$ yos act arcade new_character spec='{ …grumble.json… }'
{ "saved": "saved character \"Grumble\"", "slug": "grumble", … }

$ yos act arcade new_game spec='{ …game-pip.json… }'      # casts "Pip" by name
$ yos act arcade new_game spec='{ …game-grumble.json… }'  # casts "Grumble", custom palette

$ yos act arcade build game="Pip's Meadow Run"
{ "built": "built pip-s-meadow-run → …/games/pip-s-meadow-run/index.html", … }

$ yos act arcade build game="Grumble's Dusk Patrol"
{ "built": "built grumble-s-dusk-patrol → …/games/grumble-s-dusk-patrol/index.html", … }

$ yos act arcade verify game="Pip's Meadow Run"
{ "started": true, "summary": "verify of pip-s-meadow-run started on a worker; …" }
$ yos describe arcade        # polled until the job landed
{ "jobs": [{ "action": "verify", "status": "done",
             "detail": "pip-s-meadow-run: passed all 7 gates", … }] }

$ yos act arcade verify game="Grumble's Dusk Patrol"     # also passed all 7 gates
$ yos act arcade screenshot game="Pip's Meadow Run"
$ yos act arcade screenshot game="Grumble's Dusk Patrol"
```

Both reports, from `verify.json` beside each game (browser: google-chrome):

| gate | Pip's Meadow Run | Grumble's Dusk Patrol |
| --- | --- | --- |
| boots | engine up, 3 frames rendered | engine up, 1 frames rendered |
| no_console_errors | the console stayed clean for the whole session | same |
| frame_renders | frame has 65 distinct colours | frame has 65 distinct colours |
| input_moves_player | holding W moved the player 4.40 m | holding W moved the player 6.24 m |
| bot_reaches_win | the win bot collected all 8 items | the win bot collected all 6 items |
| bot_reaches_lose | the lose bot burned every life (lives now 0) | same |
| frame_budget | average frame 48.0 ms, budget 250 ms | average frame 68.3 ms, budget 250 ms |

Screenshots, both real frames of real games:

- `/home/yantrik/.local/share/yantrik/arcade/games/pip-s-meadow-run/screenshot.png`
- `/home/yantrik/.local/share/yantrik/arcade/games/grumble-s-dusk-patrol/screenshot.png`

`play` was exercised against a Chrome carrying the same debugging-port flag the
shell's browser route carries (the desktop shell itself was not running in this
environment): the action found the port alive, opened the tab, and the job reported
`opened index.html in the desktop Browser`; the tab list showed
`file:///home/yantrik/.local/share/yantrik/arcade/games/pip-s-meadow-run/index.html`.
The shell-ask half of `play` is covered by its refusal sentence and by the route's
own flags in `dock.rs`.

`delete` was exercised on a third throwaway game: created, built, deleted, found in
`~/.local/share/Trash/files/yantrik-arcade-throwaway-trial` with the `.trashinfo`
quoted above, then left there — it is exactly what a person's delete leaves.

The headless half of the binary (`yantrik-arcade build|verify|screenshot …`) is the
same library, compiler and verifier with a different driver, for machines with no
display; exit codes are 0 done, 1 a gate failed, 2 refused.

## The skeptic: every generated game will look the same

Half right, and here is the half. Two frames from this run, side by side:

**Pip's Meadow Run** — a pale green meadow under cream walls with low-poly trees
outside; a small teal critter with pointy ears and a curled tail, bouncing; yellow
berries; a pink box wanderer and a pink capsule patrolling a line.

**Grumble's Dusk Patrol** — a slate-violet dusk arena, darker walls, rocks outside;
a wide purple brute, crouched, small head, no ears; teal crystal shards; spiky pink
chasers that steer at you and pink boxes that do not.

What varies, all of it spec-driven: the character's silhouette (archetype,
proportions, ears, tail), its five colours, its face and stance; the arena theme and
an optional five-colour override; the arena size; the collectible kind (four
distinct shapes) and count; the hazard mix — kinds, speeds, counts — which changes
what the arena *does*, not just what it shows; the props; the music mood; and the
layout, which is seeded from the spec, so two specs place everything differently.

What does not vary: the arena is a walled square seen from the same follow-camera;
the HUD sits top-left in the same type; shadows are the same soft blobs; the engine
and its juice vocabulary are one file. That is the kit's house style, the way a
chiptune label has a sound. If the house style is the complaint, the answer is that
the engine's themes, camera and HUD are the parts with room to grow next.

And the honest limit: **fun is not measured here, only playable.** The gates prove a
game boots, renders, answers its keys, can be won and can be lost inside a frame
budget. They cannot prove anyone wants a second round. The win-bot and lose-bot are
proxies for "winnable" and "losable", which is a floor, not a review.

## Tests

- `cargo test --release -p yantrik-arcade` — **62 passed**. Validators (every refusal
  names its field; vocabularies; the unwinnable-chaser rule; item density; unknown
  fields refused not dropped), compiler (self-containedness, the runtime contract,
  byte-identical determinism, hostile titles), library (round trips, duplicate
  refusals, stale verdicts, Trash), verifier (blank vs rendered frames, event-error
  filtering, and the two fake-CDP end-to-end gate tests).
- `cargo test --release -p yantrik-app-runtime` — 37 passed.
- `cargo test --release -p yantrik-ui` — 297 passed, including
  `app_names_agree_everywhere` and `every_launchable_name_reaches_a_surface`, which
  are what keeps this app's four registration sites honest.
- `python3 deploy/yantrik-os/yos-selftest.py` — all checks passed.
- `python3 deploy/yantrik-os/yos-mcp-selftest.py` — all checks passed.
- `python3 -m unittest discover -s harnesses/tests` — 176 tests, OK.
- `python3 tests/app-lints/run.py` — `arcade clean` (no unset properties, no dead
  handlers, nothing added to the baseline).

## What this slice is not

One genre; no editor beyond the spec text box; no save-the-world persistence beyond
the library on disk; sound is synthesized and is muted under the verifier
(`--mute-audio`), so the audio path is exercised by construction, not by ear; the
250 ms frame budget is software-rendering reality, not a performance target; and the
character builder assembles primitives — it is charming, not a modelling suite. The
next slice's obvious moves are a second genre on the same engine, more themes, and a
way to hear the music without a browser tab.
