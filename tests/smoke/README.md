# Smoke checks for the control surface

Written by Hermes Agent on 17 September 2026, driving this desktop as its mind, and kept because
it is the first thing that can check an installed machine without a person at the keyboard. It is
the starting point for the release gate in
[#26](https://github.com/yantrikos/yantrik-os/issues/26), not the gate itself.

Everything here is Python 3 standard library and talks to the machine only through
`/opt/yantrik/bin/yos`.

| File | What it is |
|---|---|
| `check_desktop.py` | The check that matters: describe conformance, launching every app twice, and a small allowlist of actions. Writes `check_results.json`. |
| `build_inventory.py` | Walks `yos ls` and `yos describe <app> --full` into `inventory.json`: every surface, its actions, their grades and required arguments. |
| `smoke.py` | The first attempt, kept for its pure functions and their tests. See the finding below for why its rule could not work. |
| `test_smoke.py` | 21 unit tests over those pure functions. `python3 -m unittest discover` in this directory. |
| `REPORT.md`, `check_results.json` | The run of 17 Sep on VM 520, build v0.1.0-179-g6fc8b13. |

## Run it

```sh
python3 check_desktop.py     # on the machine, against the live surface
```

## What the first run found

15/15 launch checks passed and 5/5 allowlist actions passed — that is the regression test for the
two bugs fixed that morning: apps listed that could not open, and a second open of a running app
recorded as a failed launch. All 57 actions across 10 app surfaces declare a grade.

Two findings worth keeping:

- **`yos ls` mixes apps and services in one flat list** with nothing to say which protocol each
  speaks. `a11y`, `companion` and `harness` refuse `app.describe` — correctly, they are services —
  but a caller cannot tell that until it asks and is refused.
- **The `safe` tier is degenerate.** Exactly one action on the whole machine is graded `safe`
  (`shell check_update`), and it requires an argument. A test restricted to safe, argument-free
  actions therefore touches nothing and passes — which is what the first version of this suite did,
  and why `check_desktop.py` replaced it.
