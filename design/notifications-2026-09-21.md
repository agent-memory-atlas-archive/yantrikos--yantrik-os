# One place a notification can land

Written 21 September 2026, after "we need OS-level notifications for any notifications".

## What was wrong

Four notification systems. Not three — the survey found one more than anybody had counted.

**1. `mako`.** The stock freedesktop daemon, started from `config/labwc/autostart` and from four
build scripts. It held `org.freedesktop.Notifications`, so `notify-send`, Chromium and every
ordinary Linux program on the machine reached it — and it drew their popups in its own style,
kept them in its own head, and told nothing else. The shell's notification centre was empty all
day on a machine that was showing notifications all day.

**2. The shell's own D-Bus daemon.** `crates/yantrik-os/src/dbus_notif.rs`, 404 lines, spawned
from `SystemObserver::start` on a thread called `yos-notifications`, implementing the whole
`org.freedesktop.Notifications` interface and **claiming the same bus name**. Only one process
can own a well-known name, so which of the two a `notify-send` reached came down to start order.
The audit of 17 September caught mako winning by a few hundred milliseconds. Its
`emit_close`/`emit_action` helpers — the only way a sender would ever hear that its button was
pressed — had no callers at all, and used `dbus-send --dest=org.freedesktop.Notifications`, which
unicasts the signal *to ourselves*: even wired up, no sender would have received one.

**3. The shell's private store and toasts.** `crates/yantrik-ui/src/notifications.rs` kept a
`Vec` and wrote `~/.yantrik/notifications.json`, fed by `push_toast` from screenshots, focus mode
and the companion bridge. In-process, invisible to every service, every app and every mind. Each
toast got an id made from the wall clock in milliseconds, so two raised in the same millisecond
shared an id and dismissing one dismissed both — and each started its own `slint::Timer` and then
`std::mem::forget`-ed it, leaking one timer per notification for the life of the session.

**4. `services/notifications-service`.** 280 lines, autostarted by the shell, `Mutex<Vec<…>>` —
gone on restart. **Nothing in the entire tree ever called `notifications.add`.** Its `add`
required both `title` *and* `body`, so the one-line send every caller actually wants was a
`-32602`; nobody found out, because nobody called it. Its `dismiss` answered success whether or
not anything was removed.

And apps could not notify at all. A finished download, an event about to start, an available
update, a mind waiting for an answer — none of it left its own window.

## The single path now

```
  notify-send / Chromium / any Linux program ──org.freedesktop.Notifications──┐
  yos notify "..."                           ──notifications.add─────────────┤
  download-manager, calendar-service         ──notify::send / add────────────┤──► ONE store
  the shell (updates, the mind, screenshots) ──notify::send─────────────────┘   (one JSON file)
                                                                                      │
  toasts, the unread badge, screen 9         ◄──notifications.since(revision)──────────┘
```

`services/notifications-service` owns notifications. It owns the store **and** the bus name,
because the two cannot be in different processes without the race above. The shell has no daemon
and no store of its own any more; it polls, draws, and sends like everything else.

## The store

`~/.local/share/yantrik/notifications.json` (`$XDG_DATA_HOME` first), written to a temp file and
renamed over the real one so a process killed mid-write leaves the previous file intact.

Each notification: `id, app, title, body, urgency, created_at, read, dismissed, actions[],
source, replaces_id?, revision`.

`id` is a decimal counter as a string — "1", "2", … — not a uuid, because the freedesktop spec's
`Notify` must answer with a `u32` the sender can later pass to `CloseNotification` or as
`replaces_id`. With uuids the service would need a second id space and a map between them, and
any gap between the two is a notification the program that sent it cannot close.

**Bounds** (`crates/yantrik-ipc-contracts/src/notifications.rs`), applied on the way in, because
anything on this machine can post here including programs we did not write:

