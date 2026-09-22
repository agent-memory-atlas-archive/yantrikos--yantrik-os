# Yantrik OS

An AI-native desktop operating system where the AI **is** the shell. Built in Rust, local-first
— your data stays on your machine, your model runs on your hardware.

Yantrik OS replaces the traditional desktop metaphor with an agent that watches the system,
learns your patterns and helps without being asked, alongside a full suite of built-in apps.

## What makes it different

- **The AI is the shell, not an add-on.** The companion is woven through file management, email
  triage, presentations, spreadsheet formulas and system monitoring rather than sitting in a
  chat window beside them.
- **Every screen is drivable by an agent.** The shell and every app publish their state and
  accept actions over a unix socket — `yos describe shell`, `yos act shell open_app name=notes`.
  Anything a person can do from the keyboard, an agent can do through the same path. See
  [docs/app-control.md](docs/app-control.md).
- **Local-first.** Runs on-device with quantized open models. No cloud dependency, works
  offline, and conversations and memories never leave the machine.
- **Proactive, not reactive.** A four-stage pipeline (Detect → Generate → Score → Deliver)
  decides *when* to speak and *what* to say, so it helps without nagging.
- **Rust throughout.** Slint UI, agent, memory database and system observer. No Electron, no
  Python runtime, no Docker.

## Built-in apps

Sixteen application binaries, each its own window, each drivable by the agent.

| App | What it is |
|-----|-----------|
| **Notes** | Markdown notes with semantic search, backlinks, versions |
| **Email** | IMAP client with AI triage and smart notifications |
| **Calendar** | Events, week and day views, reminders |
| **Weather** | Current conditions, hourly and daily forecast, alerts |
| **Terminal** | Terminal emulator with AI command assist and split panes |
| **Editor** | Text editor with tabs, find and replace, go-to-line |
| **yDoc** | Document editor — rich text, comments, change tracking |
| **ySheets** | Spreadsheet — formula engine, charts, multi-sheet |
| **yPresent** | Presentations — AI deck generation, templates, speaker notes |
| **Music** | Library, playlists, equalizer, folder watch |
| **Images** | Viewer with zoom, rotate, crop, slideshow, batch operations |
| **Downloads** | Resumable transfers with checksum verification |
| **Snippets** | Code snippets by language and collection |
| **Containers** | Docker/Podman containers, images and volumes |
| **Network** | WiFi, ethernet and bluetooth |
| **System Monitor** | CPU, memory, disk, network, processes |

The shell itself provides Files, Settings, Memories, Notifications, Bond, Personality,
Permissions, Devices, Packages, Skills and About as screens rather than separate windows.

Every one of them wears the same frame. See [docs/app-sdk.md](docs/app-sdk.md) for why that is
structural rather than a convention anyone has to remember.

## The companion

Not a chatbot. A proactive agent with:

- **Instincts** — email watch, open loops, routine learning, commitment tracking, security
- **Bond** — a relationship that moves from Stranger through Acquaintance, Companion and
  Confidant to Partner, based on the quality of the interaction
- **Memory** — persistent vector-indexed recall that grows over time
- **Model-adaptive behaviour** — detects what the model can do and adjusts tool use and prompt
  complexity to match
- **Pluggable minds** — the built-in companion is one harness among several. Anything that
  speaks the attach protocol can answer instead, managing its own endpoint and credentials.
  Five exist: Yantrik Mind, Hermes Agent, Pi, DeepSeek and OpenClaw — the last four ship as
  source in `harnesses/`, and none of them is started until you configure it. See
  [docs/harness.md](docs/harness.md).
- **YAML plugins** — add tools without writing Rust

## Architecture

```
┌────────────────────────────────────────────────────────────┐
│                        Yantrik OS                          │
│                                                            │
│   yantrik-ui ──────── yantrik-companion ──── yantrik-ml    │
│    (shell)              (agent)               (inference)  │
│        │                    │                              │
│   yantrik-os          yantrikdb                            │
│    (system)            (memory)                            │
│                                                            │
│   16 app binaries · 10 services · one control surface      │
│                                                            │
│   Debian 13 → labwc (Wayland) → Slint                      │
└────────────────────────────────────────────────────────────┘
```

23 crates, 16 apps and 10 services. The ones worth knowing:

