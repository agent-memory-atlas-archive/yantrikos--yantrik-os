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

## …and the one place that rule is wrong

"Being attached is what makes you exist" is right for the *answering* list — you cannot be handed
a turn if you are not there — and it was wrong for **Settings → Harnesses**, which is the page a
person opens *because* a mind is missing. A harness the image ships but nobody installed, or one
whose unit died with a shell restart, was simply absent from it: no hint that the machine carries
it, that it needs an `npm install`, that it needs a config file, or that a unit merely needs
starting.

So a harness that ships with the image also carries a **manifest** beside its code, and the
Settings list is the manifests rather than the attach registry:

```yaml
# /opt/yantrik/share/harnesses/pi/harness.yaml
id: pi
name: Pi
detail: The pi coding agent, over its RPC mode   # only until it attaches and says better
docs: README.md
unit: yantrik-pi.service                          # omit for a harness something else starts

requires:                                         # missing any → "Not installed"
  - binary: pi                                    # on PATH
    why: Pi itself, which npm installs per user
  - file: yantrik_pi.py                           # beside the manifest, absolute, or ~/…
    why: the harness script

install:                                          # omit and the row names the docs instead
  command: npm install -g --ignore-scripts @earendil-works/pi-coding-agent
  doing: fetching @earendil-works/pi-coding-agent

setup:                                            # missing any → "Needs setup"
  - config: pi.json                               # ~/.config/yantrik/pi.json
    why: which provider and model Pi should use
```

From that, `PATH`, the config directory, systemd and the attach registry, each row derives one
state — **Not installed**, **Installing**, **Needs setup**, **Ready to start**, **Starting up**,
**Would not start**, **Attached**, **Answering** — and offers the one thing that moves it on:
*Install* runs `install.command` as a user job with its output streamed into the row, *Start*
does `systemctl --user enable --now <unit>`, and *Use this* appears only once something has
actually attached. The same list is in `describe shell` under `harnesses`, and the same two jobs
are `install_harness` and `start_harness`, both graded `sensitive` because they change the
machine.

Two rules this page keeps:

- **A manifest names a file; it never opens one.** A "Needs setup" row says
  `~/.config/yantrik/deepseek.json` and the name of the variable that holds the key, and that is
  the end of it. Nothing in the catalogue reads a config, so nothing it can print contains a
  credential, and there is a test that writes a key into a fixture and asserts no row carries it.
- **Only attachment makes a mind answerable.** The picker, the quick switcher and `use_harness`
  are unchanged and still work off the attach registry alone. A row on this page saying "Ready to
  start" is a row that cannot be selected, which is the truth.

Nothing about this is required of a harness. A manifest is how something gets *listed before it
runs*; attaching is still the whole protocol, and a harness that just attaches works exactly as
it always did — it appears in both lists the moment it does.

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
- **A tool call is a line of the answer beginning with `⚙️`.** `chunk` carries text and nothing
  else, so a call goes into the text: `⚙️ os_act studio.generate {"args":{"prompt":"a red
  kite"}}` — the tool's name, what it touched, and the rest of its arguments as one JSON object
  on the same line, which is what `harnesses/lib/yantrik_harness.py`'s `tool_trail` writes. The
  shell renders that line as the call — name, target, `key="value"` — cut to the panel's width,
  with the whole arguments a click away, and reads the same calls out under `calls` in
  `describe shell`'s `conversation`. It also understands Hermes' own progress lines
  (`⚙️ name...`, `⚙️ name: "preview"`, and the verbose `⚙️ name([...])` with the arguments on the
  line after), so a harness that already writes its calls down does not need this form. Anything
  a line carries is what the panel can show: Hermes in its default mode sends the name alone
  for an MCP tool, and the panel shows the name alone.

## Five harnesses exist

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

