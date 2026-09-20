# Lightweight native desktop

The design uses the existing Rust/Slint stack, shared vector icons, semantic
colors, and pre-rendered wallpapers. No browser runtime, additional background
service, or external font dependency is added.

## Interaction and layout

- The shared app header keeps frequent actions visible and puts remaining
  commands in a named More menu. It must support pointer, arrows, Enter, Escape,
  disabled/loading guards, and compact windows.
- Notes gives the document the main working area. Library and Assistant are
  explicit controls. The assistant starts collapsed; opening it on a small
  window hides the library to preserve writing space. Reopening the library
  hides the assistant at compact widths; shrinking a wide window also retains
  only one side panel.
- Shell window captions stay visible when the content has no window controls.
  Files calls the home directory Home.
- Empty project rails do not occupy the overview at laptop widths. Settings
  exposes Everyday and Overview modes and persists the choice.

## Resource decisions

- No continuously animated desktop particle layer or procedural backdrop.
  Wallpapers remain static PNGs; hover/focus feedback remains local to controls.
- Terminal uses a steady focused cursor instead of an infinite opacity animation.
  The old idle Terminal measured 180.06% of one CPU core and 99,294 KiB PSS in a
  10.05-second sample. This is a diagnostic sample, not a benchmark: the shell
  restarted independently during this work, and a matched post-build run is
  required before reporting an improvement.
- Deployment artifacts use the existing stripped release profile with Thin LTO.
  Fast development binaries are for iteration only.

## Validation evidence

`tests/ui-preview` contains native software-renderer interaction probes and
responsive fixtures. `verify-overflow` exercises the real shared header's
disabled menu entry, arrow/Enter activation, last action, and Escape dismissal.
`verify-idle` checks for requested Terminal redraws after its initial layout
settles. Existing control, launcher and app probes remain part of validation.

The local probes passed on September 19: overflow, shared controls, launcher,
themes/wallpapers, file-grid hit targets, and Notes panel switching/resizing with
Save still reachable. Terminal requested zero redraws during the 1.5-second
idle observation after settling. The desktop modes, Notes, Files and Settings
were rendered at 1280×800 and 800×600, and Notes was also reviewed in light mode.
Run `bash tests/ui-preview/validate.sh` to reproduce the checks and images.

Reviewed native renders, using sample data (not screenshots of the deployed VM):

- [Everyday desktop](previews/lightweight-desktop-2026-09-19.png)
- [Notes](previews/lightweight-notes-2026-09-19.png)

The final optimized release build passed for the shell and all 16 standalone
apps. All 17 ELF executables are stripped. The three shared-header contract tests
passed. The local package is
`target/ui-candidate-20260919T175115Z/native-ui.tar.gz` (234,896,369 bytes), with a
source/binary manifest, individual binary hashes, build/test logs, and an archive
hash alongside it. Every binary was read back from the archive and verified
against its SHA-256 hash. This candidate has not been deployed.

These probes do not prove compositor behavior, file persistence, console display
freshness, or all application workflows. Before deployment, confirm exclusive
use of VM 520, check unsaved work, retain rollback binaries, then exercise real
mouse/keyboard workflows and compare CPU/PSS with the same windows open.