| Crate | Purpose |
|-------|---------|
| `yantrik-ui` | The shell — Slint UI, app wiring, the control surface |
| `yantrik-companion` | The agent — tools, instincts, bond, personality, proactive pipeline |
| `yantrik-ml` | Inference — LLM backends (Ollama, OpenAI-compatible, llama.cpp, Claude CLI), embeddings, STT/TTS |
| `yantrik-os` | System integration — D-Bus, inotify, sysinfo, battery, network, processes |
| `yantrik-harness` | The attach protocol a third-party mind implements to answer for the shell |
| `yantrik-ui-kit` | The UI kit every app draws from, including the mandatory `AppHeader` |
| `yantrik-app-runtime` | What an app binary is built on — instance guard, theme, IPC, control surface |
| `yantrik-design-tokens` | Colour, type, spacing and size tokens, shared by the shell and every app |

**Threads:** the Slint event loop, a system observer (D-Bus, file watches, polling) and a
companion worker (inference, memory, tools).

## Quick start

### Install

Yantrik OS is built on **Debian 13 (trixie)**. The usual path is a cloud-init provisioned VM:

```bash
# Proxmox, libvirt, or anything that takes a cloud-init user-data file
deploy/yantrik-os/cloud-init/user-data.yaml
```

It fetches the release payload, installs the session and boots straight to the desktop.

### Hardware

| | Minimum | Recommended |
|--|---------|-------------|
| **CPU** | x86_64, 2 cores | 4+ cores |
| **RAM** | 4 GB | 8+ GB |
| **Disk** | 6 GB free | 24+ GB |
| **GPU** | Not required — the UI renders in software | Any, for interactive inference |
| **OS** | Debian 13 | Debian 13 |
| **Platform** | QEMU/KVM, Proxmox, VirtualBox | Bare metal |

Without a GPU the desktop is fully usable and inference is slow. That trade is deliberate: the
shell renders through Slint's software rasteriser and idles at around 2% of a core.

### LLM backends

| Backend | Setup | Notes |
|---------|-------|-------|
| **Ollama** | Point at a local or remote Ollama server | Any model it serves |
| **OpenAI-compatible** | Any endpoint speaking the API | Cloud latency |
| **Claude CLI** | Install the Claude Code CLI | Cloud latency |
| **llama.cpp** | Built in, GGUF on disk | Fully offline |

## Updating

```bash
yantrik-update check        # what is installed vs what the channel has
yantrik-update apply        # download, verify, install, restart the session
yantrik-update rollback     # restore the previous build
yantrik-update status       # current build and available backups
yantrik-update set-channel nightly|beta|stable   # which channel this machine follows
```

The bundle is verified against the manifest's sha256 before a file is touched, the current
binaries are backed up first, and if the new shell does not answer its control socket the
update rolls back on its own. Restarting is done through the session unit rather than by
respawning the shell, so it comes back exactly as it does on boot.

| Channel | What it is |
|---------|-----------|
| `stable` | Tested releases |
| `beta` | Promoted from nightly |
| `nightly` | Latest builds |

## Driving it from a terminal or an agent

```bash
yos describe shell                          # where you are, what is open, what is wrong
yos act shell open_app name=notes           # launch or focus an app
yos act shell show_screen screen=settings section=ai
yos describe notes                          # any running app answers for itself
yantrik ask "what is using the most disk?"  # ask the companion
```

Actions are graded safe, standard, sensitive or dangerous. `yos-mcp` exposes the same surface
over MCP.

**How often you are asked is a mode you set**, from the chip in the status bar beside the mind
chip, or in Settings → AI & Intelligence:

| mode | the mind may… |
|---|---|
| `plan` | read only — every change is refused and it has to tell you what it *would* do |
| `ask` | routine things run; sensitive ones put a card in front of you (the default) |
| `auto` | sensitive things run; you are still asked about destructive ones |
| `bypass` | nothing is asked. Time-boxed — 15 minutes, an hour, or until the shell restarts |

When a mode says to ask, a card says who is asking, what it will do and with which arguments, and
offers **Allow once**, **Deny**, and — for anything recoverable — **Allow for this session**,
which stops the asking for that one action until the shell restarts and is listed in the mode
menu with a ✕ beside it. Only those clicks grant anything: no action on any control surface can
grant, and none can make the desktop more permissive either. The one published action about modes,
`set_mind_mode`, can only tighten, so a mind can put itself into plan mode and can never take
itself out.

Nothing graded above the machine's own ceiling (`tool_permission`, on the AI page in Settings) is
ever run or even asked about, in any mode — bypass included. Bypass is never written to disk, so
a machine never boots into it. Everything that runs without you being asked is written down, in
the mode menu ("See what it did without asking") and in `~/.local/share/yantrik/mind-audit.jsonl`.

