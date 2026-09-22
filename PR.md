# The documented way to install Yantrik OS installed a different operating system

Branch: `feature/install-truth` · off `origin/main` at `d595bbf`

## What a person hit

`docs/getting-started.md` told a newcomer to install **Alpine Linux 3.18+** on their machine
and then run:

```bash
curl -fsSL https://get.yantrikos.com/install.sh | sh
```

That URL is live and serves the March installer — `install.sh` at the repository root, 38 KB
— which installs a set of binaries onto an Alpine system, downloads them from a third domain
`get.yantrik.dev` (**does not resolve**, verified), and sets up `yantrik-upgrade`, a program
that no longer exists. Yantrik OS has been a Debian 13 live ISO since spring. Following the
documentation got you a different operating system with an unrelated set of files on it.

The same page sent people to `https://releases.yantrikos.com/stable/install.sh` (**404**) and
implied a stable channel (`https://iso.yantrikos.com/stable/latest.json` — **404**; only
`nightly` has ever been published). `hardware-requirements.md` gave Alpine as the OS row and a
tokens-per-second table with no statement of what hardware produced any of it.
`architecture.md`, `apps.md`, `CONTRIBUTING.md` and `companion.md` still said Alpine, `apk`,
OpenRC and `/bin/ash`. `README.md` promised "No cloud dependency", "conversations and memories
never leave the machine" and "no telemetry, no phone-home, no cloud calls", counted "Sixteen
application binaries", and listed ySheets and Music as shipped features — all three retracted
publicly in September and never corrected here.

Found by the Discord steward.

## What this changes

### `install.sh` — replaced

The 1046-line Alpine installer is gone from the root. In its place is a 200-line POSIX `sh`
script that **installs nothing**.

Piped into a shell with no flag it fetches `nightly/latest.json`, prints the file, version,
size, sha256 and URL, says that nightly is the only published channel and that the audits are
in `design/`, and stops. It writes no file, asks for no privilege and touches nothing.

With `--download` it fetches the ISO into the directory you ran it from — to a `.part` name,
renamed only once all the bytes are there, so an interrupted download is never the thing that
gets written to a USB stick — verifies its sha256 against `latest.json`, and then prints how
to write it to a stick (`dd` with the device-not-partition warning and how to find the device
on each platform, Rufus, balenaEtcher) and that `yantrik-install` is inside the live session.

It refuses to download at all if neither `sha256sum` nor `shasum` is present, rather than
fetching 1.3 GiB it cannot check. It takes no `--channel`: there is no stable channel to point
it at, and offering the flag would only produce a confident error.

The old script is kept at **`deploy/yantrik-os/legacy/install-alpine-2026-03.sh`** with a
header saying what it was, that it is not used, and that nothing references it. **My call was
to keep rather than delete it**, for one reason: `get.yantrikos.com` still serves it, so until
that server is redeployed the repository should contain the thing strangers are being handed.
It stays mode 755 only because CI requires every tracked file starting with `#!` to be
executable.

### `docs/getting-started.md` — rewritten

