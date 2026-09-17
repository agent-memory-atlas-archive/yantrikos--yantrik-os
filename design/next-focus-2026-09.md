# What to build next, and why

Settled 17 September 2026, in a three-round design review with GPT-6 Astra, after a day spent
driving this desktop with two minds attached and fixing what broke.

## The pattern behind today's bugs

Every failure was the same mistake at a different boundary: **an advertised interface was treated
as proof of an executable contract, while the real capability, ownership and lifetime of the work
stayed implicit.**

"App exists" meant a name was registered, not that anything could open it. "The tool takes
arguments" meant the schema looked plausible, not that the model could emit them. "The task is
alive" meant some receiver was still connected. "This approval belongs here" meant some process was
scanning this conversation.

The repair, each time, was the same move: name the thing, key everything by that name, and make the
owner explicit. The running-apps registry now keys an app to the process that owns its window, not
to whichever copy launched last. The chat pump writes to the row it opened, not to whatever row is
last. The launcher asks whether an app can open, not whether its name is known.

**Work is the last unnamed thing.** A forty-minute task is a chat turn whose identity is a live
channel, which is why closing one killed the other, why an approval reached the wrong run, and why
"what is it doing now" can only be answered by reading another process's database.

## 1. Durable runs, independent of the chat — 3–5 days

Promote the turn id the OS already mints into a persisted run: sequenced events on disk, state
(running / waiting-on-person / done / failed / cancelled / orphaned), input requests answered by
id, ownership bound to the connection that started them. No resumable execution, no scheduler, no
second id namespace.

- `run.events(after_seq)` paginates; `run.answer(run_id, request_id, answer)` consumes a request
  atomically and rejects duplicates and stale replies; `run.cancel` ends one.
- Chat subscription and run lifetime are separate. Closing a panel must not cancel work.
- A superseded connection is refused. Disconnect or host restart orphans unfinished runs: readable,
  not resumable. Replay only reads history; it never re-executes.
- A harness may post a notice without a person having typed anything first, optionally against a
  run. The desktop decides whether that reads as a notification or a message.
- `describe` keeps its short summary and points at the paginated API — the 600-character clip is
  fine for a glance and useless for watching work.

**Acceptance, on the machine:** a scripted harness starts two runs, streams a message longer than
600 characters, and asks for input on both. Answering the first request twice lands exactly once,
and the second run is still waiting. Close and reopen the chat: every event comes back by sequence.
Mutate from the old connection after a reconnect: refused. Restart the host: unfinished runs are
readable and orphaned. Restart Hermes: no worker and no command resumes. Post a notice with no
preceding turn: it appears. The protocol still has no field for an endpoint, a model or a key.

**Stop rule:** if execution takeover, scheduling, sub-agents or a second task-id namespace get into
the patch, cut them. Ship durable observation and explicit interruption, not recovery.

### Where it lands

- **`crates/yantrik-harness`** — `protocol.rs`: additive event, input-request and notice payloads on
  the existing six methods. New `run_store.rs`: SQLite runs and events, unique `(run_id, seq)`,
  transactional request consumption, cursor pagination, orphaning on startup. `host.rs`:
  connection-local ownership, supersession, transitions, cancellation, notice ingestion. Scripted
  harness tests for duplicates, disconnect, restart and replay; `docs/harness.md` gains the
  transition and error tables.
- **The shell** — a run client on the control surface (`run.events`, `run.answer`, `run.cancel`);
  chat state keyed by run instead of owned by a stream; Slint gains state badges, an input control
  bound to a specific request, cancel, and notices. Prose is never interpreted as authorization.
- **`harnesses/hermes`** — map progress, structured approval requests, completion, failure and
  notices; deliver an answer to the exact pending approval callback rather than the conversational
  scanner; supervise workers, killing owned process groups on connection loss; request deadlines;
  auto-resume off properly (the freshness window set to 1s today is a stopgap, not a switch).
- **Yantrik Mind** — the same structured events, request-id routing, cancellation, disconnect
  cleanup and notices in its harness client, with governance and memory untouched. One conformance
  fixture runs against both adapters. Streamed text alone is legacy compatibility, not a
  first-class mind.

## 2. A release gate on an installed machine — 2–3 days, session lifecycle first

Move the ISO session off `.bash_profile` onto a supervised session unit, timeboxed to one day,
because the restart and upgrade tests cannot pass without it. Then gate publication on deterministic
tests run against a fresh ISO install and an upgraded previous release.

The suite drives `describe`/`act`, checks effects, and exits nonzero. A mind may launch it and
diagnose failures; it may not waive an assertion. Evidence is scoped honestly: assert a responding
app socket plus the owning process, use process liveness where an app has no socket and label that
weaker, keep screenshots as artifacts. **Window visibility stays unverified** and the gate says so —
today's witnesses cannot prove a mapped window is on screen, and the shell's own bookkeeping is not
a witness (it was the bug).

**Acceptance:** fresh install and upgrade both reach a usable shell, as does a supervised restart;
the six regressions found today are covered, plus disconnect, approval routing and outward-action
denial; a forced failure blocks publication and keeps the artifacts.

**Stop rule:** if the session conversion passes a day, ship the gate with reboot-based upgrade
verification and leave hot restart unsupported. No compositor inspection subsystem.

## 3. Run-bound approvals on the OS's own grades — 2–3 days

A grant binds (run, action, server-validated argument constraints, requester), is rechecked against
the live grade at execution, and dies with the run. `safe` OS actions need no grant; outward
operations always name the destination the person approved. Structured, OS-authorized calls are
exempt from Hermes' shell scanner; raw shell stays Hermes' business, with no blanket "read-only
pipeline" rule — `cat private | curl --data-binary @-` starts as a read and ends as disclosure.

**Acceptance:** denial prevents execution; a duplicate reply authorizes nothing; changing a path or
destination invalidates a grant; a changed grade is caught at execution; grants die with the run.

**Stop rule:** if constrained run grants pass the timebox, ship once-only exact-argument grants.
Defer general command analysis and persistent "always" grants.

## Left broken on purpose

Autonomous long-run recovery, and automatic proof that a window is on screen. Bounded execution
with honest evidence is worth more than premature orchestration, or a new compositor-testing
project wearing a release gate's clothes.