| | |
|---|---|
| title | 200 characters |
| body | 2 000 characters |
| app | 64 characters |
| actions | 4, each id 64 and label 48 characters |
| kept | newest 500 |
| dismissed | pruned a week after they were made |

Clamping counts **characters, not bytes** — cutting at a byte offset would split a UTF-8 sequence
and write a file the next start cannot parse. Nothing is refused for being too long: the
freedesktop spec has no error reply for it, and refusing would make this the desktop where
`notify-send` mysteriously fails.

**`revision`** is a counter bumped on every mutation, and every notification carries the revision
at which it last changed. `notifications.since {revision}` answers `{revision, changed:[…]}` so
the shell's one-second poll is a few hundred bytes and almost always empty. Dismissals and reads
are changes too — a client told only about additions would keep a toast up for something the
person had just closed in the notification centre. The counters are persisted, so a restart never
hands out an id the shell already has and `since` never goes backwards.

**Do Not Disturb is not in the store.** DND decides whether a toast pops, which is a question
about the screen, and the screen is the shell's. Everything is stored and counted either way.
There is a test that reads the store's own source and fails if it mentions DND at all, because
"under DND, drop it" is the obvious wrong turn and it destroys messages permanently.

## The freedesktop door

`Notify`, `CloseNotification`, `GetCapabilities`, `GetServerInformation`; `NotificationClosed`
and `ActionInvoked` broadcast on the session bus — broadcast, with no destination, unlike the old
`--dest=org.freedesktop.Notifications` that delivered every signal back to the sender of it.
`zbus::blocking` on a thread of its own, started before the tokio runtime the service's RPC
server builds, because blocking inside a tokio worker panics.

Capabilities are `body`, `actions`, `persistence`, `icon-static`. **Not `body-markup`**: the
toast and the notification centre draw plain text, and a daemon that claims markup and then shows
the tags is worse than one that never claimed it — senders format for what they are told.

Everything that can be a pure function is one, below the server, and tested without a bus: the
urgency hint byte, the flat `[id, label, id, label]` action list (a dangling id is dropped rather
than given an empty label), `replaces_id`, and the sender's name falling back to the
`desktop-entry` hint and then to "unknown".

If the name is already owned — mako still running on a machine that has not taken the update —
the service says so plainly, once, resolves *who* owns it through `GetNameOwner` and
`/proc/<pid>/comm`, keeps serving its socket, and reports
`freedesktop: "unavailable — org.freedesktop.Notifications is owned by mako (pid 754, :1.31)"` in
`describe`. It does not retry in a loop: two daemons taking turns at a well-known name is worse
than one of them losing, and a log line every second buries the one that matters.

