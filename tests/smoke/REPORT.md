# Yantrik OS — smoke / conformance report

Source: `check_results.json` (run completed 2026-09-18 00:34 UTC, `yos` at `/opt/yantrik/bin/yos`).
Scope: 13 surfaces in `yos ls`, 57 actions inspected. Three checks: describe conformance, launch, allowlist.
Totals: **10/13 describe OK, 15/15 launch OK, 5/5 allowlist OK, 0 ungraded actions.**

---

## Verdict — which failures are real, which are expected

**Real OS-side issues (worth filing):**

- **`yos ls` mixes apps and services in one flat list with no kind/type field.** Apps (`app-*`), RPC services (`a11y`, `weather`, `network`, `notifications`, `system-monitor`), and the companion/harness embedded surfaces all appear as bare tokens. Nothing in the list tells a consumer which protocol to speak. This is the only genuine OS gap in the run — not a crash, a *discoverability* gap.
- **The `safe` action tier is degenerate.** Exactly one action in the whole OS is graded `safe` — `shell check_update` — and it (a) requires a `channel` argument and (b) is caught by a content guard (`update`). Net: there is **no** fire-and-forget, argument-free, low-risk action anywhere on this machine. A cautious harness that refuses to fabricate args and refuses to run anything above `safe` has **zero** actions to do.

**Expected given how the machine is set up (not bugs):**

- **The three describe failures (`a11y`, `companion`, `harness`).** These are services, not apps. They refuse `app.describe` because they speak their own protocols (`a11y.*`, `companion.*`, `harness.*`) — and each refusal *helpfully lists its own methods*. That is working-as-designed, not a defect.
- **`network` exposes 0 actions.** It is a read-only status surface ("online via ethernet"). Nothing to act on is fine.
- **`system-monitor`'s only action is `kill_process` (dangerous).** Monitoring is read + one destructive control; that is the expected shape.
- **`weather`'s only action is `set_location` (standard, 4 required args).** Expected for a location-keyed service.
- **15/15 launch and 5/5 allowlist passing.** Correct and as expected.

---

## Per-app tables

`check` = describe | launch | allowlist. Launch records carry no per-check timing, so seconds is `—` there.

### a11y
| check | result | seconds | error |
|---|---|---|---|
| describe | **FAIL** | 0.066 | `a11y.app.describe refused: unknown method `app.describe`; this service serves a11y.windows, a11y.describe, a11y.act, a11y.status` |

### companion
| check | result | seconds | error |
|---|---|---|---|
| describe | **FAIL** | 0.057 | `companion.app.describe refused: unknown method `app.describe`; this service serves companion.ask, companion.recall, companion.status, companion.tools, companion.tool, companion.submit, companion.await, companion.jobs, companion.cancel` |

### harness
| check | result | seconds | error |
|---|---|---|---|
| describe | **FAIL** | 0.056 | `harness.app.describe refused: unknown method `app.describe`; this service speaks: harness.attach, harness.poll, harness.chunk, harness.complete, harness.fail, harness.detach` |

### notes
| check | result | seconds | error |
|---|---|---|---|
| describe | PASS | 0.080 | — |
| launch | PASS (3/3) | — | first_open, second_open, still_answering all OK |
| allowlist `new_note` | PASS | 0.055 | — |
| allowlist `save` | PASS | 0.083 | — |
| allowlist `search` | PASS | 0.062 | — |

### calendar
| check | result | seconds | error |
|---|---|---|---|
| describe | PASS | 0.062 | — |
| launch | PASS (3/3) | — | — |
| allowlist `go_to_today` | PASS | 0.062 | — |

### email
| check | result | seconds | error |
|---|---|---|---|
| describe | PASS | 0.075 | — |
| launch | PASS (3/3) | — | — |

### terminal
| check | result | seconds | error |
|---|---|---|---|
| describe | PASS | 0.064 | — |
| launch | PASS (3/3) | — | — |

### download-manager
| check | result | seconds | error |
|---|---|---|---|
| describe | PASS | 0.082 | — |
| launch | PASS (3/3) | — | — |

### shell
| check | result | seconds | error |
|---|---|---|---|
| describe | PASS | 0.071 | — |
| allowlist `refresh_apps` | PASS | 0.073 | — |

### network
| check | result | seconds | error |
|---|---|---|---|
| describe | PASS (0 actions) | 0.058 | — |

### notifications
| check | result | seconds | error |
|---|---|---|---|
| describe | PASS | 0.067 | — |

