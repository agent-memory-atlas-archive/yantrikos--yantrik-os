# Asking the person

21 September 2026. Built after a live drive in which somebody typed *"Delete the dentist
appointment from my calendar."*

The mind did everything right. It listed the apps, described the calendar, found the event, and
called `os_act calendar delete_event {"id": "evt-3"}`. The MCP bridge refused it: `delete_event`
is graded `sensitive` and the bridge's ceiling is `standard`. The grade was correct. The refusal
was correct. And then the mind told the person:

> approve it right here in the chat panel (a /approve prompt should appear), or raise
> YOS_MCP_MAX_PERMISSION in the settings

No such prompt existed. The model had not hallucinated a capability so much as described the one
that obviously ought to exist — Hermes has a `/approve` for its own shell commands, and nothing on
this desktop had anything like it. Somebody asked for an ordinary thing and was told to set an
environment variable.

The missing piece was never a policy. Every layer already had one, and they were right. What was
missing was **a way to ask**.

This is `design/next-focus-2026-09.md` §3, built at its stop rule: *"if constrained run grants
pass the timebox, ship once-only exact-argument grants."* Runs do not exist yet (§1 is unbuilt), so
a grant binds to arguments rather than to a run, and dies with the shell rather than with a run.

## The threat model, in a paragraph

Four things can go wrong with an approval and three of them are not about permissions at all.
**A mind approving itself** is the first: if anything reachable over `app.act` can grant, the card
is theatre, so there is no such action and a test reads the source of every control module to keep
it that way — the only path to a grant is a Slint callback that a click arrives on. **Replay** is
the second: a grant that can be spent twice is a standing permission with extra steps, so a grant
is consumed exactly once and a second attempt authorises nothing. **Argument swap after approval**
is the third and the subtlest: get `delete_event id=evt-3` approved and then run `id=evt-99`, or
get a file read approved and change the path — so a grant is bound to the canonical JSON of the
exact arguments the card displayed, and any value change invalidates it (key order does not,
because nobody reads JSON key order and the transport does not preserve it). **Approval fatigue**
is the fourth and the one that actually kills these systems: a person trained to click Allow on a
stack of cards will click Allow on the one that mattered. So at most three requests wait at once, a
repeat of an identical pending question reuses its card instead of stacking a second, a denial
silences the identical request for two minutes, and the Allow button is deliberately *not* the
bright inviting one — Deny is the ordinary filled button and Allow is an outline you have to aim
at.

