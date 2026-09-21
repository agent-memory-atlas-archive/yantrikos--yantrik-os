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
nothing on this socket has an identity yet (issue #43) — so the card prints
`self-declared name` directly under it, rather than letting a flattering label pass as a fact.

### Where the name comes from

The first cards read "the mind on this desktop is asking to use this machine", which is true and
useless — the person could not tell Hermes from anything else that had opened the bridge. Three
sources, best first, and all three are self-declared:

1. `YOS_MCP_REQUESTER` — an operator setting this deliberately outranks anything inferred.
2. The MCP client's own `clientInfo` from the `initialize` handshake. Hermes sends its name and
   version there and the bridge was throwing it away; it is now kept, purely as a label.
3. The shell's own `minds[]`, whichever is `answering` — the name the person already sees in the
   status bar, for a client that sends no `clientInfo` at all.

Failing all three it says `an unnamed caller`, which is worse to read and better than inventing
something.

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

Only **one** card is on screen at a time even though three requests can be waiting, with a
`N more waiting — this one first` line under it. Three stacked cards are 780px on an 800px screen,
which puts the third one's buttons under the taskbar; and a person facing a stack reads none of
them properly, which is the failure this design is most afraid of.

Every text on the card is bounded in `approvals.rs`, so its height is arithmetic rather than a
measurement: one line per argument (`ARG_VALUE_CHARS` = 60, each cut value naming its true
length), at most `ARG_ROWS` = 8 arguments followed by a line saying how many more the grant still
covers, and a purpose clipped at `PURPOSE_CHARS` = 240.

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

**The shell comes forward when a card goes up, and gets out of the way afterwards.** Drawing a
card is not the same as being seen: the shell is an ordinary toplevel to labwc, so a card drawn
while another app is focused is a card behind that app. `take_the_screen()` reads which toplevel
is in front, then asks the compositor for the shell — the same `wlrctl toplevel focus
title:Yantrik OS` that `open_lens` has needed since the Lens once opened underneath Notes, on a
worker thread for the same reason. When nothing is waiting any more — a decision *or* an expiry —
`give_the_screen_back()` refocuses the window it noted.

Only a *fresh* request raises the shell. A repeat of an identical pending question hands back the
card already on screen; raising again for it would let anything that can call a `safe` action hold
somebody's screen by asking the same thing in a loop.

"Which window was in front" is not something the shell tracks — `wlrctl toplevel list` carries no
focus flag, and `wire::timers`' "first in the list is the foreground window" is reading an
ordering that means nothing. So `window_in_front()` asks wlrctl's own `state:activated` matcher
and trusts it **only when it answers with exactly one line** that is not the shell. Two lines or
none means either nothing is activated or this wlrctl does not support the matcher and has listed
everything; both are "not knowable", and the shell then stays in front rather than throwing the
person into a window they were not in.

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

## What the first run on a real machine found

Shipped as `315663d` in `v0.1.0-241`, and the flow worked end to end — request, card, click,
grant consumed, `delete_event` ran, event gone. Two defects, both of them about the card being
*present* rather than about it being *right*, which is its own lesson: the tests all asked whether
the machinery was correct and none of them asked whether a person could see it.

**The card was drawn behind the focused app.** Calendar was open and focused; the card was drawn
in the shell's window, top right, and Calendar covered it completely — only the card's orange
border showed past the edge. The request would have expired with the person never knowing they
had been asked. The mechanism was already written down one function away: `open_lens` in
`control.rs` spawns `wlrctl toplevel focus` precisely because the Lens once opened underneath
Notes. Fixed above; the consequence (the shell covers what they were using) is handled by
recording the window in front and giving the screen back when nothing is waiting.

**The top of the card was clipped off the screen.** The header, the action name and the purpose —
the part that says what is being approved — rendered above `y = 0`, leaving the person looking at
three arguments and two buttons.

The cause was `height: self.preferred-height` on `ApprovalCard`'s root. Both of its branches are
conditional (`if !waiting` for the record line, `if waiting` for the card), and **a Rectangle whose
only children are conditional reports a `preferred-height` of zero** — Slint computes a non-layout
element's preferred size from its unconditional children, and there were none. So the root's height
resolved to zero, the enclosing `VerticalLayout` in `app.slint` sized the row at zero, and the real
content — which sets its own height and is not clipped — spilled out of a box with no room for it.

The fix is structural, not a magic number: one unconditional `VerticalLayout` holds both branches
and the root takes *its* preferred height, because `if` inside a layout does contribute to that
layout's preferred size. That is the same idiom `message_bubble.slint` has always used
(`height: msg-layout.preferred-height`). The rule worth keeping: **never ask a bare Rectangle how
tall it would like to be when everything inside it is behind an `if`.**

Bounding the text (above) is the second half. Even with the height reported correctly, a card that
can grow without limit runs off the screen and takes its buttons with it, which is why every text
on it is now a known number of lines and only one card is shown at a time.

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

It will block. **A card appears on screen within a second, over whatever screen is up, and the
shell comes to the front of whatever app was focused.** Close the card's question and the window
you were in comes back.

### The geometry to expect

The card is the top-right of the shell window, and two of its numbers are exact arithmetic rather
than measurements:

| | value | for a 1280×800 screen |
|---|---|---|
| card top edge | `48` — `status-bar-height` (32) + `sp-4` (16) | **y = 48** |
| card left edge | `W − 420` | x = 860 |
| card width | `404` | x = 860…1264 |
| Deny centre x | `W − 315` | **x = 965** |
| Allow centre x | `W − 121` | **x = 1159** |

(The button row is the card's last element: `404 − 2×12` padding = 380px split into two 186px
buttons with 8px between them.)

The vertical extent depends on how many lines the purpose wraps to, which is the one thing not
fixed. For the calendar case — a two-line purpose, three arguments, and the "cannot be undone"
warning — the card is about **298px tall**: 12 padding + 15 header + 8 + 15 self-declared + 8 + 19
action name + 8 + 32 purpose + 8 + 70 arguments box + 8 + 16 warning + 8 + 15 grade line + 8 + 36
buttons + 12 padding. So on a 1280×800 screen expect:

- card **y = 48 … ≈346**
- button row centre **y ≈ 318** (always `card bottom − 28`)
- Allow once at **≈(1159, 318)**, Deny at **≈(965, 318)**

Without the warning line (an action the app does not call unrecoverable) subtract 24px. The text
line heights above are estimates from the font sizes; **the numbers to actually check are the two
that are exact**: nothing of the card may be above `y = 48`, and the header
(`Hermes Agent 0.9.2` over `self-declared name · asking to use this machine`) must be fully
readable. That is precisely what was broken.

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
grim /tmp/card.png            # check the header is visible and read the button row's y
wlrctl pointer move -10000 -10000        # pin the pointer to the top-left corner
wlrctl pointer move $((W - 121)) 318     # x is exact; y from the table above
wlrctl pointer click left
```

See the geometry table above for where the buttons are. The x is exact; the y is a close estimate
because the card's height depends on how many lines the purpose wraps to, so read it off
`/tmp/card.png` once — it is stable for a given action.

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
  d. **Flood** — fire four `delete_event` calls with four different ids at once. Three requests
     are accepted and the fourth is refused with "already waiting"; **one** card is on screen with
     `2 more waiting — this one first` under it, and the next appears as each is answered.
  e. **Behind a window** — the regression that made all this necessary. Focus Calendar
     (`wlrctl toplevel focus title:Calendar`), then fire a `delete_event`. The shell must come to
     the front with the card fully visible. Answer it: Calendar must come back to the front. If
     `wlrctl toplevel list state:activated` on this build prints more than one line, the shell
     stays in front instead — that is the documented "not knowable" path, not a bug.

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
| `crates/yantrik-ui/src/approvals.rs` | 16 tests: single use, duplicate consume authorises nothing, key order does not invalidate, any value change does, a changed app or action does, denial prevents consumption, request expiry, grant expiry, flooding refused, identical question dedupes *and is not reported as fresh* (so a repeat cannot re-raise the shell), denial silences a repeat, unknown id is not an expiry, canonical JSON, the card shows what the grant binds, the card is a bounded number of lines, the decision leaves a record line |
| `crates/yantrik-ui/src/control_approvals.rs` | 3 tests: no published action can grant (reads the source of every `control*.rs`), the three asking actions are still published (so deleting the feature cannot make the first one pass), `args_json` binds identically whether it arrives as an object or a string |
| `deploy/yantrik-os/yos-mcp-selftest.py` | 32 checks against a fake `yos`: below-ceiling runs unasked, the five outcomes each produce their own message and only one of them runs anything, the grant binds the coerced value the app will actually receive, an argument swapped after approval spends no grant and runs nothing, the three sources of the requester's name in precedence order (a real `initialize` handshake is driven through `main()`), and the bridge's wait is shorter than the shell's request lifetime |

Neither of the two defects found on the machine is covered by a test, and honestly: they are both
about rendered geometry and compositor stacking, which nothing in this repo can assert without a
display. The release gate (`design/next-focus-2026-09.md` §2) says the same thing about window
visibility — "today's witnesses cannot prove a mapped window is on screen". Step 3 and case (e) of
the verification above are the substitute, and they are a person with a screenshot.

```
cargo test --offline --profile fast -p yantrik-ui --bin yantrik-ui approvals
python3 deploy/yantrik-os/yos-mcp-selftest.py     # needs Linux; the fake yos is an exec script
```

## Still open

- Hermes' MCP `tools/call` timeout (see Timeouts above) — not in this repo, must allow 250s.
- ~~Nothing authenticates the `requester` name on a card. Issue #43.~~ — answered below,
  21 September, in the sense that a *program* is now named. A name is still not authenticated and
  cannot be; read "What is still not verified".
- The card cannot show a diff or a preview of what the action will do — only its published purpose
  and its arguments. For `delete_event` that is enough; for something like a bulk file operation it
  would not be, and `control_files`' actions should probably grow a "what this will touch" line
  before they are routinely put in front of a person this way.

---

# 21 September 2026 — the card now says who, and separately who says so

Issue #43, which both this note and `design/mind-modes-2026-09-21.md` left open in the same
sentence: *nothing authenticates the requester name.* The card read

```
Hermes Agent 0.14.0
self-declared name · asking to use this machine
```

and the second line was the whole of the honesty. The name came off the MCP `initialize`
handshake, the bridge passed it to `request_approval(…, requester)` as an argument, and anything
that could open `app-shell.sock` could put `Your bank` there instead. The same string went into
`~/.local/share/yantrik/mind-audit.jsonl` for everything a mind did unasked. A person deciding
whether to allow a `sensitive` action was judging partly by a label nobody had checked.

The answer was free and was being thrown away.

## What is now verified, and how

**The kernel, at `connect`.** `SO_PEERCRED` on an accepted unix socket yields the peer's `(pid,
uid, gid)`, filled in by the kernel from the peer's own process. The caller does not write it and
cannot influence it. `yantrik-ipc-transport::server` reads it at `accept` — not when a handler
asks, because the peer of an MCP-borne request is `python3 /opt/yantrik/bin/yos`, which runs one
JSON-RPC call and exits; asked a second later there is nothing left to ask about.

**Carried to where the handler actually runs.** Handlers execute on the UI thread, reached by
posting a closure to the Slint event loop (`UI_ROUNDTRIP` in `yantrik-app-runtime/src/control.rs`),
so the socket thread is *not* where a handler could read a "current caller". The credentials
travel **with** the closure and are installed on the far side for exactly the duration of that one
dispatch, in a thread-local guarded by a `Drop` impl:

```
accept → PeerCred{pid,uid,gid}  (transport)
       → ServiceHandler::handle_from(method, params, peer)
       → on_ui_thread(who, job)
       → [UI thread] CallerScope::enter(who)  →  handler  →  scope dropped
```

A global would have been wrong and the mistake would not have shown up in testing: two
connections can be in flight at once, and a handler would sometimes read the pid belonging to
somebody else's request. `control::caller() -> Option<Caller{pid,uid,gid}>` is the whole public
surface, and **the handler signature is untouched** — the fourteen apps build `|args| { … }`
closures and not one of them changes. The runtime refuses nothing on this basis; policy is the
shell's.

`ServiceHandler` gained `handle_from` with a **default implementation** that discards the peer and
calls `handle`, so every other service and app compiles and behaves exactly as before.

**Resolved into a program, in `crates/yantrik-ui/src/caller_identity.rs`.** From the pid:
`/proc/<pid>/exe` (the real binary), `/proc/<pid>/cmdline` (NUL-separated), `/proc/<pid>/stat`
(parent pid and **start time**). The walk goes up at most 8 ancestors, remembers what it has seen
so a reused pid cannot send it round forever, and stops at `systemd`/`init`. Every read is allowed
to fail — the direct peer usually *has* exited by the time anyone looks, which is why the chain is
captured at handler time and kept on the request.

The interesting fact is **the first ancestor that is not our own plumbing**. `yos`, `yos-mcp` and
bare shells are skipped; `bash deploy.sh` is not a bare shell and is exactly what should be named.
For a Hermes request the chain is

```
python3 yos                             ← the peer, gone within milliseconds
python3 yos-mcp                         ← ours
…/hermes-agent/venv/bin/python -m hermes_cli.main gateway run   ← what a person recognises
systemd --user                          ← stop
```

**Matched against the attached minds** (`describe shell` → `minds[]`). The harness registry records
*no pid and no executable* — `yantrik_harness::host::Entry` is `{id, name, detail, builtin, active,
capabilities}` — so the match is by name against the ancestry, which is weaker than it sounds and
is written down as such in `mind_for`. Tokens shorter than four characters do not count (a mind
called "AI" would otherwise match half the process table), and our own bridge processes are
excluded from the search: a mind's name in the path of the program *we* wrote to talk to it proves
nothing. **If the registry ever grows a pid, `mind_for` is the one function that has to change**,
and the match becomes an identity rather than a coincidence of spelling.

## What the card says

The single self-declared line became two labelled pairs — value over label, twice — so a person
reads a claim and a fact rather than a sentence:

| | what it says | how |
|---|---|---|
| Hermes through `yos-mcp` | `Hermes Agent 0.14.0` / `says the caller` · `python -m hermes_cli.main gateway run (pid 696) · the attached mind` / `verified by this machine` | peer pid → /proc → skip `yos`, `yos-mcp` → name matches an attached mind |
| a bare `yos act` typed in the Terminal app | `an unnamed caller` / `says the caller` · `yantrik-terminal (pid 812)` / `verified by this machine` | peer is `yos`, parent is a bare `bash`, the app above it is the answer |
| a script run over ssh | whatever it sent / `says the caller` · `python3 nightly.py (pid 5500)` / `verified by this machine` | the script is above the bridge and below the `sh -c`; `sshd` is further up and not needed |
| nothing recognisable above the peer | … · `a program started from a terminal: python3 script.py (pid 4242)` | a bare shell was skipped and nothing above it was readable |
| `/proc` gave nothing | … · `could not be identified` | no pid at all, or every read failed |

Never blank and never a guess. `could not be identified` is a sentence the card prints on purpose;
an empty row would read as "nothing to report", which is the opposite, and would collapse the row
— which is precisely how this card lost its header off the top of the screen on 20 September.

## The grade is a claim too, and the shell now checks it

Found while the shared decision vectors were being built, and it is the same bug in a second
place. `request_approval(app, action, grade, …)` takes the **grade** as an argument, so it is
exactly as self-declared as the name — and the shell consulted `mind_mode::decide` and then acted
on the answer **only when the mode was `plan`**. A caller that skipped the bridge could therefore
get a card raised for an action graded above `tool_permission`: a question no answer could
satisfy, because `yantrik-app-runtime::control` refuses the action whatever the person clicks.
The rule in both notes is that nothing above the machine ceiling is ever put in front of a
person; until now only the bridge kept it, and the socket is reachable without the bridge.

Two fixes, in this order, because the second depends on the first.

**The grade comes from the app, not from the caller.** `settle_grade` asks the target app what it
publishes:

- the desktop itself → `yantrik_app_runtime::control::published_grade(action)`, straight out of
  the registry the UI thread already holds. **Not over the socket**: the shell asking the shell
  would be a call its own UI thread has to answer while it is blocked making it.
- any other app → `app.describe` over its socket, `GRADE_LOOKUP` = **500ms** (the handler's own
  budget is `UI_ROUNDTRIP` = 3s, and `SyncRpcClient`'s breaker makes a second attempt free), then
  `actions[] → permission`.
- an app this desktop does not have, an action it does not publish, or a surface that will not
  say → **refuse**. None of those is a reason to put a card in front of somebody, because there
  is nothing behind it for them to allow.

`surface_for` resolves three names, because an app has up to three: the launcher's route table
("Downloads" opens as `downloads`, describes as `download-manager`), `shell`/`yantrik` for the
desktop, and — new here — anything answering under its own name in `running_apps()`, so a surface
that is plainly up but not in the launcher is not treated as fictional.

If the declared grade is not the published one, the published one decides **and the card says so**:

```
│ Caller said `standard`; the app publishes `dangerous`.
```

Both directions are said. The understated one is why this is checked at all — it is how a
`dangerous` action would have been asked about as though it were routine, or, under a `standard`
ceiling, asked about at all instead of refused outright.

**Every outcome of the table is acted on**, not the plan-mode third of it:

| `decide` says | the shell does |
|---|---|
| `refuse_grade` / `refuse_ceiling` / `refuse_mode` | refuses, **relaying `decide`'s own sentence verbatim** — so a mind that came through the bridge and one that came straight to the socket hear one story |
| `run` / `run_logged` | no card. Answers `{"status": "not_needed", app, action, grade, mode, next}` — a person shown a question the machine was going to say yes to anyway learns that cards are noise |
| `ask` | raises the card |

`not_needed` has one real caller and it is worth naming: the bridge's own
`YOS_MCP_MAX_PERMISSION` is a cap a harness puts on *itself*, and it can turn a desktop `run` into
a bridge `ask`. Before this, that reached `request_approval` and got a card. Now the shell answers
`not_needed` and `ask_the_person` reports it as *"this bridge would have asked, the desktop's mode
runs it without asking anybody, so nobody was disturbed"* and lets the action go ahead — which is
the honest reading: a self-imposed cap cannot make the desktop more cautious than its owner set it.

**When the claimed name and the verified program disagree**, a red line in the same style as
"cannot be undone":

```
│ “Hermes Agent” is attached here — this is not it.
```

Deliberately narrow. It fires only when the claimed name names a mind that is *genuinely attached
to this desktop* and the verified ancestry belongs to something else — the case a person cannot
possibly catch by reading, because the name will be exactly right. It stays quiet for a name no
mind here uses (`Your bank` is not a claim this can contradict; the card already prints the
verified program beside it) and quiet when `/proc` gave nothing, because absence of evidence is
not disagreement. A warning that fired on every ordinary unnamed caller would train people to
ignore the one that matters, which is the same approval-fatigue failure the rest of this design is
built around.

Both this and the grade correction go in one list — `Verified::discrepancies`, "what does not add
up about this request" — rendered as a `for` over one-line elided rows rather than as two
hand-written conditional blocks. Concatenating them into a single elided row would have shown the
first and silently dropped the second, and the second is the one that changes what the machine
does; a third of these later costs no markup and no new height arithmetic.

`describe shell` gained `verified` **beside** `requester`, in `pending_approvals[]` and in
`mind_audit_recent[]` — and in every line of `mind-audit.jsonl`:

```json
{ "id": "appr-1",
  "requester": "Hermes Agent 0.14.0",
  "verified": { "line": "python -m hermes_cli.main gateway run (pid 696) · the attached mind",
                "exe": "/home/pranab/hermes-agent/venv/bin/python",
                "pid": 696,
                "attached_mind": "Hermes Agent",
                "discrepancies": [] },
  "app": "calendar", "action": "delete_event", "grade": "sensitive", "age_secs": 4 }
```

Two keys, not one. A log that kept only the claim is the same gap in a different file; a log that
kept only the fact would lose what the caller *said*, which is the thing that turns out to be
interesting when they disagree.

### The geometry, updated

Two `fs-micro` rows were added (the verified value and its label, in one `VerticalLayout` with
`spacing: 0` so the gap belongs between the pairs rather than inside one), plus one conditional
`fs-caption` row for the mismatch warning. Everything is one line, `no-wrap`, elided, and the text
is bounded in Rust (`LINE_CHARS` = 66, `WARNING_CHARS` = 58 in `caller_identity.rs`), so the card's
height stays arithmetic. All of it lives inside the one unconditional `VerticalLayout` the
20 September fix introduced — **conditional content never becomes a bare Rectangle's only child.**

| | before | now |
|---|---|---|
| identity block | 15 + 8 + 15 = 38 | 15 + 8 + 15 + 8 + 30 = **76** |
| discrepancy rows | — | + 24 **each**, only when one fires (16 + 8); at most two today |
| card height, `calendar.delete_event` | ≈298 | **≈336** (+24 per discrepancy: ≈360, ≈384) |
| card **y** on 1280×800 | 48 … ≈346 | 48 … **≈384** (≈408 / ≈432) |
| button row centre **y** | ≈318 | **≈356** (≈380 / ≈404) |
| Allow once | ≈(1159, 318) | **≈(1159, 356)** |
| Deny | ≈(965, 318) | **≈(965, 356)** |

The x column is unchanged and still exact: card left `W − 420`, width `404`, Deny centre `W − 315`,
Allow centre `W − 121`. The button row is still `card bottom − 28`. The two numbers to actually
check on a screenshot are still the two that are exact: nothing of the card may be above `y = 48`,
and the four identity lines must all be readable.

## What the socket's permissions allow today — a finding, not a change

`yantrik_ipc_transport::server::socket_dir()` hardens the directory to **0700** and re-checks it
on every call. The socket *files* inside it are created with the default umask and come out
`srwxr-xr-x` (0755) — mode alone would let any local user connect. The directory is what actually
stops them: no traverse, no connect. On the VM:

```
drwx------ 2 yantrik yantrik  /run/user/1000/yantrik      (or /tmp/yantrik-1000)
srwxr-xr-x 1 yantrik yantrik  .../app-shell.sock
```

So today a request from a different uid should be **unreachable for anyone but root**, which makes
it exactly the thing worth noticing if it ever happens: `who_is_asking` compares the peer's uid
with the shell's and logs `a request arrived on the control socket from another user` at WARN.
**Nothing was changed about the permissions.** If they are ever to be tightened, the socket file's
own mode (`0700` after bind, or a `umask(0o077)` around it) is the belt to the directory's braces,
and that is a deliberate decision to take separately.

## What is still NOT verified

Worth being blunt about, because the card is now more persuasive than it was and that is only an
improvement if the limits are written down.

- **This identifies a PROGRAM, not an intent and not a person.** "The request came from
  `hermes_cli`" says nothing about whether what `hermes_cli` is asking for is a good idea. The
  card still shows the real `app.action` and every argument verbatim, and that is still what a
  person actually judges.
- **A malicious process running as this user can simply *be* the ancestor.** It can fork, exec
  anything, name itself whatever it likes in `argv[0]`, and sit in the chain where Hermes would
  sit. There is no defence against this at this layer and there cannot be: same uid, same
  everything. The uid boundary is the real boundary, and it is the directory mode above.
- **Exe paths can be replaced.** `/proc/<pid>/exe` is the file that was executed; nothing here
  hashes it or checks a signature, and `…/venv/bin/python` is whatever that file is today.
- **Name-matching a mind is spelling, not identity.** See `mind_for` above: the harness registry
  holds no pid, so "this ancestry belongs to Hermes" means "something in this ancestry has
  `hermes` in its path". A program deliberately installed under a matching path would match.
- **The pid-reuse window is narrowed, not closed.** Each `/proc` read pair is bracketed: `stat` is
  read before and after `exe` and `cmdline`, and if field 22 (`starttime`) moved, the facts are
  thrown away rather than half-attributed. Within a walk no pid is visited twice. What remains is
  the gap between the kernel stamping the pid at `connect` and the shell's walk — microseconds to
  a few milliseconds, inside one `app.act` — during which the peer could exit and its pid be
  reused. That produces a *wrong* program name, not a forged one, and it is why the chain is
  captured at handler time rather than when the card is drawn.
- **Nothing is refused on this basis.** Not in the runtime (policy belongs to the shell) and not
  in the shell (the machine ceiling and the mind mode are the policies, and they are about grades,
  not about callers). The verified line is information for the person, not a gate.

## Verifying it on the machine

As the desktop user, `XDG_RUNTIME_DIR=/run/user/1000 WAYLAND_DISPLAY=wayland-0`.

**0. The permissions, first, because everything below assumes them.**

```sh
ls -ld /run/user/1000/yantrik ; ls -l /run/user/1000/yantrik/app-shell.sock
#   drwx------  …  — the directory is what keeps other users out
#   srwxr-xr-x  …  — the socket's own mode would not
```

**1. Hermes, through the bridge.** Drive the same `delete_event` as step 1 of the original
verification above. The card must show four identity lines, in this order:

```
● Hermes Agent 0.14.0                                    114s left
  says the caller · nothing on this machine checked that name
  python -m hermes_cli.main gateway run (pid 696) · the attached mind
  verified by this machine · the kernel said so, not the caller
```

Check the pid against the process tree, from a second shell:

```sh
yos describe shell | python3 -c 'import json,sys; \
  print(json.load(sys.stdin)["state"]["pending_approvals"][0]["verified"])'
#   {'line': 'python -m hermes_cli.main gateway run (pid 696) · the attached mind',
#    'exe': '/home/pranab/hermes-agent/venv/bin/python', 'pid': 696,
#    'attached_mind': 'Hermes Agent'}
ps -o pid,ppid,args -p 696
```

**2. A bare `yos` typed in the Terminal app.** Open Terminal, then in it:

```sh
yos act shell request_approval app=calendar action=delete_event grade=sensitive \
    args_json='{"id":"evt-3"}' purpose='Delete an event. It is not recoverable.' \
    requester='Hermes Agent'
```

The claimed line says `Hermes Agent`; the verified line must name **the terminal**, not Hermes
(`yantrik-terminal (pid …)`), and the red mismatch line must appear. That is the whole feature in
one command: the same self-declared string that used to be the only thing on the card is now
contradicted by the machine, on the card, while the person is looking at it.

Step 2 is also the check for the grade gate, because `request_approval` will now refuse outright
if `calendar` is closed (no grade to read) and will correct the grade if you understate it:

```sh
yos act shell request_approval app=calendar action=delete_event grade=safe \
    args_json='{"id":"evt-3"}' purpose='x'
#   card appears, headed `Graded sensitive`, with
#   │ Caller said `safe`; the app publishes `sensitive`.

yos act shell request_approval app=nosuchapp action=x grade=safe
#   refused: there is no app called `nosuchapp` on this desktop …

yos act shell request_approval app=files action=delete grade=safe args_json='{"name":"x"}'
#   with tool_permission: standard — refused, naming `tool_permission`, and NO card appears.
#   This is the gap that was open: before, a card went up that no answer could satisfy.

yos act shell request_approval app=notes action=append grade=standard args_json='{"text":"hi"}'
#   with the desktop in `auto` — {"status": "not_needed", …}, and no card.
```

**3. Nothing recognisable.** Over ssh, with no terminal app in the chain:

```sh
ssh yantrik@vm 'XDG_RUNTIME_DIR=/run/user/1000 yos act shell request_approval \
    app=notes action=append grade=standard args_json="{\"text\":\"hi\"}"'
```

The verified line names the `sshd` session or the `yos` peer itself — never blank, never
`systemd`.

**4. A caller with no credentials.** There is no supported way to produce one on Linux (every
unix-socket peer has a pid), which is the point; the path is exercised by the tests, and by the
Windows dev build where the transport is TCP and the card says `could not be identified`.

**5. The audit.** With the desktop in `auto` mode, let a mind run something unasked, then:

```sh
tail -1 ~/.local/share/yantrik/mind-audit.jsonl | python3 -m json.tool
#   "requester": "Hermes Agent 0.14.0",
#   "verified": {"line": "…", "exe": "…", "pid": 696, "attached_mind": "Hermes Agent"},
```

Both keys, always. If `verified.pid` is `0` on a line where `requester` is a real name, the caller
reached `record_unasked_action` without credentials — which on this machine means a bug, not a
caller.

## Tests

| where | what |
|---|---|
| `crates/yantrik-app-runtime/src/control.rs` | 3: a caller is current only inside its own dispatch (and nested scopes restore, and nothing leaks afterwards); a panicking handler leaves no caller behind; **the caller reaches the handler across the UI hop** — a real `UnixStream` to a real served surface, asserting the handler saw `pid == std::process::id()` and the uid that owns the socket. The hop is a stand-in worker thread (`test_ui_thread`, `#[cfg(test)]`), because the real one needs a Slint event loop, which needs a window, which needs a display |
| `crates/yantrik-ui/src/caller_identity.rs` | 17: `stat` parsed from the **last** `)` (a comm containing `(weird) name` with spaces), a truncated `stat` is `None`, `PPid` out of `status`, NUL-separated `cmdline` with its trailing NUL and its path-shortening and its bound; ancestor selection (skips `yos`/`yos-mcp`, skips bare shells but **not** `bash deploy.sh`, stops at `systemd`, names the terminal, names a script over ssh, names the peer when nothing is above it); a short mind name cannot match half the process table; our own bridge cannot stand in for a mind; mismatch fires on a borrowed name and is silent for an unknown one, for an honest one, and when `/proc` gave nothing; the warning is one bounded line; the real walk is bounded and cycle-free |
| `crates/yantrik-app-runtime/src/control.rs` | 1 more: an app can read its own published grade without a round trip, and an action it does not publish answers `None` rather than a default |
| `crates/yantrik-ui/src/approvals.rs` | 2 more: the claim and the fact stay apart (and `to_json` carries no claim); an unidentifiable caller carries an empty fact rather than a flattering one |
| `crates/yantrik-ui/src/control_approvals.rs` | 3 more: an unidentified caller never renders a blank row — it renders `could not be identified`, one line, and two discrepancies stay two rows; an understated grade is corrected and said in one bounded line, and agreement is silent; **`request_approval` acts on every outcome of the decision table** — each of the six the shared vectors name (`run`, `run_logged`, `ask`, `refuse_grade`, `refuse_ceiling`, `refuse_mode`) at least once, refusals relayed verbatim, plus the case the whole lookup is for: `files.delete` declared `standard` would have *run* unasked and, on its published `dangerous`, is refused outright under a `standard` ceiling |

```
cargo test --offline --profile fast -p yantrik-app-runtime
cargo test --offline --profile fast -p yantrik-ui --bin yantrik-ui
cargo check --offline --profile fast --workspace     # all fourteen apps, unchanged
python3 tests/app-lints/run.py                        # 0 new
```
