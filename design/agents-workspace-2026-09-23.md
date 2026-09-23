# Agents — one pane per agent, with its work inside it

*23 September 2026. Asked for by Pranab: "Think about how we use Hermes or pi now and how seamless
it is. Spin up multiple agents. Currently the agent opens up a shell and we can see the output
while it should be inside the agent… Agent shell, and each will have one agent and details."*

## What happens today

Four minds are attached to VM 520 right now — Hermes, pi, OpenClaw and DeepSeek — and a person can
talk to exactly one of them, in one conversation, in the Lens. When that mind runs a command:

1. it calls `os_act terminal.run` through `yos-mcp`;
2. if the Terminal app is not open, it is told to open it, and does — a separate window;
3. the command is **typed into the person's own active tab** (`apps/terminal/src/main.rs:907`);
4. the Terminal's UI thread sleeps up to 900 ms and returns whatever lines appeared by then —
   no exit status, no output that came later;
5. `yos-mcp`'s `follow()` raises the Terminal window over everything.

So the person watches a terminal window they did not open fill with commands they did not type,
two minds (or a mind and the person) share one shell, and the conversation shows a line of text
— `⚙️ os_act terminal.run` — with nothing under it. The harness wire has two kinds of chunk,
`Text` and `Failed` (`crates/yantrik-harness/src/lib.rs:83`); a tool call, its result and a
command's output have no way to reach the shell except as prose. #125 makes that prose readable
(the call's arguments, a click away). This design gives it structure.

Using pi or Hermes in a terminal is seamless because the agent's work is *in* the agent: each tool
call is a block, the command's output is under it, a long one streams, a failed one is red, and
the session is one scrollable thing that belongs to one agent. That is the shape to build.

## The shape

A screen called **Agents** — in the dock and the launcher, `open_app agents`, `show_screen agents`
— because the record belongs to the desktop, not to any one mind: a mind can be replaced or its
harness stopped; the pane survives both.

```
┌ Agents ───────────────────────────────────────────────────────────────────────────────┐
│ + New agent          │ pi · "tidy the photos folder"                  ● running 2m14s │
│                      │────────────────────────────────────────────────┬───────────────│
│ ● pi                 │ you  tidy the photos folder, dupes into Trash  │ Mind   pi     │
│   tidy the photos…   │                                                │ Model  qwen…  │
│   running · 2m       │ pi   I'll find duplicates by hash first.       │ Since  21:04  │
│                      │ ┌ ▶ run  fdupes -r ~/Pictures ─────── ✓ 0 ─┐  │ Turns  3      │
│ ◐ deepseek           │ │ ~/Pictures/a.jpg                          │  │ Calls  14 (1✗)│
│   release notes      │ │ ~/Pictures/copy of a.jpg                  │  │ Commands 5    │
│   waiting for you    │ │ … 212 more lines            [show all]    │  │ Files  38     │
│                      │ └───────────────────────────────────────────┘  │ Approvals 1/1 │
│ ✓ openclaw           │ ┌ ▶ files.move  38 files → Trash ── ⏳ ask ─┐  │ Tokens 41k    │
│   weather summary    │ │  [ Allow ]  [ Deny ]   sensitive           │  │               │
│   done · 21:02       │ └───────────────────────────────────────────┘  │ [Stop] [Close]│
│                      │ ┌ type to pi… ───────────────────────────────┐ │               │
└──────────────────────┴───────────────────────────────────────────────┴───────────────┘
```

- **Left: the agents.** One row per agent: its mind, the task (its first prompt, cut), and its
  state — *thinking*, *running a tool*, *waiting for you*, *idle*, *done*, *failed* — with how
  long. **New agent** picks a mind and takes the first prompt. Rows needing the person sort to the
  top.
- **Middle: the agent's session.** The person's prompts, the mind's text, and each tool call as a
  **card**: its name, what it touched, its arguments on one line (all of them a click away — #125's
  rendering, reused), its state (running / ✓ / ✗ with the exit code), and its **output inside the
  card**. A command's card holds that command's own terminal, live while it runs, the last lines
  when it ends, all of it on a click. An approval the call needs appears *as* the card, with Allow
  and Deny, in the pane of the agent that asked.
- **Right: details.** Mind and model; when it started; turns; tool calls and how many failed;
  commands and their exit codes; files it touched, where the arguments say; approvals asked and
  given; tokens and cost when the harness reports them. Stop, and Close.

The Lens stays what it is — the quick question to the active mind — and shows the active agent's
thread, with "open in Agents" for the whole of it.

## Four decisions that carry the weight

### 1. The unit is a conversation, not a harness

An agent is **one conversation with one mind**: `AgentId = "<harness>:<conversation>"`, e.g.
`pi:c-7f3a91`. The conversation part is **issued by the host**, random and never reused, so an id
from yesterday's session cannot name today's agent. Today the host has one `active` harness and routes every turn to it
(`host.rs:168`); every harness keeps exactly one conversation (Hermes' `CHAT_ID = "desktop"`,
pi `--no-session`, OpenClaw `yantrik-desktop`).

- `harness.attach` gains `conversations: true` for a harness that can hold more than one. Each
  assignment it polls then carries `conversation: "c3"`, and it keeps a separate history per
  conversation. A harness that does not say so gets one conversation, and its pane says so
  plainly ("Hermes holds one conversation at a time") instead of pretending.
- The host addresses a turn to an agent, not to "the active mind". `active` survives only as the
  Lens's default.
- **Spinning up agents** is starting conversations: one pi *process* per conversation (its RPC mode
  is one session per process), a history per conversation for DeepSeek, a session name per
  conversation for OpenClaw. The shell caps live agents (six to begin with) and says so when the
  cap is reached.
- A mind can spin up agents too — `shell.new_agent {mind, task}` — and a child's row shows its
  parent. That is the fan-out a person does by hand today, made visible. It is a **gated act**
  (`sensitive`: in Ask mode the person sees a card), children **cannot spawn** (depth one), a
  parent may hold at most three children, and the global cap still applies. A child starts with
  **no grants** — the parent's session grants are not inherited — and Stop on a parent stops its
  children.
- One turn at a time per conversation: the host queues the next turn for an agent until the
  current one completes. Different agents run at once.

Also fixed on the way: `Host::poll` takes the *newest* queued turn (`queued.pop()`, host.rs:315);
turns are handed out first-in, first-out.

### 2. Structured events beside the text

A new method, `harness.event {session, turn_id, event}`. Text still travels as `harness.chunk`;
an event says what the agent is *doing*:

| kind | carries | shown as |
|---|---|---|
| `tool_start` | `call`, `name`, `target`, `args` | a card opens, running |
| `tool_output` | `call`, `stream`, `delta` | text inside that card |
| `tool_end` | `call`, `ok`, `summary`, `exit_code?` | the card settles ✓ / ✗ |
| `thinking` | `delta` | a folded "thinking" line |
| `status` | `text` | the agent's state line ("waiting for approval") |
| `usage` | `model`, `input_tokens?`, `output_tokens?`, `cost_usd?` | the details panel |

The types are `crates/yantrik-harness/src/event.rs`, and every piece below builds on them.

**Events are the harness's claims.** A harness can say a call succeeded when it did not, or
leave a call out. So the pane draws two kinds of card and says which is which: *reported* (from
`harness.event`) and *verified* (from what the shell itself did — the commands it ran through
`agent_run`, their output and exit codes, the approvals it drew, the acts the runtime dispatched
for this agent, #148). The details column counts only verified facts; "files touched" is
listed only where a verified act names them.

**The host enforces a lifecycle.** An event is accepted only for a turn in flight, from the
session that holds it, and a call's events in order (`tool_start` before its output and end, one
end). `complete` and `fail` are terminal: later events for that turn are dropped and counted, and
any call still open is settled *interrupted*. One event is at most 64 KiB; a call's retained
output is at most 2 MiB, and past that the card keeps the head and the tail with a marker saying
how much was dropped. An unknown kind is ignored; a malformed event of a known kind is logged and
counted, not silently lost. An
event of a kind this shell does not know is ignored, not an error — a newer harness must not break
an older desktop. A harness that sends no events at all keeps working exactly as today; its tool
calls still show, read back out of its text by #125's trail reader.

The Python library gains `turn.tool_start / tool_output / tool_end / thinking / usage`; pi maps
`tool_execution_start / _update / _end`, DeepSeek its own loop, OpenClaw what its JSON gives.

### 3. A mind's commands run in its own terminal, inside its pane

The person's Terminal stays the person's. A command an agent runs gets **its own PTY, owned by
the shell, in that agent's pane**:

- The shell gains `shell.agent_run {agent, command, cwd?, wait?}` (graded `sensitive`, like
  `terminal.run`). It starts the command in a fresh PTY (`yantrik_terminal::Session` is already a
  library — `apps/terminal/Cargo.toml` `[lib]`), in the agent's working directory, and streams the
  terminal into the call's card as `tool_output`.
- It **settles when the command exits**, and answers with the exit code (or the signal), the
  working directory after, and the output's tail (capped; the whole is in the pane). A command
  still running at `wait` (default 120 s, at most 600) answers `running: true` with a job id; the
  card keeps streaming; `agent_job {job, wait}` waits for it and returns the same answer,
  `agent_input {job, text}` writes to it, `agent_kill {job}` stops it. A job that finishes after
  its call returned is reported to the agent in the context of its next turn.
- **One command, one PTY, one card**, and a deliberate rule about state: **the working directory
  carries from one command to the next; shell state does not** — exported variables, functions
  and activated environments end with the command. That is exactly the rule of Claude Code's own
  Bash tool, the experience this is measured against. The command runs under a small wrapper that
  writes the final `pwd` to a separate pipe, so the new directory is read from the command itself,
  not guessed afterwards. A persistent interactive shell was the alternative; it interleaves
  commands and needs prompt markers to find their boundaries, and a model's commands do not need
  its state.
- **Each command is its own process group** (the PTY makes it a session leader), and Stop,
  `agent_kill`, a timeout, a closed pane or a shell exit kill the **whole group**, not one pid.
  The environment is built, not inherited: `HOME`, `USER`, `PATH`, `LANG`, `TERM`, the agent's
  directory, and nothing from the shell's or the harness's own environment. Output is capped as in
  decision 2. **The working directory is not a sandbox**: a command can read what the user can.
  The guard is the grade (`agent_run` is `sensitive`) and #116's one rule on every door; a
  Landlock profile per agent, as perception-service already uses, is a later step.
- **A command that asks for input** — `sudo`'s password, `ssh`'s host key, `git`'s editor — shows
  the prompt in its card, the state line says *waiting for you*, and **the person** answers by
  typing into the card: it is a real terminal. What the person types goes to the command, never
  into the mind's transcript. A command silent for 20 s while reading its terminal is marked
  *waiting for input?* so an agent never just looks hung.
- **Routing, and who an agent is.** An agent id is never taken on a caller's word. When the host
  hands an agent its first turn, the assignment carries an **agent token**: 128 random bits, known
  to the shell and that harness. The harness passes it to the `yos-mcp` it starts for that
  conversation (`YANTRIK_AGENT_TOKEN`), and every act from that bridge carries it. The shell
  resolves the token to the agent **and** checks, by walking `/proc` from the socket peer as the
  approval card already does, that the caller descends from the harness process that attached
  (the host records its pid from the peer credentials at attach). A token from the wrong process
  is refused, and an act, job or event can only ever touch its own agent's pane. The limit is
  stated: processes of the same user can read each other's environment, so this stops confusion
  and casual impersonation, not a hostile program running as the person — that is what the grades
  are for.
- With a token, the bridge sends `terminal.run` to `shell.agent_run` and does not raise the
  Terminal window, and every act it makes is recorded against that agent (the approval card and
  the ledger, #148, say which agent asked).
  pi's extension also offers its own `bash` tool through the same call, so pi keeps the tool it
  was trained on and the command lands in the pane. A person's `yos act terminal run`, and any
  caller with no agent, behave as today.

### 4. The approval lives where the agent is

An approval request carries the agent (from the token, never from the request's text); the card
renders in that agent's pane and in the Lens. Nothing about the grades changes — #116 puts the same
rule on every door; this only decides where the question is drawn.

- The card is **the shell's**, drawn in the shell's own style and never from agent content: an
  agent's text or a reported card cannot draw an Allow button. It names the agent, the verified
  caller, the exact action and arguments, the grade and the scope.
- Allow and Deny resolve **one request id**, once: answering in the pane answers it in the Lens and
  the other way round, and the second view shows who answered and when.
- The agent list does not reorder under the pointer: a row that needs the person moves to the top
  only when the pointer is not over the list.
- A command's terminal is drawn by the emulator from cells: OSC 52 clipboard writes, hyperlinks and
  title changes from a command do nothing in a card.

## When a harness dies, and closing a pane

- The pane is the desktop's, kept on disk (bounded, `~/.local/share/yantrik/agents/`), so it reads
  the same after the harness exits or the shell restarts.
- A harness that stops polling for 90 s is gone: its open calls are settled *interrupted*, its
  pending approvals are withdrawn (a card for a mind that is gone is refused, never granted), and
  its row says *harness gone*. Commands it started through `agent_run` belong to the shell and keep
  running; the card says so and offers Stop.
- **Stop** ends the work: the turn is aborted (`/stop`), the agent's commands and its children are
  killed. **Close** removes the row; a pane with work still running asks first.

## What this is not

- **Not a new mind.** The OS still owns which mind the person is talking to and nothing else; the
  harness still owns its model, config and tools.
- **Not the ledger.** #148 is the record of every act on every door, agent or not; a pane is one
  agent's session. Both are fed by the same events.
- **Not Hermes, yet.** `harnesses/hermes/adapter.py` and `desktop.py` are not changed by this work
  (standing instruction). Hermes appears as one agent, its tool calls read from its text (#125),
  its commands run by its own `terminal` toolset if enabled. Giving Hermes conversations and
  events means changing the adapter — Pranab's call.

## The work, in the order it can land

| # | Piece | Depends on | Touches |
|---|---|---|---|
| 0 | This doc + `event.rs` | — | `crates/yantrik-harness` |
| 1 | **Wire**: `harness.event` with the lifecycle and caps, conversations with host-issued ids and agent tokens, one turn at a time per conversation, FIFO, `Chunk::Event`, `Host::agent_for_token`; Python lib emitters; pi (process per conversation) and DeepSeek (history per conversation); tests | rebases over #125 | `crates/yantrik-harness`, `harnesses/lib`, `harnesses/pi`, `harnesses/deepseek`, `wire/chat.rs` |
| 2 | **Agent terminal**: per-command PTYs in the shell (wrapper for `pwd`, process groups, built environment, caps, input, silence detection), `agent_run / agent_job / agent_input / agent_kill`, token check against the verified caller; then `yos-mcp` routing and pi's `bash` | #116 merged (for `yos-mcp`) | new `crates/yantrik-ui/src/agent_terminal.rs`, `control.rs`, `yos-mcp`, `harnesses/pi/extension` |
| 3 | **Agents screen**: the store (agents, sessions, cards), the Slint screen, the card with embedded output, details, New agent, Stop/Close | 0 (a fake feed until 1 lands) | new `crates/yantrik-ui/src/agents/`, new `agents.slint`, `app.slint`, `control.rs`, `dock.rs` |
| 4 | **Glue**: Lens "open in Agents"; notifications "pi finished" / "deepseek needs you"; `describe shell` → `agents`; `new_agent / send_to_agent / stop_agent`; the built-in companion as an agent | 1–3 | `wire/`, `control.rs`, `notifications.rs` |

## The red-team, and what it changed

GPT-6 (sol) reviewed the first draft on 23 September. It found: a self-declared agent id was an
impersonation switch (now a host-issued token checked against the kernel-verified caller); a
child agent could turn `new_agent` into unbounded work or inherit grants (now gated, depth one, no
inherited grants); harness events were being treated as evidence (now *reported* vs *verified*);
the event stream had no lifecycle or limits (now enforced by the host, with caps); per-command
PTYs were promising shell continuity they cannot give (now an explicit rule: the directory carries,
state does not); Stop killed one pid (now the process group, with a built environment); approvals
could be spoofed or clicked on the wrong row (now shell-owned, one request id, no reordering under
the pointer, no active terminal content); and a dead harness stranded its work (now settled,
withdrawn, and shown). It agreed with keeping the person's Terminal separate, the conversation as
the unit, panes that outlive their harness, events beside backward-compatible text, and #116's
one rule on every door.

## Done looks like

On VM 520: ask pi to tidy a folder; start DeepSeek on release notes and OpenClaw on the weather.
Three rows. pi's commands run inside pi's pane with their output and exit codes, and no Terminal
window opens. DeepSeek's `files.move` asks — the card is in DeepSeek's pane and the row sorts to
the top. `describe shell` lists three agents with their states. Closing pi's harness leaves its
pane readable, marked "harness gone".