Two things are known and not defended against, and are worth saying out loud. The requester states
its own `grade` and `purpose` when it raises a request, and could understate either — that cannot
raise privilege (the machine ceiling is enforced inside the target app, from the app's own
published grade, whatever the card said) but it could mislead a person's judgement. The mitigation
is that the card always shows the real `app.action` and every argument verbatim, which is what a
person actually judges. And the requester names itself; nothing authenticates that name, because
nothing on this socket has an identity yet (issue #43).

## What is built

### The store — `crates/yantrik-ui/src/approvals.rs`

In memory, in the shell, which is the process that owns the screen and the person's attention.

- A **request** is `{id, requester, app, action, args, canonical, grade, purpose, created}`. It
  expires unanswered after **120s**.
- A **grant** is created only by `grant()`, which is `pub(crate)` and has exactly one caller: the
  Allow callback. It is single-use, bound to the exact `(app, action, canonical-JSON args)`
  triple, and expires **60s** after being given if nobody spends it.
- `consume()` succeeds at most once and its refusals name the part that differed — app, action, or
  arguments — because a caller told only "no" retries the same thing.
- Expiry is derived on every read, never stored. A request cannot be alive merely because nothing
  looked at it.
- At most **3** pending; identical pending questions dedupe to one card; an identical question
  denied within **120s** is refused rather than shown again.

### The surface — `crates/yantrik-ui/src/control_approvals.rs`

Three actions on `shell`, all graded `safe`, and they are safe for the same reason: **none of them
decides anything.**

| action | answers | why `safe` |
|---|---|---|
| `request_approval(app, action, grade, args_json?, purpose?, requester?)` | `{request_id, status:"pending", expires_in_secs, next}` | puts a question on screen |
| `approval_status(request_id)` | `{request_id, status, age_secs, expires_in_secs?}` | reads an answer somebody else gave |
| `consume_approval(request_id, app, action, args_json?)` | `{request_id, consumed:true, authorises}` | spends a grant; can only make it worth less |

`status` is one of `pending | granted | denied | expired | consumed`. The fifth is not in the
original sketch and is deliberate: a caller that polls after spending its grant asked a real
question, and "the grant you were given has been used" is the true answer. Reporting `granted`
would invite a replay that would be refused anyway; reporting `expired` would be false.

`describe shell` gained two keys:

- `pending_approvals: [{id, requester, app, action, grade, age_secs}]` — so a second mind, or a
  test, can tell "waiting for a person" from "hung". Seeing a request forges nothing: consuming
  still needs a grant that only a click creates.
- `tool_permission: "<grade>"` — the machine's standing ceiling, read through
  `yantrik_app_runtime::control::configured_ceiling()`, the same function the runtime enforces
  with, from the same file. Not `ui.get_settings_tool_permission()`, which is the Settings
  screen's copy and would be one save behind.

### The card — `crates/yantrik-ui-slint/ui/components/intent_lens.slint`

`ApprovalRequest` and `ApprovalCard` live beside `LensResult` because the card's home is the
conversation. It says, in plain words: who is asking, `app.action`, the action's own published
`purpose` (the thing commit `d73760d` taught `yos describe` to print), every argument as a
`key: value` line, the grade, and — for `dangerous`, or for any purpose that says the action cannot
be undone — a warning line in red. Buttons: **Deny** and **Allow once**.

It is drawn in two places:

- **In the Lens**, between the transcript and the reply box, when the Lens is open in chat mode.
  Not interleaved in the message ListView: a request is not a message, it arrives while somebody
  is reading, and a card that scrolls away with the transcript is a card that gets missed.
- **Over whatever screen is up**, from `app.slint`, when the Lens is not showing it. The Lens lives
  on the desktop screen and is closed most of the time; a request nobody is shown expires in two
  minutes, which from the person's side is indistinguishable from the machine ignoring them —
  which is the bug this whole feature exists to fix. Suppressed on the boot, onboarding, lock and
  login screens: a card that can be allowed from a locked screen is a way to act on a machine
  without unlocking it.

After a decision the card is replaced by a one-line record that stays in the Lens's conversation:
`Allowed once: calendar.delete_event — 12:03`.

**Keyboard: Enter does not allow anything.** Both buttons are `TouchArea`s, which take no keyboard
focus in Slint, so there is no default action and no key reaches them. Allowing costs a deliberate
pointer movement onto the word Allow.

### The bridge — `deploy/yantrik-os/yos-mcp`

`guard_act` used to be a wall and is now a question. The ladder below it is unchanged.

```
grade, purpose = action_detail(app, action)      # from `yos describe`, signature + the line under it
grade is None                    -> refuse (unchanged: an ungradeable action is not run)
grade <= YOS_MCP_MAX_PERMISSION  -> run, nobody asked
grade >  machine tool_permission -> refuse WITHOUT asking
otherwise                        -> request_approval, poll ~110s, on granted consume then run
```

`effective_args()` is the part that is easy to get wrong. `os_act` renders every value to text for
`yos act k=v`, and `yos` parses each one back with `json.loads` — so a model sending `{"id": "3"}`
means the app receives `3`. Binding the grant to what the model typed rather than to what the app
will run with would mean the card showed one thing and the machine did another. The round trip
happens once, in the bridge, and the same dict is what the card shows, what the grant binds, and
what the call sends.

Every outcome is reported to the mind in words it can relay honestly:

| outcome | what the mind is told | what ran |
|---|---|---|
| granted | "The person at the machine was asked and allowed this once, just now, for exactly these arguments. The grant has been spent…" — carried back **in front of** the action's own output, on success and on failure | the action, once |
| denied | "…was asked to allow `calendar.delete_event` and said no… This is an answer, not an error: do not ask again and do not look for another route to the same effect." | nothing |
| expired / no answer | "…did not answer within 110s… They were probably away from the keyboard. Tell them what you were trying to do; they can ask you to try it again." | nothing |
| above machine ceiling | "…this machine does not allow callers like this past `standard` (`tool_permission`, on the AI page in Settings). The person was **NOT asked**, because nothing they could answer would let it run…" | nothing, and nobody was disturbed |
| no shell | "…it needs the person at the machine to allow it — and they could not be asked: `<reason>`. Nothing was run and nothing was changed." | nothing |
| grant could not be spent | "the person allowed …, but the grant could not be spent: `<reason>`. Nothing was run." | nothing |

### Timeouts

The wait happens in the bridge process, between the tool call arriving and `yos act` running — so
raising the `os_act` subprocess timeout would not have covered it, and would only have delayed
noticing a genuinely hung desktop. Named constants instead:

| constant | value | what it bounds |
|---|---|---|
| `ACT_TIMEOUT` | 60s (unchanged) | the `yos act` subprocess |
| `SHELL_CALL_TIMEOUT` | 20s | one `yos describe` / `yos act shell …` |
| `APPROVAL_WAIT` | 110s (`YOS_MCP_APPROVAL_WAIT`) | waiting on the person |
| `OS_ACT_MAX_SECONDS` | 250s | **what an MCP client must allow for one `os_act`** |

`APPROVAL_WAIT` is deliberately under the shell's 120s request lifetime: a bridge that gave up
later than the shell would report "no answer" for a request the person had just allowed.

**Open:** Hermes' own per-tool-call MCP timeout is not in this repo. `harnesses/hermes/` is the
desktop *adapter*, whose only deadlines are `PICKUP_SECONDS` (180s, raised on 09-17 by somebody
else's uncommitted change — untouched) and `ABANDONED_QUIET_SECONDS` (90s), both of which already
accommodate a 110s wait. Whatever Hermes uses to bound an MCP `tools/call` has to allow
`OS_ACT_MAX_SECONDS`; if it is 40s or 60s today, a person will be cut off mid decision and the
mind will report a timeout for a machine that was working correctly.

## What is deliberately NOT built

- **Persistent "always allow".** There is no way to record a standing yes. The standing policy on
  this machine is `tool_permission`, set at the keyboard in Settings, and it belongs there — one
  place, owned by the owner, enforced in the dispatch every `app.act` crosses. A second standing
  permission minted by a card is a second place for the truth to live.
- **Run-bound grants.** §3's first shape. A grant would die with the run and be rechecked against
  the live grade at execution — but runs (§1) are not built, so binding to them would mean
  inventing a second id namespace, which §1's stop rule forbids. Arguments are what a person
  actually reads, and they are what the grant binds.
- **Persistence across a shell restart.** The store is memory. A grant that survives the thing that
  was asking is a grant nobody remembers giving.
- **Raw-shell analysis.** Hermes' own `/approve` for shell commands stays Hermes' business. This is
  only about structured, OS-published actions, where the OS already knows the grade and the app
  already publishes what the action does.
- **Re-checking the grade at execution.** The app re-reads its own grade inside `app.act` anyway
  and refuses above the machine ceiling regardless of any approval, so the failure §3 worried about
  is covered one layer down rather than here.
- **Any authentication of the requester name.** Nothing on this socket has an identity yet.

## Verifying it on the machine

Everything below runs on the VM, as the desktop user, with
`XDG_RUNTIME_DIR=/run/user/1000 WAYLAND_DISPLAY=wayland-0`.

**0. The machine's standing policy allows a `sensitive` action at all.**

```sh
yos describe shell | grep tool_permission
#   "tool_permission": "sensitive"
```

If that says `standard`, step 2 will refuse without asking, which is case (c) below — set it to
`sensitive` on the AI page in Settings to see the card.

**1. The thing that used to fail.** With the bridge's own ceiling at its default `standard`:

```sh
yos act shell open_app name=calendar && sleep 3
yos describe calendar          # find an event id
```

Then, as the mind would, through the bridge:

```sh
printf '%s\n' \
 '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
 '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"os_act","arguments":{"app":"calendar","action":"delete_event","args":{"id":"<id>"}}}}' \
 | /opt/yantrik/bin/yos-mcp
```

It will block. **A card appears on screen within a second, over whatever screen is up.**

**2. Confirm the card exists without looking at pixels**, from a second shell:

```sh
yos describe shell | grep -A10 pending_approvals
#   "pending_approvals": [
#     {
#       "id": "appr-1",
#       "requester": "the mind on this desktop",
#       "app": "calendar",
#       "action": "delete_event",
#       "grade": "sensitive",
#       "age_secs": 4
#     }
#   ],
```

**3. Press Allow.** With a person at the machine: click it. To drive it from a script, the desktop
already ships `wlrctl` (the shell uses it to raise itself), and `wlrctl pointer move` is relative,
so pin to the corner first:

```sh
W=$(wlr-randr | awk '/current/ {split($1,a,"x"); print a[1]; exit}')
grim /tmp/card.png            # look at it, and read the Allow button's y off the image
wlrctl pointer move -10000 -10000        # pin the pointer to the top-left corner
wlrctl pointer move $((W - 121)) <y>     # x is deterministic; y is not
wlrctl pointer click left
```

The x is exact and does not need reading: the overlay's inner column is `404px` wide starting
`420px` from the right edge, with `12px` padding, and the two buttons split the remaining `380px`
with `8px` between them — so **Allow's centre is `W - 121`**. The y is not exact, because the card's
height depends on how many lines the purpose and the arguments wrap to; the button row is the last
thing in the card, its centre sits `28px` above the card's bottom edge, and the card's top is at
`y = 48`. Read it off `/tmp/card.png` once; it is stable for a given action.

**Expected:** the blocked `yos-mcp` returns within a couple of seconds, and its text begins

```
The person at the machine was asked and allowed this once, just now, for exactly these
arguments. The grant has been spent and cannot be used again …
```

followed by the calendar's own reply, and the event is gone from `yos describe calendar`. The card
is replaced, in the Lens's conversation, by `Allowed once: calendar.delete_event — HH:MM`.

**4. The other four outcomes.**

  a. **Deny** — repeat step 1 with a different event id, press Deny. The bridge returns "said no",
     `isError` is true, and `yos describe calendar` still has the event. Repeat the *identical*
     call within two minutes: it is refused without a second card ("denied … a moment ago").
  b. **No answer** — repeat step 1 and walk away. After ~110s it returns "did not answer within
     110s", nothing ran, and the card disappears from the screen and from `pending_approvals` at
     120s.
  c. **Above the machine ceiling** — set `tool_permission: standard` on the AI page in Settings,
     then repeat step 1. **No card appears at all**, and the refusal names `tool_permission`. This
     is the important one: an approval must not be able to exceed the owner's standing policy.
  d. **Flood** — fire four `delete_event` calls with four different ids at once. Three cards; the
     fourth is refused with "already waiting".

**5. The invariant, from the outside.** Nothing on the shell's surface can grant:

```sh
yos describe shell --brief | grep -iE 'approve|grant|allow|deny'
#   act: request_approval(app, action, grade, args_json, purpose, requester)  [safe, settles on return]
#   act: approval_status(request_id)  [safe, settles on return]
#   act: consume_approval(request_id, app, action, args_json)  [safe, settles on return]
```

(`--brief` drops the `?` that marks an optional argument; the full `yos describe shell` shows
which of those are optional, and the sentence under each action.)

Three, all `safe`, none of which decides anything. Try to authorise one from the socket:

```sh
yos act shell consume_approval request_id=appr-1 app=calendar action=delete_event
#   refused: `appr-1` has not been answered yet; nothing was authorised.
```

## Tests

| where | what |
|---|---|
| `crates/yantrik-ui/src/approvals.rs` | 15 tests: single use, duplicate consume authorises nothing, key order does not invalidate, any value change does, a changed app or action does, denial prevents consumption, request expiry, grant expiry, flooding refused, identical question dedupes, denial silences a repeat, unknown id is not an expiry, canonical JSON, the card shows what the grant binds, the decision leaves a record line |
| `crates/yantrik-ui/src/control_approvals.rs` | 3 tests: no published action can grant (reads the source of every `control*.rs`), the three asking actions are still published (so deleting the feature cannot make the first one pass), `args_json` binds identically whether it arrives as an object or a string |
| `deploy/yantrik-os/yos-mcp-selftest.py` | 29 checks against a fake `yos`: below-ceiling runs unasked, the five outcomes each produce their own message and only one of them runs anything, the grant binds the coerced value the app will actually receive, an argument swapped after approval spends no grant and runs nothing, and the bridge's wait is shorter than the shell's request lifetime |

```
cargo test --offline --profile fast -p yantrik-ui --bin yantrik-ui approvals
python3 deploy/yantrik-os/yos-mcp-selftest.py     # needs Linux; the fake yos is an exec script
```

## Still open

- Hermes' MCP `tools/call` timeout (see Timeouts above) — not in this repo, must allow 250s.
- Nothing authenticates the `requester` name on a card. Issue #43.
- The card cannot show a diff or a preview of what the action will do — only its published purpose
  and its arguments. For `delete_event` that is enough; for something like a bulk file operation it
  would not be, and `control_files`' actions should probably grow a "what this will touch" line
  before they are routinely put in front of a person this way.
