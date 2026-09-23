# Problem reports — how a crash on someone else's machine reaches us, without the OS phoning home

Pranab, 22 Sep 2026, late: *"add report issue anonymously with exceptions and stuff to make us
better."* This is the design, and the constraint that shapes it.

## The constraint

`README.md`, Privacy and security, first bullet, written today and true:

> **The OS itself sends nothing anywhere.** No telemetry, no phone-home, no analytics, no call
> it makes on its own behalf. […] The two places it does reach out are ones you asked for.

A crash reporter that posts on its own would make that sentence false the day it shipped. So
every byte that leaves the machine leaves because a person pressed Send on a screen that showed
them the bytes. Not a setting that defaults on, not a "we've sent a report" toast after the fact.
The report is the person's act; the OS's job is to make that act easy, complete and safe.

Anonymous is a separate property and it is held by construction: the record carries no name, no
hostname, no IP, no account, and the paths in it have the user's home and username removed
before the file is even written to disk.

## Three parts

### 1. Records — written locally, always, by the thing that broke

`yantrik-app-runtime::problems`. Every app binary already runs `init_tracing()` first; it now
also installs a panic hook that writes a **problem record** before the default hook prints the
panic. The shell's launcher reaper does the same when a child exits non-zero (a crash that was not
a Rust panic: a segfault in a native library, an abort, a kill).

`~/.local/share/yantrik/problems/<unix-secs>-<program>.json`:

| field | what | from |
|---|---|---|
| `kind` | `panic` · `crash` · `failure` | who wrote it |
| `program` | `yantrik-studio` | the binary |
| `version`, `git` | what BUILD says | `yantrik_version` |
| `message` | the panic payload or exit status | scrubbed |
| `location` | `crates/yantrik-app-runtime/src/control.rs:533` | scrubbed |
| `backtrace` | if `RUST_BACKTRACE` produced one | scrubbed |
| `log_tail` | the last 40 lines this process logged | scrubbed |
| `machine` | cores, RAM (GB), GPU yes/no, virtualised yes/no, kernel | `/proc`, `/sys` |
| `when` | unix seconds | |

**Scrubbing** is one function with tests: `$HOME` and `/home/<user>` become `~`; the username
as a token becomes `<user>`; anything shaped like a credential — `sk-…`, `ghp_…`, `Bearer …`,
`token=…`, `key=…` — becomes `<redacted>`. The record is written already scrubbed, so a person
who opens the file sees exactly what a report would contain, and nothing in the OS ever holds an
unscrubbed copy.

`failure` is for the things that are not crashes and matter as much: a `describe` that timed out,
a `verify` that failed, an action refused by its own app. An app records one with
`problems::record_failure(what, detail)`. Nothing is recorded for ordinary refusals — a wrong
argument is the caller's problem, not ours.

Records are capped at 50; the oldest go. They are plain files a person can read, copy, or delete.

### 2. The screen — "Report a problem"

A notification when a record lands: *"Studio crashed just now. Report it?"* with **Report** and
**Not now**. Report opens the screen; Not now leaves the record where it is. The screen is also
reachable from About and from Settings, for the case where nothing crashed and something is
wrong — then the record is written by the screen itself, with the person's description as
`message`.

The screen shows:

- what happened, in a sentence;
- **the exact JSON that will be sent**, in a scrollable box — the record, not a summary of it;
- a free-text field: what were you doing;
- one line saying where it goes and that it carries no name, hostname or IP;
- **Send** and **Delete this record**.

Sending shows the outcome: the issue number it landed on, or *"sent; it joined 12 earlier
reports of the same crash"*, or the error. Nothing is retried in the background. A report that
fails to send stays a local record.

The same is an action on the shell's surface, for minds: `report_problem(record, note)`, graded
**`sensitive`** — data leaves the machine — so a mind cannot send one without a card, and the
card shows the record.

### 3. Transport — an intake service we run, holding the only secret

`https://report.yantrikos.com/v1/report`, on the project VPS beside `iso.` and `releases.`.

Why a service and not the alternatives:

- **A GitHub token in the image** is extractable by anyone who mounts the ISO, and then it is
  everyone's token. Same for a Discord webhook. Neither survives a public image.
- **A prefilled GitHub new-issue URL** needs a GitHub account and puts a name on it. Not anonymous,
  and a wall for exactly the person who just hit a crash.

The service:

- accepts one JSON record plus the note, size-capped, rate-limited per source address (the
  address is used for the limit and **not stored, not put in the issue**);
- computes a **fingerprint** — program, version, and the panic location or exit status — and
  keeps one GitHub issue per fingerprint: the first report opens it (label `crash` or `failure`,
  title in the repo's voice: *"Studio panicked at control.rs:533 on v0.1.0-312"*), later reports
  add a comment with the note and bump a count in the title;
- posts through the project's token, which lives only on the server;
- answers the client with the issue URL and the count, so the screen can say where it went.

The record it posts is the record it received: it adds nothing about the sender. The service's
own logs keep addresses for the rate limiter only, for a day.

## What this does not do, on purpose

- It never sends without a press. There is no "always send" switch in this version; if one is
  ever added it belongs on the AI page beside the other grades, and the README sentence changes
  with it.
- It does not collect usage, feature counts, or "anonymous analytics". Crashes and failures only.
- It does not send the whole log. Forty lines from the process that broke, scrubbed.
- It does not open an issue per report. One per signature, with a count — the number that says
  which crash to fix first.

## Order of work

1. `problems` module in the runtime, with the scrubber's tests and a panic-hook test.
2. The launcher reaper writes `crash` records; `describe` timeouts write `failure` records.
3. The screen, the notification, and `report_problem` on the shell's surface.
4. The intake service on the VPS, its GitHub token, DNS for `report.yantrikos.com`.
5. README: one sentence added to the privacy bullet — *and a problem report you chose to send*.