### system-monitor
| check | result | seconds | error |
|---|---|---|---|
| describe | PASS | 0.080 | — |

### weather
| check | result | seconds | error |
|---|---|---|---|
| describe | PASS | 0.052 | — |

---

## The three describe failures, plainly

`a11y`, `companion`, and `harness` are **services, not apps**. `a11y` is a dedicated binary (`a11y-service`, running, pid 846); `companion` and `harness` are RPC-live embedded surfaces with no dedicated binary at all. None of them implements the `app.describe` method that check 1 assumes — each speaks its own verb family and *tells you so* in the refusal.

`yos ls` lists all three next to `app-notes`, `app-shell`, etc., in one flat list, with nothing to tell them apart. So the same verb (`yos describe … --full`) works on the apps and is refused on the services.

**Which of the three is at fault — the listing, the services, or the test's expectation?**

Mostly the **test's expectation**, secondarily the **listing**:

- **Not the services.** They are doing exactly the right thing. Each exposes a coherent, self-describing protocol and even returns its own method list in the refusal. That is *better* than a silent `rc 0` with garbage, and arguably the correct behaviour for a surface that genuinely has no `app.describe`.
- **The test's expectation is the primary defect.** `check_desktop.py` hardcodes the premise that `app.describe` is the universal describe verb for everything in `yos ls`. The OS plainly has at least three protocol families (`app.*`, `a11y.*`, `companion.*`/`harness.*`). A conformance check that assumes one protocol for all is testing a uniformity the OS never claimed. The check should either scope the `app.describe` contract to the `app-*` family, or discover each surface's protocol first.
- **The listing has a real, but minor, gap.** Putting apps and services in one unannotated list means a consumer can't tell from `ls` alone which verb to use. That is a legitimate discoverability complaint — the closest thing in this run to a "real" OS bug.

If I must name one owner: **the test's expectation**, with a secondary, defensible note against the listing. The services are clean.

---

## Grades on this machine

All 57 inspected actions declare a grade (0 ungraded). Distribution across every surface:

| grade | count | where |
|---|---|---|
| **safe** | **1** | `shell check_update` — **requires `channel`**; also matches the `update` content guard |
| **standard** | **50** | shell 19, notes 7, download-manager 9, email 6, calendar 5, notifications 2, terminal 1, weather 1 |
| **sensitive** | **3** | shell `lock`, download-manager `cancel`, terminal `run` |
| **dangerous** | **3** | shell `apply_update`, shell `files_delete`, system-monitor `kill_process` |
| **total** | **57** | |

**What this means for anyone testing this OS:** the `safe` tier — the only grade a conservative harness would auto-run — is **empty in practice**. The single `safe` action needs an argument (so it is not fire-and-forget) *and* trips a content guard, so a harness that "runs only `safe`, no-arg, non-forbidden actions" invokes **zero** actions on this machine. The genuinely usable low-risk surface is the **standard** tier (50 actions), but that is one notch up, and the large majority of those *also* require at least one argument (URLs, paths, recipients, lat/lon, titles). In short: **there is no argument-free, safe-grade action anywhere, so a cautious test has nothing to do out of the box** — it must be willing to supply arguments (accepting real side effects) or it will exercise the OS not at all. That is the single biggest practical testing constraint this surface presents.

---

## What I would test next — and can't test today

I would next **verify behaviour, not just presence**: does `notes.save` actually persist, does `calendar.add_event` create a real event, does `download-manager.add` start a real transfer, does `weather.set_location` change the reply — i.e. state-change and round-trip assertions across the 50 standard actions. I cannot do that today for three reasons. First, the **sanctioned scope was read-only probes + `shell open_app` + a fixed 5-action allowlist**; I ran exactly that and nothing more. Second, most standard actions **require fabricated arguments that map to real side effects** — a real download (needs a URL), a real file write, a real email compose (and email is *"no account configured"* here anyway) — so exercising them would act on this live machine, not a sandbox. Third, **two whole protocol families (`companion.*`, `harness.*`) and `a11y` can't be reached at all** by this harness: it only speaks `app.describe`/`app.act`, and a proper test would need a client for those RPC methods. I would also test the **destructive tier** (`files_delete`, `apply_update`, `kill_process`, `lock`, `terminal.run`) and the **error paths** (bad arg types, missing socket, unknown app) — all of which I deliberately did not touch, both because they are genuinely destructive on the machine I'm running in and because I only ever saw the happy path (every launch and allowlist check passed).
