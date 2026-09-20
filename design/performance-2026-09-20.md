# Shell resource reduction — September 20, 2026

## Measured diagnosis

The inherited `SLINT_BACKEND=winit` value specifies a window backend, not a renderer. The previous selector treated it as an explicit rendering choice, bypassed GPU detection, and Slint selected software OpenGL on VM520. The animation policy simultaneously classified this generic name as CPU rendering. Live thread and memory profiles confirmed llvmpipe workers and loaded LLVM/Gallium libraries.

A same-binary A/B restart using `winit-software` reduced the settled Settings sample from 447.3 MiB PSS / 5.85% of one core to 252.9 MiB / 2.8%. First samples after startup were noisier due to asynchronous initialization; use the repeated Settings sample for the settled comparison. These are short real-machine observations, not laboratory benchmarks.

## Changes

- Backend-only `winit` and empty renderer settings go through hardware detection. Explicit choices such as `winit-femtovg`, `winit-software`, and `winit-skia` remain honored. `LIBGL_ALWAYS_SOFTWARE=1` chooses Slint's direct software renderer instead of software OpenGL.
- Settings uses generated 320×200 previews instead of decoding seven 1920×1200 wallpapers. Total decoded RGBA budget is 1,792,000 bytes rather than 64,512,000 bytes, a 97.2% reduction for those previews. Full-resolution desktop wallpapers are unchanged. Regenerate with `scripts/render-wallpaper-previews.py`.
- Polling retains unchanged window, pinned-app, service, and harness models, avoiding unnecessary list replacement. Changes in row data, order, count, or status still publish.
- The Settings Privacy “Review” button now opens the Permissions dashboard (screen 28); it previously pointed at Images (screen 11).
- Device and permission inventories scan on entry and refresh only while their dashboards are visible. Fast UI synchronization also stays scoped to those screens. Overlapping refresh workers are prevented; cached results stay available and a new visit requests fresh data.
- Process monitoring requests the PID/name/thread/CPU metadata it uses, rather than unused per-process memory, disk I/O, executable paths, and environment data. CPU monitoring skips frequency refreshes.

No new runtime dependencies, daemons, UI effects, or recurring workloads were added.

## Validation

- Six renderer-policy tests and a live process-monitor integration test passed. The latter starts and terminates an owned child and verifies start/stop events and valid CPU/memory readings.
- Two UI-model regression tests verify unchanged models stay intact and additions, removals, edits, and reordering remain visible.
- The production Settings keyboard, accent, retry, category, and scrolling probe passed. The new previews were visually inspected.
- Final shell `cargo check` passed before the optimized build.

## VM520 deployment and live verification

Deployed to `/opt/yantrik/ui-deployments/performance-20260920T050000Z`. The release was built offline with optimization; its source hashes were checked before packaging and its binary checksum was verified after installation. The previous binary and preferences are backed up in that deployment directory.

Binary SHA-256: `8f82fc5093643d5f3569d0bf68789163fadef114de247054bc002d0d55f9396d`.

The new shell is PID 131674. Compositor 72690, Terminal 96090, Editor 104775, and Notes 120098 survived the shell-only deployment. Settings preferences remained byte-for-byte unchanged throughout live verification.

The new process was deliberately launched with the original generic `SLINT_BACKEND=winit`. Logs confirmed automatic selection of `winit-software`; its loaded mappings and threads contained no LLVM, Gallium, or llvmpipe.

The repeated Settings sample measured **195.5 MiB PSS and 2.35% of one core**, compared with **447.3 MiB and 5.85%** before deployment: about **56% less attributable memory and 60% less idle CPU**. Both samples cover 20 seconds. This measures the shell process, including its internal monitoring and companion work, not the entire OS. Startup remains transient: the first new-shell sample reached 14.95% CPU while monitoring initialized. Memory also varies with views visited and retained caches.

Live checks covered Settings search/Enter/Escape, the Privacy-to-Permissions link, dashboard entry and re-entry, and current System readings. Permissions completed its scan; Devices populated 47 devices (9 USB, 27 PCI, 6 input, 4 storage, 1 display). A disposable Network window was found in the actual Slint window model and became unavailable there after closing. The direct test launch explicitly selected the software renderer, matching the environment the new shell supplies to child applications.

Screenshot: `design/previews/performance-vm-2026-09-20.png`.

A second pass after exercising both inventories and opening/closing the Network app confirms the saving persists with those caches populated:

| View | PSS (MiB) | CPU (% of one core, 20 seconds) |
| --- | ---: | ---: |
| Settings | 203.1 | 1.80 |
| Desktop | 222.9 | 1.50 |
| Files | 203.1 | 1.10 |
| Settings, repeated | 203.1 | 1.45 |

The short samples establish a material reduction, not a precise universal percentage. Representative Settings memory is now about 55% below the original sample. Raw selected measurements are retained in `design/performance-measurements-2026-09-20.json`. Further work can target startup monitoring and the remaining roughly 200 MiB shell footprint.
