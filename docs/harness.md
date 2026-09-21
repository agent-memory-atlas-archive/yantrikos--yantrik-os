# Attaching a harness

Yantrik OS does not run your agent. It does not hold your endpoint, your model name or your API
key, and it has nowhere to put them. `yantrik-mind` has its own setup, hermes-agent has its own,
OpenClaw has its own — that work is done, and doing it again in this OS would mean doing it worse
and keeping it in sync forever.

What the OS owns is **which mind the person is talking to**. This is the interface for becoming
one of the candidates.

## The whole thing

Six methods on the `harness` socket, all spoken by the harness:

```text
harness.attach   {id, name, detail?, tools?, memory?}  → {session}
harness.poll     {session}                             → {turn_id, text, context} | {}
harness.chunk    {session, turn_id, delta}             → {}
harness.complete {session, turn_id}                    → {}
harness.fail     {session, turn_id, error}             → {}
harness.detach   {session}                             → {}
```

Attach, then loop: ask for a turn, stream the answer back in pieces, say you are done.
`crates/yantrik-harness/examples/echo_harness.rs` is a working one end to end, and the only part
a real harness replaces is the function that produces the answer.

## Why the harness dials in

The OS never connects to you. That is deliberate, and three useful things follow:

- **Anything with a JSON-RPC client can be a harness.** No callback URL, no inbound port, no
  reachability requirement. A harness in a container or behind NAT works like one running beside
  the shell.
- **The OS stores nothing about you.** There is no config file to install and no field anywhere
  for an endpoint or a credential. The protocol has no place to put one, and there is a test that
  fails if a field is ever added.
- **Being attached is what makes you exist.** You appear in the picker when you attach and are
  gone when you stop polling. Nothing to deregister, and nothing can be listed that is not
  actually there.

## Driving the desktop

Separate, and it already exists. An attached harness reads and steers the OS through the control
surface every app publishes — `app.describe` and `app.act`, or the `yos` command — which is
graded `safe`/`standard`/`sensitive`/`dangerous` and enforced against your ceiling. See
[app-control.md](app-control.md).

Attaching is about the conversation. Driving is about the desktop. Keeping them apart means a
harness can do either without the other: a mind that only talks never needs permissions, and a
script that only acts never needs to attach.

## Rules worth knowing

- **`id` is what a person types to select you**, so it cannot be empty or contain spaces.
- **You cannot attach over a built-in.** The companion is compiled into the shell; a client
  taking its id could leave a machine with no working mind and no way to say so.
- **Re-attaching under the same id replaces the old you.** That is what a harness that crashed
  and came back should get, and any turn the old one owed is failed rather than left hanging.
- **Stop polling and you are dropped** after 90 seconds, with anything you owed failed so nobody
  is left waiting on an answer that is not coming.
- **Nothing waiting is an ordinary reply**, not an error. You will poll far more often than a
  person types.

## Four harnesses exist

**Yantrik Mind** attaches from its own process (`crates/mind-core/src/harness.rs` in its repo).
It is the reference for a mind written in Rust that already has its own model and memory.

**Hermes Agent** attaches through a plugin this repo ships, `harnesses/hermes`, because Hermes
is a gateway with its own platforms (Telegram, Slack, IRC) and this makes the desktop one more
of them. To install it on a machine that already runs Hermes:

```sh
cp -r harnesses/hermes ~/.hermes/plugins/yantrik
hermes plugins enable yantrik-desktop
systemctl --user restart hermes-gateway    # or however Hermes is started
yos act shell use_harness id=hermes        # once it appears in the picker
```

Hermes keeps its model, endpoint, keys and memory in `~/.hermes`, as it always has. The plugin
reads none of it except the model name, which it passes as the `detail` the picker shows.

**Give the desktop platform the desktop's tools, not Hermes's own.** Hermes arrives with a
`terminal`, `file`, `code_execution`, `browser` and `web` toolset of its own. On this desktop
they are a second, ungraded route to everything the apps already offer — and each `terminal`
call stops on Hermes's own approval, which reaches the person as a paragraph of text to answer
with `/approve`, five minutes at a time. The first long job given to it (research, slides,
calendar, a checklist) spent most of its life waiting on those. With them off it did the same
job through the Terminal, Browser, Presentation, Calendar and Notes *apps*, where every action
carries the app's grade, the person's mode decides what is asked, and the work is on screen.
The `hermes tools` command does not know plugin platforms, so set it in `~/.hermes/config.yaml`:

```yaml
platform_toolsets:
  yantrik: [skills, todo, memory, session_search, clarify, delegation, yantrik_os]
delegation:
  max_iterations: 25        # a research sub-agent that may take 50 turns will take 50
```

