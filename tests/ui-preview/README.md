# Native shell visual checks

This headless runner compiles the production Slint desktop, launcher, taskbar,
status bar and UI kit. It renders with Slint's software renderer; it does not
start services, attach a mind, or replace the running desktop. Sample names and
system readings are fixture data. The `app` scene demonstrates the shared frame,
not a running Notes application.

Run the complete interaction and responsive rendering checks with
`bash tests/ui-preview/validate.sh` after dependencies have been fetched. It uses
the existing `CARGO_TARGET_DIR` when set and writes scene images under
`target/ui-validation/`.

From the repository root, on a machine with the normal Slint build dependencies:

```sh
cargo run --manifest-path tests/ui-preview/Cargo.toml --profile fast -- target/desktop.png 1280 800 desktop
cargo run --manifest-path tests/ui-preview/Cargo.toml --profile fast -- target/desktop-light.png 1280 800 desktop light
cargo run --manifest-path tests/ui-preview/Cargo.toml --profile fast -- target/agent.png 800 600 agent
cargo run --manifest-path tests/ui-preview/Cargo.toml --profile fast -- target/launcher.png 800 600 launcher
cargo run --manifest-path tests/ui-preview/Cargo.toml --profile fast -- target/frame.png 800 600 app light
cargo run --manifest-path tests/ui-preview/Cargo.toml --profile fast -- verify-controls
cargo run --manifest-path tests/ui-preview/Cargo.toml --profile fast -- target/unused.png 800 600 verify-launcher
```

The last two commands dispatch actual keyboard and pointer events. They assert:

- Enter and Space activate the focused shared button.
- A focused button cannot fire after becoming disabled or loading.
- Re-enabling restores activation.
- Disabled inputs cannot be cleared; enabled inputs can.
- Launcher selection opens the expected application and closes the overlay.
- A reopened launcher can be dismissed with Escape.

The runner is a separate Cargo workspace so production release builds do not
include test fixtures. Its lockfile pins the same Slint version as the OS.
For complete-shell integration, also run `cargo check -p yantrik-ui` and
`cargo test -p yantrik-ui-kit -p yantrik-design-tokens` in the main workspace.

## Actual app interiors

The `notes`, `files`, `files-grid`, `files-empty`, and `settings` scenes instantiate
the production NotesEditor, FileBrowser, and SettingsScreen components. Their
content is deterministic fixture data, and service callbacks are recorded without
performing disk operations. Unlike the `app` scene, these are actual app interiors.

```sh
cargo run --manifest-path tests/ui-preview/Cargo.toml --profile fast -- target/notes.png 1280 800 notes
cargo run --manifest-path tests/ui-preview/Cargo.toml --profile fast -- target/files.png 800 600 files-grid
cargo run --manifest-path tests/ui-preview/Cargo.toml --profile fast -- target/settings.png 1280 800 settings light
```

`files-home` (grid) and `files-home-list` draw Files at home as the desk-and-mind redesign
has it: the places sidebar, folder tiles with item counts and "changed … ago" (one folder
whose count could not be read, drawn as unknown), and the recent row under the grid.

A target directory shared with another checkout of this repository can run THAT checkout's
build script: the preview's build.rs bakes in `CARGO_MANIFEST_DIR`, and cargo hashes a path
package by its path relative to the workspace, so the script from the other tree is reused
and compiles the other tree's .slint files. The symptom is an error about a property that is
plainly in the file. Use a target directory per checkout, or
`cargo clean -p yantrik-ui-preview` before building.

App interaction regression checks (1280×800):

```sh
cargo run --manifest-path tests/ui-preview/Cargo.toml --profile fast -- target/unused.png 1280 800 verify-apps
```

This sends real pointer/key events to the production components and checks theme
selection, repeated activation of the active theme, wallpaper keyboard activation,
disabled history navigation, and distinct file-grid hit targets across rows.

The file-grid probe resizes the native window from 1280×800 to 800×600 and verifies
that the last file remains selectable on the wrapped second row. The standalone
Notes wrapper should also be checked with `cargo check -p yantrik-notes`; its
preferred-size constraints can reveal resize cycles hidden by a fixed fixture.

`verify-overflow` exercises the production app header with keyboard and pointer:
disabled commands, arrow/Enter selection, the final menu command, and Escape.
`verify-idle` settles a production Terminal screen, then checks for zero requested
redraws over 1.5 seconds. This is a render-loop regression check, not a substitute
for measuring the deployed process's CPU and memory.

The app probe also switches Notes side panels, saves with the assistant open,
and resizes from a wide window with both panels to a compact window. It checks
that the writing area is not squeezed between both panels at compact widths.

## Agents

`verify-agents` draws the production Agents screen (and an agent's own window) from
fixture data — two agents, pi's session with a finished command's terminal opened, a
reported call, a running command's live output and a failed one — and sends real
pointer events: the list reports the pointer over it and leaving it (so rows hold
still under it), a row selects, a tab filters, Stop reaches the agent shown and a card
opens from its line. It writes the screen, the screen in the light theme, and the
window beside the output path.

```sh
cargo run --manifest-path tests/ui-preview/Cargo.toml --profile fast -- target/agents.png 1280 800 verify-agents
```
