# Arcade: a game construction kit with a control surface

Branch: `feature/arcade-kit` · design account: `design/arcade-kit-2026-09-22.md`

## What a person can now do

Open **Arcade** from the launcher (or run `yantrik-arcade`), write a small JSON
character and a small JSON game, and get a playable 3D arena-collector as one
self-contained HTML file — built, headless-verified against seven hard gates,
screenshotted, opened in the desktop Browser, and deletable to the Trash. Or do all
of it without a window, through the control surface (`yos act arcade …`) or the
binary's own headless half (`yantrik-arcade build|verify|screenshot`).

## What is in this branch

- `apps/arcade/` — the kit and the app:
  - `src/spec.rs` — the two bounded grammars (`character`, `game`) with validators
    that refuse with a sentence naming the field, vocabulary pre-checks included.
  - `src/compile.rs` — the deterministic compiler: spec → runtime JSON → one HTML
    document embedding the pinned vendored Three.js r149 (`vendor/`, MIT licence
    beside it) and the engine; `</`-escaping so no spec text can leave the script.
  - `src/engine.js` — the game engine: arena, items, three hazard behaviours,
    particles, screen shake, WebAudio synthesis and per-mood music, character
    assembly and animation, the `window.__arcade` contract with win/lose bots that
    steer the ordinary input path.
  - `src/library.rs` — the library on disk (`~/.local/share/yantrik/arcade`),
    name/slug resolution, build bookkeeping, delete to the freedesktop Trash.
  - `src/verify.rs` — the headless verifier: a small pure-Rust CDP driver over the
    Chrome already installed in WSL (Playwright is not, and needs an install this
    does not), seven gates in the charter's order, plus a fake-CDP unit test that
    runs the whole gate sequence browser-free.
  - `src/play.rs` — opens a built game in the desktop Browser by reusing that
    route's debugging port.
  - `src/cli.rs`, `src/main.rs`, `ui/app.slint` — the headless subcommands, the
    window, and the seven-action control surface (play/verify/screenshot defer to
    worker threads).
- Registration: `SURFACES` in `crates/yantrik-app-runtime/src/control.rs`, `ROUTES`
  and `PURPOSES` in `crates/yantrik-ui/src/wire/dock.rs`, `APP_NAMES`/`STEM_TO_ID` in
  `crates/yantrik-ui/src/windows.rs`, `apps/desktop-files/yantrik-arcade.desktop`.
  `build-release.sh` needed no change (it discovers binaries and desktop files).

## Verified

- `cargo test --release -p yantrik-arcade`: 62 passed. `-p yantrik-app-runtime`:
  37 passed. `-p yantrik-ui`: 297 passed, including the two tests that cross-check
  the registration tables above.
- `yos-selftest.py`, `yos-mcp-selftest.py`: all checks passed.
  `python3 -m unittest discover -s harnesses/tests`: 176 tests OK.
  `tests/app-lints/run.py`: `arcade clean`.
- One real run through the surface: two characters (Pip the bouncy critter, Grumble
  the crouched brute), two games (meadow with berries and patrols; dusk with
  crystals, chasers and a custom palette), each built, verified — all seven gates
  passed on both, 48.0 ms and 68.3 ms average frame under software rendering — and
  screenshotted. A third throwaway game was created, built and deleted to prove the
  Trash path. Commands, report JSON and PNG paths are in the design doc; the
  transcript is `/tmp/arcade-run/real-run.log`.

## Not done, on purpose or by reach

- One genre (3D arena collector); the kit's grammars and gates are the reusable part.
- `play` was proven against a Chrome carrying the browser route's debugging-port
  flag; the desktop shell was not running in this environment, so the shell-ask half
  is covered by its refusal path and the route's flags, not by a live shell.
- Fun is not measured, only playable: the gates prove bootable, renderable,
  controllable, winnable, losable, within budget. The design doc's skeptic section
  says what varies between games and what is the kit's house style.
- Sound is synthesized and muted under the verifier; the audio path is exercised by
  construction, not by ear.

Co-Authored-By: Qwen 3.8 Max (via Claude Code) <noreply@alibabacloud.com>
Claude-Session: https://claude.ai/code/session_012NJMVuSihrV9NvSwpz5mei

🤖 Generated with [Claude Code](https://claude.com/claude-code)
