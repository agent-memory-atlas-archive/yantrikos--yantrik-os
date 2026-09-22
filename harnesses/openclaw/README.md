# OpenClaw as the desktop's mind

A harness that gives the Yantrik OS conversation to [OpenClaw](https://github.com/openclaw/openclaw)
— the local-first personal agent with a gateway daemon on 127.0.0.1, a primary agent that spawns
sub-agents, and its own persistent memory. Python 3.10+, stdlib only, no pip install and nothing
to build.

OpenClaw keeps everything that makes it OpenClaw: its model, its keys, its channels, its memory
and its MCP servers, all in `~/.openclaw/openclaw.json` where it already keeps them. This harness
carries text between OpenClaw and the desktop's chat panel, and closes every turn exactly once.
It reads none of OpenClaw's configuration and the desktop protocol has no field that could carry
any of it.

**Install OpenClaw from its own documentation.** This directory assumes a working install and
says nothing about how to get one.

## Read this before you trust the gateway route

This harness was written on a machine with **no OpenClaw checkout and no network**. That has one
consequence and it is worth stating plainly:

| Part | Status |
|------|--------|
| The desktop side (attach, poll, stream, close exactly once) | Verified against the fake desktop in `harnesses/tests` |
| RFC 6455 framing — masking, continuation, ping/pong, close | Verified against an independent fake server |
| The CLI route's *plumbing* — spawn, stream, exit codes, abort | Verified |
| `openclaw agent`'s actual **flags** | **Assumed.** `args` in the config replaces them |
| The gateway's WebSocket **path** | **Assumed.** Probed; `gateway_url` overrides |
| The gateway's message **envelope** (field names) | **Assumed.** One function to correct |

Nothing here was derived from `src/gateway/` or from OpenClaw's docs, because neither was
reachable. The decoder in the other direction needs no correcting — it accepts flat
`{type, delta}`, Anthropic-shaped `content_block_delta`, and OpenAI-shaped `choices[].delta`
all at once — but what this harness *sends* is a guess in both routes, and each guess lives in
exactly one place so that correcting it is an edit rather than a rewrite:

- CLI flags → `args` in `~/.config/yantrik/openclaw.json` (no code change at all).
- Gateway path → `GATEWAY_PATHS`, or just put the real path in `gateway_url`.
- Gateway envelope → `client_envelope()` in `yantrik_openclaw.py`, about fifteen lines.

**Start with the CLI route.** It is the default because its failure modes are visible: a flag
OpenClaw does not recognise is a non-zero exit with a sentence naming `args`, not a silent hang.

### Checking the assumptions against a live install

Five minutes, and it settles everything above:

```sh
openclaw --version
openclaw agent --help          # the real non-interactive flags → `args`
openclaw gateway --help        # whether it prints the WebSocket path → `gateway_url`
openclaw mcp serve --help      # the other way to hand OpenClaw tools, if `.mcp.servers` moved
```

For the envelope, open the dashboard on `http://127.0.0.1:18789`, watch the WebSocket frames in
the browser's network tab while you send one message, and make `client_envelope()` say the same
thing. If answers arrive but nothing appears on the desktop, the log says
`unrecognised event from OpenClaw: …` — that is the decoder telling you which spelling it did
not know, which is the difference between a protocol mismatch and an agent that has gone quiet.

## Install

```sh
mkdir -p ~/.config/yantrik
$EDITOR ~/.config/yantrik/openclaw.json          # optional — see below

# run it in the foreground first, to see what it says
python3 /opt/yantrik/share/harnesses/openclaw/yantrik_openclaw.py

# then as a user service
cp /opt/yantrik/share/harnesses/openclaw/yantrik-openclaw.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now yantrik-openclaw

yos act shell use_harness id=openclaw            # once it appears in the picker
```

Nothing is enabled by default. The image ships this as source and starts nothing: a machine that
has never been configured never talks to a provider.

## Giving OpenClaw the desktop's tools

OpenClaw has an MCP client of its own, so the desktop's tools do **not** go through this harness.
Register the bridge in `~/.openclaw/openclaw.json` under `.mcp.servers`:

```json
{
  "mcp": {
    "servers": {
      "yantrik-os": {
        "command": "/opt/yantrik/bin/yos-mcp",
        "env": {
          "YOS_MCP_REQUESTER": "OpenClaw"
        }
      }
    }
  }
}
```

`YOS_MCP_REQUESTER` is what the desktop's approval card prints on its **`says the caller`** line.
Without it the card reads "the mind on this desktop is asking to use this machine", which is true
and useless. It is a label and nothing more: the machine establishes who is really calling from
the socket's peer credentials and prints that separately, under `verified by this machine`, and
says so when the two disagree. Setting it to something else does not get you anything.

Then **approve the tool scope once on the dashboard.** The gateway reads `.mcp.servers` on its
first turn, and a server it has not been given scope for is listed but not callable. Open
`http://127.0.0.1:18789`, find `yantrik-os` under the agent's tools, and allow it. Until that is
done OpenClaw will answer questions but will not be able to touch the desktop, which looks exactly
like a harness that forgot to set `tools`.

Restart the gateway after editing the file (`openclaw gateway restart`, or however your install
starts it) — `.mcp.servers` is read on the first turn of a gateway's life, not on every turn.

### Give the desktop's tools, not OpenClaw's own

The same decision the Hermes and Pi harnesses made, for the same reason: OpenClaw arrives with
shell, file and browser tools of its own, and on this desktop they are a second, **ungraded**
route to everything the apps already offer — no card, no mode, no grade, no audit line. Turn them
off for the agent that answers this desktop, in OpenClaw's own config, and let `os_*` and `web_*`
be the way it touches this machine. Leaving them on is the person's call, not this harness's, and
it is worth making on purpose.

## The config file

`~/.config/yantrik/openclaw.json`, **written by you**, and optional — with no file at all this
harness runs `openclaw agent` with OpenClaw's own defaults, which is a working setup for somebody
who has already configured OpenClaw.

```json
{
  "route": "cli",
  "session": "yantrik-desktop"
}
```

| key | default | what it is |
|-----|---------|------------|
| `route` | `cli` | `cli` (per-turn `openclaw agent`) or `gateway` (the WebSocket — see the warning above). |
| `command` | `openclaw` | How to run the CLI. A string is split like a shell would; a list is taken as-is. |
| `args` | `["agent", "--json"]` | The subcommand and output flag. **Replace this wholesale** if `openclaw agent --help` disagrees. |
| `extra_args` | — | Anything else to pass, after the session. |
| `local` | `false` | Adds `--local`, which bypasses the gateway entirely. |
| `message_on_stdin` | `false` | Send the message on stdin instead of as the last argument. |
| `agent` | — | Which agent answers. Omitted means OpenClaw's primary. |
| `session` | `yantrik-desktop` | The conversation name. `/new` moves it to `-1`, `-2`, … |
| `model` | — | Display only: the left half of the picker's `detail` line. |
| `gateway_url` | `ws://127.0.0.1:18789` | Gateway route only. Include a path (`…:18789/ws`) to stop the probing. |
| `token` / `token_env` | — | A bearer token for the gateway, if yours needs one. Prefer `token_env`. |
| `connect_attempts` | `3` | How many times to try the gateway before answering "it is not running". |
| `connect_backoff` | `0.5` | Seconds before the second try, doubling. |
| `connect_timeout` | `10` | Seconds for one connection attempt. |
| `silence_timeout` | `420` | How long OpenClaw may say nothing before the turn is failed rather than left hanging. |
| `preamble` | a short desktop prompt | Sent once per session, ahead of the first message. `""` turns it off. |
| `path` | — | Prepended to `PATH`, for a per-user `openclaw`/`node` install. |
| `env` | — | Extra environment for the CLI. |

`token_env` reads the variable **in this process**. A user service does not inherit your shell, so
put it in an `EnvironmentFile` the unit names (mode 600) or in `~/.config/environment.d/`.

`silence_timeout` is 420 seconds on purpose. It has to exceed the longest *legitimate* silence,
and that is not the model: an `os_act` above this session's ceiling puts an approval card on the
desktop and waits about 270 seconds for the person to answer it, inside a single tool call, with
nothing on the wire at all.

## In the conversation

- `/stop` ends what OpenClaw is doing — the gateway's abort on one route, the child process on the
  other. Either way the turn is closed once, and within ten seconds even if nothing acknowledges.
- `/new` starts a fresh session. **OpenClaw's persistent memory is not touched**: it is OpenClaw's
  own and this harness has no business in it. What `/new` forgets is the conversation, not what
  OpenClaw has learned.
- A tool call shows as one line — `⚙️ os_act calendar.add_event` — naming what was touched. The
  arguments are not shown: they routinely hold the body of the note or the text of the message.
- A message typed while OpenClaw is working gets an answer straight away saying so. It is not
  queued: the desktop is owed an answer for it.
- The first message of each session carries a short preface about this desktop — start with
  `os_apps`, describe before acting, and **a result whose first word is `REFUSED` is an answer,
  not an error**. Set `"preamble": ""` if your agent's own instructions already cover it.

## What leaves the machine

The harness itself opens nothing but a loopback socket and a child process. What OpenClaw then
does is OpenClaw's:

- **Everything you type on this desktop goes wherever OpenClaw's model lives.** If that is a
  cloud provider, it leaves the machine. If it is a local model, it does not. This harness cannot
  tell which, and does not ask.
- **Every tool result OpenClaw asked for goes the same way** — the note it opened, the page it
  read, what is on your calendar, what `os_apps` says is running. That is what an agent with tools
  is, and it is the reason the MCP entry and the scope approval are both deliberate steps.
- **OpenClaw remembers.** It is attached with `memory=true` because it has persistent memory of
  its own, across sessions and across `/new`. Where that memory is stored, and whether it is on
  this machine, is a question for OpenClaw's config rather than for this one.
- **The gateway is loopback.** `ws://127.0.0.1:18789` does not leave the host. If you point
  `gateway_url` somewhere else, that is a network connection and a token travelling over it, and
  `wss://` is then the only sensible scheme.
- **No key is held here.** The only credential this harness can hold is the gateway's own bearer
  token, and it goes in the `Authorization` header of the handshake and nowhere else.

## When something goes wrong

`journalctl --user -u yantrik-openclaw -f`.

| what you see | what it means |
|---|---|
| `OpenClaw's gateway is not running — start it with openclaw gateway start` | Nothing is listening on the port. Start the daemon, or set `"local": true`. |
| `openclaw exited 2 without answering: error: unknown option '--json'` | `args` does not match this build. Run `openclaw agent --help`. |
| `unrecognised event from OpenClaw: …` | The gateway answered in a shape the decoder does not know. Correct `client_envelope()`, or use the CLI route. |
| `OpenClaw said nothing for 420 seconds` | Given up on rather than left hanging. It may still be working; ask again or `/new`. |
| `OpenClaw's gateway dropped the connection` | The daemon restarted mid-answer. The next turn reconnects by itself. |
| It answers, but cannot touch the desktop | The MCP scope was never approved on the dashboard, or the gateway was not restarted after `.mcp.servers` was edited. |