It is also where the **decider** is, because this is the harness whose loop is small enough to
see the seam in. A `decider` block in `~/.config/yantrik/deepseek.json` names a decision model —
Jev (`POST /v1/systemone`, TypeSafe), Kev (the same wire, served locally) or a `/v1/decide`
endpoint — and each step it is asked, in one request, whether the request is finished, which app
the next step uses, and which of that app's actions. Above a confidence gate the chat model is
then given `os_act` as its only tool, with `tool_choice` on it and the app and action already
fixed in the schema, so all it writes is the arguments. Below the gate the generative tool call
happens exactly as before and the disagreement is logged. No block means none of it runs, and
the whole thing is tested offline against a fake System One server.

**It picks; it does not write.** That line is the design and not a caveat. The decider chooses
among things the desktop already published — an app in `os_apps`, an action in `os_describe` —
and answers one yes/no question; every word that reaches an app is still the generator's. Four
things that shape carries with it:

- **A decision model can only choose an option it was given**, so the option list has to be
  complete or not offered at all. An `os_describe` answer that was too long to keep whole is a
  partial action list, and a partial list is worse than none, because it does not make the
  answer uncertain — it makes it confidently wrong. `none of these` is always among the apps,
  which is what turns "nothing here applies" into an answer rather than a wrong app.
- **The cheap half cannot depend on the expensive half's habits.** Attached to a live desktop,
  Kev answered nothing at all on the first real question put to it: the system prompt asks the
  model to start with `os_apps`, and a model that already knows this desktop went straight to
  `os_describe` — a good answer that left the decider with no list of apps and nothing it could
  be asked. So the harness reads `os_apps` itself at the start of a question, once, when the
  conversation does not already hold a listing. It shows in the trail like any other tool call,
  because a tool call the person cannot see is worse than a line they did not need.
- **Standing aside has to be as loud as falling back.** The first version of this returned
  quietly when there was nothing to ask, and the failure above was invisible in the journal —
  indistinguishable from a decider that was asked and disagreed. Every step the decider is not
  asked now says so and why.
- **Questions in one request cannot read each other, and the id is not sent to the model.** So
  each question's instructions carry their whole meaning, and the speculative ones say so —
  "suppose the next step is taken with `calendar`, whether or not another app would be a better
  choice; that is decided elsewhere".
- **A refusal is not a thing to decide around.** After a `REFUSED` the decider is stood down for
  the rest of that question. What a refusal means is in the system prompt, which the generator
  reads and the decider is not shown, and the wrong reading of it — another route to the same
  thing — is exactly what a fast pick would produce.
- **The gate is the whole of the protection, and it is a probability rather than a measured
  accuracy rate.** A pinned step is a step the model is forced to take, so the only honest thing
  to do with the number is log it: one line per step with the pick, the probability, the latency,
  whether the gate held, and what the generator picked on every step where both of them did.

**OpenClaw** (`harnesses/openclaw`) is the local-first personal agent: a gateway daemon on
127.0.0.1, a primary agent that spawns sub-agents, its own channels, and persistent memory. It is
the only one attached with `memory=true` and the only one whose tools this repo does not carry —
OpenClaw has an MCP client of its own, so `yos-mcp` is registered in the person's
`~/.openclaw/openclaw.json` under `mcp.servers` with `env.YOS_MCP_REQUESTER=OpenClaw`. Nothing
about that passes through this harness, which is the right shape: a mind that already knows how
to hold tools should be given the tools, not a proxy for them.

This harness was written on a machine with no OpenClaw checkout and no network, and then run
against **OpenClaw 2026.9.1** on the live image. Everything below is what that install does;
where it disagreed with the blind version, the disagreement is recorded rather than quietly
fixed, because the failure modes are the interesting part.

- **`openclaw agent`'s flags were guessed wrong twice, and `--json` is not a stream.** The
  message is `--message <text>`, not a positional; the session is `--session-key <key>`, and
  there is no `--session`; and `--json` prints one indented JSON document when the turn is over
  (`{runId, status, summary, result: {payloads, meta}}`). A JSON-lines reader pointed at that
  document fails on every line and emits each one as text, so the first thing the person would
  have seen on this route is the raw JSON in their chat panel. The harness now collects stdout
  whole, and a run that prints nothing readable is reported as that rather than as an empty
  answer — an empty bubble cannot be told from a hang.