Around the real path: download and verify from `iso.yantrikos.com/nightly/`; try it in a VM
(with the settings that are known to work, and the CI boot test's smaller ones); write it to a
USB stick; the four GRUB entries and what each does; the two facts about the live session
people need before putting it on a network (`yantrik`/`yantrik` with passwordless sudo; SSH
installed and disabled); what first-run setup asks; **attaching a mind**, which is where
`docs/harness.md` is pointed at and where the "no model ships" fact lives; the mind modes; the
disk installer, both ways in; `yantrik-update` with the commands and exits it really has; and
`yos`. Keyboard shortcuts now match `config/labwc/rc.xml` — the terminal is `Ctrl`+`Alt`+`T`,
not `Win`+`T` as the old page said.

One thing found by checking a command rather than writing it down: the installer has to be
invoked as **`sudo /opt/yantrik/bin/yantrik-install`**, not `sudo yantrik-install`.
`yantrik-session` puts `/opt/yantrik/bin` on the user's `PATH` — verified in the running
shell's `/proc/<pid>/environ` on the test machine — but Debian's `sudo` replaces `PATH` with
`secure_path`, which does not include it. Checked on the VM: `command -v yantrik-install`
resolves, `sudo -n sh -c 'command -v yantrik-install'` does not. Both the doc and the script
now print the full path and say why.

Honest at the top about what it is: nightly-only, early, things break, and the audits are in
`design/`.

### `docs/hardware-requirements.md` — rewritten

Debian 13, not Alpine. Every number is labelled **measured**, **configured** or **not
measured**, and where nothing has been measured it says so instead of printing a figure:

- Image size **1,411,915,776 bytes / 1.31 GiB** — measured, from `latest.json`.
- CI boot test **4096 MB, 4 CPUs, virtio-vga, BIOS, no disk** — configured, from
  `boottest.py`. This is the one configuration every published image is known to boot in.
- The test machine **4 cores, 7.8 GiB, virtio-gpu, 32 GB disk, QEMU/KVM Q35 + SeaBIOS, kernel
  6.12.107+deb13-amd64** — measured over SSH, read-only, 2026-09-22.
- `/opt/yantrik` **measured** per directory: `bin` 757 MB, `models` 354 MB (whisper 147,
  tts 120, embedder 88; `llm/` empty), `data` 64 MB, `logs` 9.3 MB, `share` 616 KB — about
  **1.2 GB**. Stated with the caveat that this machine's `bin/` holds ~112 MB of hand-deploy
  duplicates, and that its `backups/` (2.0 GB) and `ui-deployments/` (6.5 GB) are development
  debris, not an install. **A fresh installed footprint is not measured, and the page says so.**
- **Not measured**, said outright: real hardware, the UEFI path, any GPU but QEMU's, Wi-Fi on
  real adapters, VirtualBox against a current build.
- The tokens-per-second table is gone. There is no measurement in this repository behind it.

### `docs/architecture.md`, `apps.md`, `CONTRIBUTING.md`, `companion.md`

- `architecture.md`: the stack line is Debian 13, with a sentence saying what it used to say
  and that `apk`/OpenRC/`/bin/ash` are not on the machine.
- `apps.md`: `/bin/ash` → `$SHELL` falling back to `/bin/bash`; "Alpine's `apk`" → `apt-get`
  through `sudo` (checked against `wire/apt.rs` and `companion-tools/src/package.rs`, which
  detect the manager and use apt here). **ySheets and Music are marked SHELVED** with the
  reason, what brings each back, and a pointer to `design/shelved-2026-09-20.md` — including
  the separation the shelved record insists on, that audio *does* play through the shell's own
  media screen and never went through the Music app.
- `CONTRIBUTING.md`: "Deploy Alpine VM / `setup-alpine-vm.sh`" replaced with booting the
  published image, and a named list of the Alpine-era deploy scripts with "do not start from
  one".
- `companion.md`: the `package` and `service` tool descriptions said apk and OpenRC; they are
  apt and systemd on this machine (both tools detect at runtime and land there).

### `README.md` — three retractions

1. **The privacy claims.** "Local-first … No cloud dependency, works offline, and conversations
   and memories never leave the machine" and "No telemetry, no phone-home, no cloud calls" are
   replaced with what is true: **the OS itself sends nothing anywhere** — no telemetry, no
   phone-home, no call it makes on its own; the image ships pointed at loopback and nothing
   else. But this OS is built to be driven by any mind, cloud models included — that is the
   point of the harness protocol, and the token-efficiency comparison this project publishes
   was itself measured against a cloud model. Point it at Ollama or llama.cpp and nothing
   leaves the machine; point it at a provider and what you type goes to that provider. And the
   desktop says which: the status bar chip names the answering mind and whether it is local or
   cloud (`components/status_bar.slint` — the harness chip and the privacy/provider chip).
   The backend table now has a "where what you type goes" column instead of "notes".
2. **The count.** "Sixteen application binaries" → **fifteen ship**, seventeen are in the tree,
   two are shelved. Arcade was missing from the old table; it is there now. The ASCII diagram
   (`16 app binaries`), the prose (`23 crates, 16 apps`) and the tree (`apps/ 16 application
   binaries`) all corrected.
3. **ySheets and Music.** Out of the shipped table, into a shelved table with the reason and
   the condition for coming back.

Also: the install section now leads with the ISO rather than cloud-init; the channel table says
nightly is the only one with builds; the Links section adds the image index and Discord.

## How this was verified

- **Every URL printed in every changed file returns 200**, checked with `curl`:
  `iso.yantrikos.com/nightly/` and `/latest.json`, `releases.yantrikos.com`,
  `get.yantrikos.com/install.sh`, `rufus.ie`, `etcher.balena.io`, the GitHub blob and issue
  links, `discord.gg/7cDw3jd3Xf`. The only non-200 URLs that appear are the two the text names
  *as* 404s (`iso.yantrikos.com/stable/latest.json`, `releases.yantrikos.com/stable/install.sh`)
  and `get.yantrik.dev`, which does not resolve — all three cited as facts, none offered as a
  link.
- **The checksum flow was run end to end in WSL against the real nightly.**
  `sh install.sh --download` fetched `yantrik-os-v0.1.0-304-g7bc7d6b.iso`, all 1,411,915,776
  bytes of it, and printed `sha256 ok 9c89169ee3222e72a33e15fedb737b3b86bceb7aac1ba742af78bd509fa8bdb7`
  — matching `latest.json`. Exit 0. The ISO was deleted afterwards.
- **The failure path was exercised too**: a deliberately corrupt file of the right name is
  detected, both hashes printed, "do not boot it", exit 1. Also checked: no-flag run leaves the
  directory empty, `--help` works when piped into `sh` (it is a heredoc, not a `sed` over
  `$0`, which would print nothing in exactly the situation the script is most often run in),
  and an unknown flag is an error.
- **`bash -n` and `sh -n` clean** on the new `install.sh`, and `bash -n` across every script in
  the tree that CI parses — 0 failures. **shellcheck is not installed** on this machine, so it
  was not run.
- **CI's script checks pass**: every tracked file starting with `#!` is executable in git
  (`install.sh` needed `git update-index --chmod=+x`; the legacy file kept 755 through the
  rename).
- **The CI selftests are green**: `yos-selftest.py`, `yos-mcp-selftest.py`,
  `server/publish_selftest.py`, `yantrik-update selftest`, and the cloud-init YAML parse.
- **The harness suite** (`python3 -m unittest discover -s harnesses/tests`) — **176 tests, OK
  in 41s**. It reported one failure on an earlier run —
  `test_openclaw.CliRouteTests.test_stop_ends_the_child_and_closes_the_turn_once` — taking
  331s while the 1.3 GiB ISO download ran alongside it. Unloaded it passes, as do all 61
  openclaw tests in isolation. It is a process-termination timing test that goes flaky under
  contention; nothing in this branch touches `harnesses/`. Worth knowing if it ever goes red
  on a busy runner.
- **No Rust was touched**, and nothing in `crates/`, `apps/` or `tests/` references the root
  `install.sh` or any of these documents, so nothing needs rebuilding.
- Facts read off the running VM at `192.168.4.44`, **read-only** (`du`, `free`, `nproc`,
  `lspci`, `cat`): nothing installed, nothing restarted, nothing configured.

## What I could not verify

- **shellcheck** — not installed here.
- **`get.yantrikos.com` still serves the old Alpine script.** The docs describe it serving the
  new one, which is true after you deploy it. I did not touch that server, as instructed.
- **The disk installer.** Not run. The docs say to treat it as the least-proven part and to use
  a machine you can afford to wipe, which is what the ISO's own README.txt already says.
- **`https://iso.yantrikos.com/nightly/README.txt` is stale on the server** — it describes
  build `v0.1.0-226` from 21 September, says "14 first-party apps", and is dated a day before
  the current image. `latest.json` is current. Not something a PR to this repository can fix;
  worth a look at what writes it (`deploy/yantrik-os/server/yantrik-publish`).

## Next thing somebody should look at

The deploy scripts that still target Alpine. They are tooling, not documentation, so this PR
leaves them alone — but they are the remaining Alpine surface in the tree and one of them is
still named in a way that invites use:

| Script | Alpine references |
|---|---|
| `deploy/yantrik-os/build-vbox-image.sh` | 25 |
| `deploy/yantrik-os/setup-vbox.sh` | 18 |
| `deploy/yantrik-os/deploy-stack.sh` | 16 |
| `deploy/yantrik-os/build-iso.sh` | 16 |
| `deploy/yantrik-os/setup-alpine-vm.sh` | 15 |
| `deploy/yantrik-os/deploy-vbox.sh` | 8 |
| `deploy/yantrik-os/boot-desktop.sh` | 2 |
| `deploy/yantrik-os/setup-wsl2.sh` | 1 |

`build-iso.sh` is the one to look at first — the name says it builds this project's ISO and it
does not; `build-debian-iso.sh` is what CI runs.

Two smaller ones found while reading and deliberately not changed here:

- `crates/yantrik-ui/src/terminal.rs:77` still prefers `/bin/ash` over `/bin/bash` when
  `$SHELL` is unset. The app binary (`apps/terminal/src/session.rs`) has it right. Dead on
  Debian, but it is the shell-embedded terminal's fallback and it names a binary that does not
  exist on this OS.
- `README.md` says "23 crates" while `crates/` holds 25 directories. 23 is the workspace-member
  count from that directory; `yantrik-harness` and `yantrik-mcp` are the other two. Left alone
  as out of scope for this PR.

---

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>

https://claude.ai/code/session_012NJMVuSihrV9NvSwpz5mei