`YOS_MCP_MAX_PERMISSION` still exists as a cap a harness puts on itself. It can only ever be
*stricter* than the desktop's mode — it turns an unasked run into a card — and never looser.
Leave it unset and the desktop's mode is the whole policy.

Because a call may wait for a person, **an MCP client must allow `os_act` up to 270 seconds**.
A client that gives up sooner cuts the person off mid-decision. For Hermes:

```yaml
mcp_servers:
  yantrik_os:
    command: /opt/yantrik/bin/yos-mcp
    timeout: 300
```

## Configuration

One YAML file at `/opt/yantrik/config.yaml`:

```yaml
user_name: "Your Name"
companion_name: "Yantrik"

backend: "api"                      # api, claude-cli, or llamacpp
api_url: "http://localhost:11434"
api_model: "qwen3:8b"

features:
  resource_guardian:
    enabled: true
    battery_warning_threshold: 20
  email_watch:
    enabled: true
    check_interval_minutes: 5
```

Themes live at `~/.config/yantrik/theme-override.yaml`; the token list is in
[docs/CONTRIBUTING.md](docs/CONTRIBUTING.md).

## Development

### Prerequisites

- Linux, or Windows with WSL2 — the workspace builds on Debian/Ubuntu
- Rust 1.92+ (Slint 1.17 requires it)

### Build and test

```bash
cargo build --workspace
cargo test --workspace
```

The tests are worth running for their own sake: a good number of them exist to stop a specific
mistake returning — that every shipped app draws its header with the shared component, that the
screen a caller asks for is the screen the shell draws, that an application answers to one name
on every surface, that the release script packages the directory it built into.

### Release

```bash
deploy/yantrik-os/build-release.sh --publish nightly
```

Discovers what the OS is made of rather than reading a list, packages it, uploads it, verifies
what is actually being served matches what was built, and prunes the channel.

### Looking at it

```bash
scripts/screen-survey.sh                 # photograph every screen, section and app
scripts/screen-survey.sh --only apps
```

Design review needs the actual pixels. Nearly every defect worth finding in this project was
found by looking at a running machine, and almost none of them were visible in the source.

### Project structure

```
yantrik-os/
├── crates/                    23 crates
│   ├── yantrik-ui/            the shell
│   │   ├── src/wire/          one module per screen, wiring UI to system
│   │   ├── src/features/      proactive features
│   │   └── src/control*.rs    the agent-facing control surface
│   ├── yantrik-ui-slint/ui/   the shell's Slint markup, one file per screen
│   ├── yantrik-ui-kit/slint/  the shared components every app draws from
│   ├── yantrik-companion/     the agent
│   ├── yantrik-ml/            inference
│   ├── yantrik-os/            system observer
│   └── yantrik-harness/       the pluggable-mind protocol
├── apps/                      16 application binaries
│   └── desktop-files/         their freedesktop entries
├── harnesses/                 minds that attach: hermes, pi, deepseek, openclaw, and the half they share
├── services/                  10 background services
├── config/labwc/              compositor config, theme and autostart
├── deploy/yantrik-os/         cloud-init, session, release and update scripts
├── scripts/                   probes and the screen survey
└── docs/
    ├── architecture.md        system design
    ├── app-sdk.md             how to write an app, and why the frame is not yours
    ├── app-control.md         how apps publish state and accept actions
    ├── harness.md             attaching a different mind
    ├── footprint.md           what it costs to run, and where that goes
    └── CONTRIBUTING.md        contributor guide
```

## Privacy and security

- **Local-first.** Inference runs on your machine. No telemetry, no phone-home, no cloud calls
  unless you choose a cloud backend.
- **The memory is yours.** It lives at `/opt/yantrik/data/` as plain SQLite you can read,
  export or delete.
- **Graded permissions.** Tools are safe, standard, sensitive or dangerous; the last two need
  explicit approval.
- **Path sandboxing.** File tools refuse `.ssh`, `.gnupg` and similar.
- **Applications are not sandboxed.** Software installed through the package manager runs with
  your own access, as on any ordinary Debian desktop. Said plainly here rather than left to be
  discovered.

## License

GPL-3.0. See [LICENSE](LICENSE).

## Links

- **Releases**: https://releases.yantrikos.com
- **Issues**: https://github.com/yantrikos/yantrik-os/issues