- **The tool trail exists on one route and not the other, and it says so.** The CLI route builds
  it from the run's own `result.meta.toolSummary`, which names the tools and not their arguments.
  The gateway route has none: OpenClaw's internal tool calls stay internal on that surface.
- **The streaming route is HTTP, not the WebSocket, and that is an authorization fact rather
  than a spelling one.** The gateway's control plane is `{type:"req", id, method, params}` at the
  bare root path — the blind envelope was wrong, and so was the path probing. But correcting the
  spelling would not have helped: a `connect` carrying only the shared gateway token is accepted
  with `auth.scopes: []`, so `chat.send` answers `FORBIDDEN / missing scope: operator.write`.
  Scopes come from device pairing — an Ed25519 identity signing a challenge-bound payload,
  approved with `openclaw devices approve` — and there is no Ed25519 in the standard library.
  The harness therefore uses the gateway's OpenAI-compatible route,
  `POST /v1/chat/completions` with `stream: true` and `x-openclaw-session-key`, which OpenClaw's
  own docs describe as "a normal Gateway agent run (same codepath as `openclaw agent`)". It is
  off until `gateway.http.endpoints.chatCompletions.enabled` is true, and the 404 that says so
  names that setting.
- **Tools are gated by the tool profile, not by a dashboard approval.** The blind version told
  people to approve a scope on the gateway's dashboard; there is no such step. A configured MCP
  server is a plugin-owned tool under `bundle-mcp`, and `tools.profile` decides whether the agent
  sees it — `minimal` plus `alsoAllow: ["bundle-mcp"]` is what leaves OpenClaw with the desktop's
  eleven tools and none of its own ungraded shell, file and browser tools. OpenClaw also prefixes
  a server's tools with its name, so the desktop's `os_apps` reaches the model as
  `yantrik-os__os_apps`; the harness's preamble names them that way, because a mind told to start
  with a tool it cannot see spends its first turn guessing.
- **An unrecognised event is reported as unrecognised.** The failure this exists for is a
  protocol mismatch that looks exactly like an agent which has gone quiet: a decoder that
  silently drops what it does not know turns a five-minute config fix into an afternoon.
- **A daemon that is not running is an answer, not a wait.** Connecting is retried with backoff
  and then the turn is failed with the sentence that names the command to run, which is the whole
  difference between a harness a person can debug and a cursor that never stops blinking.
- **Ending a stream is not the same as closing it.** Two versions of one bug. The shared
  `end_process` closes a child's stdout before terminating it, which deadlocks when a reader
  thread is blocked inside that stream, so the CLI route signals the child first. And
  `http.client` hands the socket to the response for any reply that will close the connection,
  clearing `HTTPConnection.sock` — so `conn.close()` on a live stream is a no-op and the reader
  blocks forever. The gateway route keeps the socket it captured before `getresponse()` and shuts
  that down. A `/stop` that leaves the reader blocked is a turn that never closes.

All four ship as source in the image and none is started: a machine that has not been configured
never talks to a provider. `harnesses/lib/yantrik_harness.py` is the half they share — attach,
poll, heartbeat, `/stop`, `/new`, the MCP client, and one `_close` that every path out of a turn
goes through — and `harnesses/tests` runs all of it offline against a fake desktop, a fake
bridge, a fake chat API, a fake System One endpoint, a fake `pi` and a fake OpenClaw gateway. The
fakes answer the way the real things answered when each harness was run against them, which is
the only reason the offline suite is worth anything: a fake that agrees with a guess proves the
guess is self-consistent and nothing else.

## What is not here yet

Tasks, events with sequence numbers, approvals as a first-class message and automations are
designed (`design/hermes-on-yantrik.html`) but not in the protocol: today a long task is one turn with
its progress streamed as text, and an approval is a line of that text the person answers by
typing `/approve`.
