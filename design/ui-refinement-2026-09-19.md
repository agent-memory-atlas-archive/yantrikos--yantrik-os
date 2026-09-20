# Desktop and shared app frame refinement

The default desktop now has a clear starting point: greeting, a prominent Lens
entry, a responsive grid of the person's pinned apps, recent items, and a compact
companion entry. These use the existing shell callbacks and models. No synthetic
activity, suggestions, or system statistics are added to the product.

The visual system uses graphite surfaces, brighter secondary text, clearer edges,
app-specific icon tiles, and contrasting foregrounds on accent buttons in both
appearances. Light appearance adds a veil over wallpaper so dark desktop text
remains readable. Existing wallpaper assets and animation budgets are retained.

The launcher adapts its columns to the available width. Search takes focus on
open; arrows select results, Tab traverses results even while typing, Enter opens
the selection, and Escape closes. Empty results cannot launch anything. Opening
the launcher resets the visible and backing filters together. The taskbar's
window list scrolls independently of its launcher and companion controls.

Apps using the shared `AppHeader` inherit its changes. Titles elide, search shrinks,
trailing actions become icon controls in compact windows, and overflow stays in
a bounded strip. A fixed viewport separates responsive contents from split-pane
intrinsic size calculations. The standard header height stays at 48px.

Shared buttons now expose button semantics and labels, accept Enter/Space, show
focus, and reject activation while disabled or loading, including immediately
after disabling a focused control. Disabled text fields also disable their clear
and reveal affordances. Loading buttons retain their label.

Agent mode keeps its existing workflow, with rails yielding space at 1000px and
1360px breakpoints. Its primary working area stays usable at 800×600.

`tests/ui-preview` is the reproducible native visual/input harness. It renders the
actual desktop and shared components using Slint's software renderer and sample
data. It is not evidence of an installed VM deployment, compositor behavior, or
end-to-end app/service integration. See its README for rendering and input checks.

Validation on September 19:

- `cargo check --offline --profile fast -p yantrik-ui` passed for the final tree.
- The three shared app-header contract tests passed.
- Native keyboard/pointer probes passed for button activation, disabled/loading
  states, input clearing, launcher arrows/Tab, reopening, empty results and Escape.
- Software-rendered desktop, launcher and frame scenes were inspected in dark and
  light appearances, including 800×600, 1280×800 and the 1600×900 agent desktop.
- `git diff --check` passed. No VM deployment was performed.

App interiors follow-up:

- Notes has a wider, collapsible library, roomier note cards, a document title and
  readable text measure. New and Save appear before secondary actions. Import
  remains available without an open note, while Save requires a selection.
- Files uses adaptive grid columns with explicit row arithmetic, aligned list
  columns, calmer selection treatments, and contextual empty states. Unavailable
  Back/Forward actions are disabled. The sidebar scrolls and detail text yields
  space at compact widths.
- Settings uses the common header, a scrolling category list, keyboard-accessible
  appearance choices, and thumbnails of all seven actual built-in wallpapers.
  The previously unused 16ms animation timer is removed.
- Other apps inherit the shared controls and theme; their individual screens have
  not received this same app-specific redesign.

App validation:

- Full shell and standalone Notes compile with
  `cargo check --offline --profile fast -p yantrik-ui -p yantrik-notes`.
- A responsive Notes sidebar initially exposed a preferred-size binding loop in
  the standalone window. A noninteractive viewport isolates its content sizing;
  the subsequent integration build has no layout-cycle warnings.
- The native `verify-apps` probe passes theme selection and idempotence, Enter on
  wallpaper choices, disabled Forward navigation, and file-grid hit targets before
  and after a 1280-to-800-pixel resize.
- Actual NotesEditor, FileBrowser and SettingsScreen renders were inspected in
  dark and light appearances, with sample content at 1280×800 and 800×600.
- These checks cover layout and UI callback dispatch, not live persistence or
  compositor/end-to-end service behavior. No app or shell was deployed to the VM.


Deployment completed on September 19, 2026:

- Target verified against the Proxmox cluster: node1, VM 520 (`yantrik-os`),
  guest `192.168.4.44`. This is the live desktop, not the WSL session.
- Built and installed the shell plus all sixteen standalone apps using the `fast`
  development profile. The renderer dependencies remain optimized. This is a VM
  deployment of the local UI working tree, not a published release-channel update.
- Deployment ID: `ui-20260919T095021Z`. All seventeen installed binaries match the
  staged SHA-256 manifest, and all dynamic libraries resolve on the guest.
- Previous binaries and their checksums are retained under
  `/opt/yantrik/ui-deployments/ui-20260919T095021Z/backup/` on the guest.
- This VM starts the desktop through `getty@tty1` and `.bash_profile`; it has no
  `yantrik-session.service`. The tty1 session was restarted after verifying Notes
  had no unsaved edits and Terminal was idle. Configuration, documents, services,
  and mind installations were preserved.
- Verified the new running shell (PID 34084) and Notes (PID 34368) against their
  staged binary hashes. Hermes and Yantrik Mind reattached. No failed launches
  were reported. Files and Appearance settings opened on the running shell;
  Notes retained all six existing notes and opened an existing test note.
- Live VM captures are in `target/ui-vm-desktop.png`, `target/ui-vm-settings.png`,
  `target/ui-vm-files.png`, and `target/ui-vm-notes.png`. These are separate from
  the earlier headless sample-data previews. Build evidence is in
  `target/ui-deploy-build.log` and `target/ui-deploy-staged-sha256.txt`.
- View the result in Proxmox: node1 → 520 (yantrik-os) → Console. Notes is open;
  minimize it to see the updated desktop.

Deployment withdrawn after hands-on review (September 19, 2026):

- The user rejected the live result. The deployment checks above established
  executable integrity and API dispatch, but did not establish acceptable UX.
- Mouse interaction through the actual Proxmox console exercised the launcher,
  opening Files, entering a folder, launching Notes and taskbar/window switching.
  The new Notes header visibly clips secondary actions at its default window
  width. The default agent sidebar further constrains the writing area.
- Files has a blank window-caption row and labels the home folder `~`. These are
  observed defects, not all proven new regressions; the caption visibility rule
  was already present before this change.
- The browser console also showed stale regions: the guest's `grim` capture
  showed an open launcher while the browser still showed the desktop. Do not
  mistake every apparent overlap for a layout bug, or declare the console fixed
  based on a guest-only screenshot. A direct noVNC tab was also inspected.
- Restored all 17 original binaries from the deployment's `backup/bin` after
  checking Notes and the shell editor for unsaved changes. Both backup and
  installed checksum manifests passed. The restarted shell (PID 40129) matches
  the original binary; companion connectivity returned and no launch failures
  were reported. Notes still contains all six original notes.
- The candidate binaries remain in `staged/bin`, and the local source changes
  remain available. No source reset, document deletion, or service rollback was
  performed. The earlier deployment is no longer active.
- Before another deployment: remove toolbar clipping with accessible overflow,
  verify the actual persisted desktop mode, review populated side panels, and
  exercise opening, editing, saving, resizing, minimizing and restoring through
  the live console. This work is not yet at the requested desktop quality bar.