**mako is no longer started or installed** — `config/labwc/autostart`, `build-debian-iso.sh`
(autostart heredoc, config heredoc, apt list), `cloud-init/user-data.yaml` (its removal from the
apt list is *required*, or the ISO's own cloud-init/ISO parity check fails the build), plus the
legacy Alpine scripts `install.sh`, `deploy-stack.sh`, `build-vbox-image.sh` and `build-iso.sh`.
`libnotify-bin` stays: that is only the `notify-send` client, and it now lands in this store.

## What feeds it

| Sender | What it says | Where |
|---|---|---|
| any Linux program | whatever it sends | `org.freedesktop.Notifications` |
| `yos notify <title> [body] [--urgency] [--app]` | a person or a script | `deploy/yantrik-os/yos` |
| a mind | `act notifications notify title=…` (grade `standard`) | the service's control surface |
| download-manager | finished, with the folder and an "Open folder" button; failed, with the reason | `engine.rs` `transfer` / `fail` |
| calendar-service | an event starting in ten minutes | `services/calendar-service/src/reminders.rs` |
| the shell | an update is available, once per version | `wire/notifications.rs` |
| the shell | a mind is waiting for an answer (`critical`) | `approval_waiting`, one call for `control_approvals` |
| the shell | a bypass ran out on its own, and what it did | `bypass_ended`, from the mode tick |
| the shell | the mind finished while the Lens was closed | `bridge.rs` → `companion_said` |
| the shell | screenshots (saved / failed), focus session complete | `wire/screenshot.rs`, `focus.rs` |

Apps and services send with one line:

```rust
notify::send(
    Notification::new("Downloads", "debian-13.iso finished")
        .body("Saved to ~/Downloads")
        .action_with("open_folder", "Open folder", json!({ "id": 4 })),
);
```

It never blocks the caller — every send is handed to a short-lived thread, and there is a test
that calls it with no service running and asserts the call returned in milliseconds. It starts
the service on demand the way calendar and email do, is bounded at eight sends in flight (a
caller in a loop loses notifications rather than spawning threads), and says once, not every
time, when the service cannot be reached.

**Action buttons work in both directions.** A freedesktop sender hears `ActionInvoked` — that is
the whole mechanism the spec gives. Our own apps have no such signal and may not even still be
running, so the shell *presses the button on the sender's behalf*: it calls the named action on
that app's own control surface, with the arguments the notification carried, starting the app
first if the transfer outlived the window. Download Manager's "Open folder" is `open_folder` with
`{"id": 4}` — the same call the button inside its window makes. Without this, a button on one of
our notifications would be a control that does nothing.

## Where toasts sit

**Bottom right, stacking upward, newest on top, above the taskbar.** Not top right.

The approval card — the one a mind puts up when it needs a yes — is an overlay at
`x = parent.width − 420px`, `y = status-bar-height + sp-4`, 404px wide, with a height that grows
with the request. Toasts were at the top right too, 360px wide at `x = parent.width − 380px`, so
the two overlapped by 360 of 404 pixels: whichever was declared later in `app.slint` won, and a
toast could cover the buttons on a question the machine was waiting to have answered.

Stacking below the card was considered and rejected: the card's height is not knowable from
outside, because it is built inside a conditional block and there is no element to measure, and a
fixed reserve would be a guess that goes wrong the first time a request has a long argument list.
Opposite edges cost nothing and cannot go wrong. Questions at the top, where they are waited for;
news at the bottom, where it can be ignored.

Three visible, "+N more" below them, six seconds for low and normal, critical stays until it is
dismissed. Under DND or focus mode nothing pops but `critical` — everything is still stored and
counted. No toast on the lock, login, boot or onboarding screens: the same list the approval card
uses, and for the same reason.

One `Timer`, not one per toast: a sweeper that runs while anything is up and stops itself when
the queue empties, dropped on a later turn of the event loop rather than from inside its own
callback.

**The Slint trap, written down so it is not hit a third time.** A non-layout element takes its
preferred size from its *unconditional* children. A `Rectangle` whose children are all behind
`if` reports a preferred height of zero and its content spills upward off the screen. Every card
here — toast and notification-centre row — wraps its variable content in one unconditional
`VerticalLayout` and takes that layout's `preferred-height`.

## A mind that is not answering must not write in the conversation

Observed live while this was being built, and fixed here because the fix is a notification.

The desktop's answering mind was Hermes. A person asked it "Is this machine online, and what is
its IP address?" and, while Hermes was working, the **built-in companion's** serendipity instinct
pushed `Something came to mind — you once said: "User is interested in: technology"` into the
same transcript, as an `assistant` message. It reads as the answer to the question. A task grader
took it as the answer and failed the task; a person would have been just as confused.

Two independent faults:

* **The transcript belongs to whoever is answering.** When that is not the built-in companion,
  the companion's unprompted output is not part of the conversation at all — it is a notification
  (`app: "Yantrik Companion"`, low urgency) and nothing else. The answering mind comes from the
  harness host, the same source `describe`'s `minds[].answering` and `use_harness` use.
* **The Synthesis Gate thought nobody was talking.** `conversation_active` came from a timestamp
  bumped only inside the companion worker's own `SendMessage` arm, which only messages bound for
  the built-in companion ever reach — so a live conversation with a harness mind looked like an
  idle user. Every typed message passes through `wire::chat::dispatch` on its way to whichever
  mind answers, and that is where the clock is now. And nothing unprompted is delivered at all
  while a mind is mid-answer, with a three-minute ceiling so a harness whose channel is dropped
  without a `__DONE__` cannot silence the machine for the rest of the session.

A held message is held *before* it is recorded as sent, so the anti-repetition tracker does not
suppress it the next time it is genuinely due. A finished background task is the one exception to
holding: it is a result somebody is waiting on, so it becomes a notification rather than silence.

Not fixed, not this lane, but worth saying: the quoted "memory" is an auto-extracted profile stub.
`User is interested in: technology` is not worth saying to anybody, in a transcript or a toast.
`crates/yantrik-companion-instincts/src/serendipity.rs:59` is choosing badly. Routing it correctly
makes it quiet; it does not make it worth reading.

## Deliberately left out

* **Sounds.** No audio at all. It needs a sound theme, a volume rule and a mute that is separate
  from DND, and none of those have anywhere to live yet.
* **Per-app mute rules.** There is no per-app settings surface to put them in, and a mute that
  cannot be found again is a message silently lost.
* **History search.** The notification centre lists; it does not search. 500 entries grouped by
  app fit on a screen a person can scroll.
* **Grouping across restarts.** Grouping is by app name, recomputed on each draw, and the group
  order is "which app spoke most recently". Nothing about a group is stored.
* **`expire_timeout`.** Accepted and logged, never stored. It is a sender's wish about how long
  its popup stays, and the popup is the shell's — one rule for every sender, so no program can
  pin a message to a corner of somebody's screen. mako made the same call with its
  `default-timeout`. There is a test asserting the mapping ignores it.
* **Icons.** `app_icon` and the image hints are not read. The toast draws the first letter of the
  sender's name. A real icon path means a theme lookup and a fallback chain, and the letter is
  honest about what it is.
* **Migrating `~/.yantrik/notifications.json`.** The shell's old private file has a different
  shape and is history nobody has read. It is left where it is rather than translated.
* **A push channel.** `since(revision)` is a poll. It recovers by itself when the service is
  restarted under a running shell, which a long-lived subscription would not.
* **Notifications on the lock screen.** Stored and counted, never drawn: what is on a locked
  screen is readable by whoever is standing in front of it.
* **The unread badge on the status bar.** The property is live and correct
  (`notification-unread-count`); the status-bar component belongs to another change in flight, so
  the binding is one line for its owner to add. See the report.

## Verifying it on the VM

```bash
# 1. Nothing else holds the name. Expect exactly one owner, and it to be ours.
busctl --user status org.freedesktop.Notifications | head -5
pgrep -af 'mako|notifications-service'

# 2. The service's own account of itself: the store path, the counts, and the bus.
yos describe notifications

# 3. An ordinary Linux program.
notify-send --app-name=Chromium --urgency=critical "Download finished" "debian-13.iso"
yos describe notifications | head -20          # source: freedesktop, app: Chromium
#   → a toast, bottom right, red accent, stays until dismissed

# 4. Ours.
yos notify "Build finished" "cargo test: 34 passed"
yos notify "Disk full" "/ is at 98%" --urgency critical

# 5. A mind keeping a promise.
yos act notifications notify title="I finished the thing" body="as promised" urgency=low

# 6. The desktop agrees with the store.
yos describe shell | grep -A 8 notifications   # unread, showing, latest[3]

# 7. It survives. This is the check the old service could not have passed.
pkill -f /opt/yantrik/bin/notifications-service
rm -f "$XDG_RUNTIME_DIR/yantrik/notifications.sock"
yos act shell start_service name=notifications
yos describe notifications                      # the same ids, the same titles

# 8. Do Not Disturb holds the popup and keeps the count.
yos act shell set_do_not_disturb on
notify-send --urgency=low "Quiet" "no toast for this"     # nothing pops
yos describe shell | grep -A 3 notifications              # unread went up anyway
yos act shell set_do_not_disturb off

# 9. An action button that really does something.
#    Download something, wait for it to finish, press "Open folder" on the toast.
yos act shell open_app name=downloads
yos act download-manager add url=https://deb.debian.org/debian/dists/stable/Release

# 10. Screen 9 lists the same store, and says so when the service is not running.
yos act shell show_screen screen=notifications
pkill -f /opt/yantrik/bin/notifications-service    # leave it down
#   → "The notifications service is not running." — not an empty list

# 11. The suite.
python3 tests/conformance/run.py --app notifications --verbose
```

---

## Later the same day, 21 September 2026

One new sender, and one thing the routing could not do that nobody had noticed because nothing
had asked it to.

### The shell can be the app a button belongs to

"Action buttons work in both directions" above says the shell presses the button on the sender's
behalf, by calling the named action on that app's own control surface. That worked for every
sender it had been used by — Download Manager's "Open folder" is `open_folder` on
`app-download-manager` — and it could not have worked for the **shell itself**.

`surface_for` resolves a notification's `app` through `wire::dock::openable()`, the table of
things this desktop can *open*. Nothing opens the desktop, so there is no `yantrik` row in it,
so `surface_for("Yantrik")` answered `None` — and `forward_to_app` logged "a notification button
named an app this desktop does not open" and did nothing. Every notification the shell has sent
so far has been button-less, so this had never come up.

The shell does have a control surface: `app-shell`, the one `yos act shell …` reaches. One line
in `surface_for` now says so. A button on one of the shell's own notifications is a real call
like everybody else's.

### A bypass that ran out on its own

`design/mind-modes-2026-09-21.md` listed "no notification when a bypass lapses" as open, and
pointed at this lane. It is `wire::notifications::bypass_ended`, called from the one-second mode
tick in `control_approvals::wire` that was already folding the expired bypass back.

```
Bypass ended
The mind is back in Ask mode. It asks you before anything that could matter.
It did 3 things without asking while bypass was on.
                                                            [ See what it did ]
```

**`normal`, not `critical`.** `approval_waiting` is `critical` because a question has a deadline
and the person loses something by not seeing it. This is the opposite: the machine has just
become *stricter*, on its own, and nothing is waiting. A notification that survived Do Not
Disturb and held the screen until dismissed, to say that a machine had stopped doing something,
would be exactly the kind of thing that teaches people to dismiss notifications without reading
them.

**The button is dropped when the count is zero.** "See what it did" under "Nothing ran without
asking while bypass was on" is a control that contradicts the sentence above it. The three
lines that decide this live beside the sentence, in `mind_mode::bypass_ended_body`, which is also
where the 0 / 1 / many wording is unit-tested — a test for a sentence should not need a
notification service running.

The body's middle sentence is `Mode::meaning()`, the same string the mode menu draws, so the
notification and the chip cannot come to describe `auto` differently.

### Verifying it

```bash
# The shell has to be started with the test hook, or this takes fifteen minutes.
# It can only ever SHORTEN a bypass; see design/mind-modes-2026-09-21.md.
YANTRIK_BYPASS_SECONDS=20 /opt/yantrik/bin/yantrik-ui

# Chip → Bypass → 15 minutes, then drive a sensitive action through the bridge and wait.
yos describe notifications | head -20     # app: Yantrik, title: "Bypass ended", urgency: normal
#   → a toast, bottom right, with one button

# The button is the same call the mode menu's own row makes.
yos act shell show_mind_audit
#   { "showing": "the record of unasked actions", "entries": 1, … }
#   → the mode menu opens on its audit list

# And exactly one, however long you leave it: the lapse is taken, not polled.
yos describe notifications | grep -c "Bypass ended"     # 1
```