`delegation` is worth keeping: a sub-agent inherits the desktop's tools, so "spin up a research
agent" works and its work is as visible and as graded as the parent's.

Two things a gateway-shaped harness has to get right, both learned by running one:

- **Close every turn exactly once.** The desktop is waiting on the turn it handed over, and a
  gateway has paths that answer without going through its own completion hook — a `/stop`, a
  command answered inline. Anything that leaves a turn open leaves the desktop waiting forever,
  and a heartbeat keeps it waiting convincingly.
- **A message that arrives while you are working is a turn too.** Queueing it behind the current
  one is fine for a chat app, where nothing is owed; here the turn it came from is owed an
  answer. Answer it — even if the answer is "still working on the last one".

**Pi** (`harnesses/pi`) is the [pi coding agent](https://www.npmjs.com/package/@earendil-works/pi-coding-agent)
driven over its RPC mode: `pi --mode rpc` on a pipe, one JSON line per message, each desktop turn
fed in as a `prompt`. It assumes Pi brings everything — provider, key, model, session, loop — and
that the harness's whole job is carrying text and closing turns.

Pi has no MCP client, so the desktop's tools reach it through a Pi extension
(`harnesses/pi/extension/yantrik-os.ts`) that asks `yos-mcp` for its tool list and proxies every
call to it. The extension decides nothing: modes, grades, cards and the taint rule stay in the
bridge, which is the only place they can be kept correct.

Three things it taught, all about ending:

- **An agent has more ways to finish than to start.** `agent_settled`, an `agent_end` that is
  never followed by one, a `response` that says the prompt was refused, and the process exiting
  are four different endings, and each one is a turn the desktop is holding open. They all have
  to arrive at the same single close.
- **Silence is not an ending, but it has to become one.** A harness that waits forever on a mind
  that has stopped talking leaves the person watching a cursor. Failing after a while is worse
  than answering and better than hanging — and the timeout has to exceed the longest legitimate
  silence, which on this desktop is an `os_act` waiting about 270 seconds for someone to answer
  an approval card.
- **A harness must not answer a dialog.** Pi can open its own `confirm`, and answering it is the
  most natural thing in the world to automate. It is declined, and the question is repeated into
  the conversation instead: this desktop already asks for permission in a way the person sees and
  the machine records, and a second approval path that nobody can see is worse than an
  inconvenient one.

Pi's own `bash`, `read`, `write` and `edit` are off by default, for the reason in the Hermes
section above — on this desktop they are an ungraded second route to what the apps already do.
Turning them on is one line in `~/.config/yantrik/pi.json` and is the person's call.

**DeepSeek** (`harnesses/deepseek`) is the opposite end: no agent, just a model. It is a plain
tool-calling loop over an OpenAI-compatible `/chat/completions` — stream the answer, collect the
tool calls, run them through `yos-mcp`, append the results, go again — and it is the reference
for attaching something that is only an endpoint and a model name. Nothing in it is
DeepSeek-specific but the defaults; it is tested against a fake server and runs unchanged against
any OpenAI-compatible endpoint, which is how it can be exercised on a machine with no DeepSeek
key at all.

What it assumes, and what that cost:

- **Tool calls arrive in pieces.** A streamed `tool_calls` delta splits the function name, the
  id and the arguments across chunks, several calls can be in flight in one assistant message,
  and some providers resend the whole name on every chunk instead of a fragment. Concatenating
  blindly turns `os_act` into `os_actos_act` and the call comes back as an unknown tool.
- **A model's thinking is not its answer.** `reasoning_content` is dropped rather than streamed:
  the person asked a question, and on a panel the deliberation reads as rambling.
- **The key exists in exactly one place.** It goes in the `Authorization` header and nowhere
  else — not into a log, a chunk, an exception, or the conversation history if the provider
  echoes it back. Every string the module can produce is redacted, and a test drives the whole
  loop against a server that deliberately echoes the header to prove it.
- **Everything the mind reads goes to the provider.** Every tool result — the note it opened, the
  page it read, the calendar it looked at — is in the next request, because that is what a
  tool-calling loop is. The README says so in those words, and the config file is the person's
  rather than the machine's.

Both ship as source in the image and neither is started: a machine that has not been configured
never talks to a provider. `harnesses/lib/yantrik_harness.py` is the half they share — attach,
poll, heartbeat, `/stop`, `/new`, the MCP client, and one `_close` that every path out of a turn
goes through — and `harnesses/tests` runs all of it offline against a fake desktop, a fake
bridge, a fake chat API and a fake `pi`.

## What is not here yet

Tasks, events with sequence numbers, approvals as a first-class message and automations are
designed (`design/hermes-on-yantrik.html`) but not in the protocol: today a long task is one turn with
its progress streamed as text, and an approval is a line of that text the person answers by
typing `/approve`.
