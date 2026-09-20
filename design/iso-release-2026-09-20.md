# The ISO, and what it would take to publish it

2026-09-20. An audit of `deploy/yantrik-os/build-debian-iso.sh` against what Yantrik OS
actually is today, the fixes that audit produced, a build, and a boot test.

Written to be read by someone deciding whether to put a link on a website. The short answer
is **not yet**, and the reasons are listed rather than summarised.

---

## 1. What the brief got wrong

Worth saying first, because two of these would have sent the work in the wrong direction.

- **"It shipped only TWO binaries."** Not since 2026-09-17. `build-debian-iso.sh` step 4
  already calls `build-release.sh --no-build` and installs whatever that discovers, then
  asserts `>= 20` binaries landed and that `yantrik-ui`, `yantrik`, `yantrik-notes`,
  `weather-service` and `yos` are among them. The "one list of binaries" the brief asked for
  already existed for the ISO. What did *not* exist was that list reaching the four dev
  scripts — that part was real, and is fixed here.
- **"`MIND_TARBALL` or `YANTRIK_ISO_WITHOUT_MIND=1`."** Correct, and still true. The mind is
  a separate repository's artifact. This build was made with `YANTRIK_ISO_WITHOUT_MIND=1`.
- **"The build environment is only WSL."** Correct, and there is a second reason the brief
  did not name: the Windows working copy has CRLF line endings, and `bash` cannot parse a
  CRLF shell script whose function bodies open at end of line. `build-debian-iso.sh` could
  not be executed from `C:/Users/sync/codes/yantrik-os` at all. See §5.
- **"`design/shelved-2026-09-20.md`, `tests/app-lints/shelved.toml`, the `SHELVED` table."**
  All three exist and agree. `SHELVED` in `crates/yantrik-ui/src/wire/dock.rs` is the one
  that names *binaries*, so it is the one the packaging scripts now read.

---

## 2. What the ISO contains

### Base

Debian 13 (trixie), `amd64`, `debootstrap --variant=minbase`, `main contrib non-free
non-free-firmware`. A live image: SquashFS root (xz, `-Xbcj x86`), `live-boot` /
`live-config`, hybrid BIOS + UEFI via `grub-mkrescue`.

### The OS

Installed to `/opt/yantrik/bin`, discovered by `build-release.sh` from the cargo release
directory and filtered through `deploy/yantrik-os/shelved-bins.sh`:

- the shell `yantrik-ui`, and the `yantrik` CLI
- 14 apps — Notes, Email, Calendar, Weather, System Monitor, Terminal, Text Editor, Image
  Viewer, Document Editor, Presentation, Network Manager, Container Manager, Download
  Manager, Snippet Manager
- 9 services the shell's `ServiceManager` registers in `crates/yantrik-ui/src/main.rs`:
  `weather-service`, `system-monitor-service`, `notes-service`, `notifications-service`,
  `calendar-service`, `network-service`, `email-service`, `a11y-service`,
  `perception-service`; plus `perception-journal` and `transcribe`
- the agent surface `yos` and `yos-mcp`, the updater `yantrik-update`, the session
  `yantrik-session`, the disk installer as `yantrik-install`
- `/opt/yantrik/share`: labwc `rc.xml`, `themerc`, ten titlebar button PNGs, the autostart
  file, five fonts (Barlow ×3, JetBrains Mono ×2), 14 `.desktop` entries
- `/opt/yantrik/BUILD` (version, git rev, build time, binary count)
- `/opt/yantrik/THIRD-PARTY-NOTICES.md` and `/opt/yantrik/LICENSE` (GPL-3.0) — see §8
- `/opt/yantrik/share/harnesses/hermes` — the Hermes desktop plugin, as source

The 27 compiled binaries, as discovery actually produced them for this build:

```
a11y-service              calendar-service          email-service
network-service           notes-service             notifications-service
perception-journal        perception-service        system-monitor-service
transcribe                weather-service           yantrik
yantrik-calendar          yantrik-container-manager yantrik-document-editor
yantrik-download-manager  yantrik-email             yantrik-image-viewer
yantrik-network-manager   yantrik-notes             yantrik-presentation
yantrik-snippet-manager   yantrik-system-monitor    yantrik-terminal
yantrik-text-editor       yantrik-ui                yantrik-weather
```

plus the four scripts `yos`, `yos-mcp`, `yantrik-update`, `yantrik-session`, and
`yantrik-install` — **32 files in `/opt/yantrik/bin`**.

**Not shipped:** `yantrik-music-player` and `yantrik-spreadsheet` (shelved) and their
`.desktop` entries; `test-brain` (a test binary); `.cargo-lock` (cargo's own lock file, which
was mode 755 in the release directory and had been packaged as a binary in every tarball
until this change).

### Models

Always: MiniLM embedder (~90 MB), Whisper tiny (~150 MB), Piper TTS binary + the
`en_US-lessac-medium` voice (~65 MB). All fetched from Hugging Face and GitHub **at build
time**, so the ISO build needs a network even though the resulting image does not.

Only with `--with-llm`: a GGUF and `llama-server` on port 8341. **This build has no LLM.**

### Desktop

`getty@tty1` autologin as `yantrik` → `~/.bash_profile` → `/opt/yantrik/bin/yantrik-session`
→ `labwc -s '/opt/yantrik/bin/yantrik-ui /opt/yantrik/config.yaml'`. No display manager.
`yantrik-session` installs the shipped compositor config, theme and fonts into the user's
home at every login and puts `/opt/yantrik/share` on `XDG_DATA_DIRS`.

### First boot

`crates/yantrik-ui/src/onboarding.rs` gates on `~/.yantrik/.onboarding_complete`, absent on
a fresh image. `wire/ai_onboarding.rs` asks the person to choose a model provider (ollama,
llamacpp, lmstudio, vllm, or a cloud API), tests it, and reports whether the endpoint is
local or remote before saving. The image itself holds **no endpoint and no key** — the
shipped `config.yaml` points at `127.0.0.1:8341`.

The GRUB "Install" entries pass `yantrik.install=true`, which makes `.bash_profile` touch
`/opt/yantrik/.installer-mode`; `wire/installer.rs` reads that and puts disk-install fields
into the onboarding flow. `yantrik-install` is the text fallback, reachable from a shell.

---

## 3. Secrets, PII and private addresses

Every hit, with `file:line`. "Ships" means the bytes reach a published image.

### Ships — fixed in this change

| Where | What | Status |
|---|---|---|
| `deploy/yantrik-os/yantrik-update:82` | `http://192.168.4.28/manifest.json` as a hardcoded fallback for the manifest read | **fixed** |
| `deploy/yantrik-os/yantrik-update:209` | `http://192.168.4.28/$CHANNEL/$artifact` as a hardcoded fallback for the bundle download | **fixed** |
| `deploy/yantrik-os/build-debian-iso.sh` step 6 | `server: "http://releases.yantrikos.com"` — public host, but plaintext | **fixed** (https) |
| `deploy/yantrik-os/build-debian-iso.sh` step 10 | `nameserver 8.8.8.8` written into every installed machine's `/etc/resolv.conf` | **fixed** |
| `deploy/yantrik-os/build-debian-iso.sh` (LLM step) | `/mnt/c/Users/sync/.ollama/models/blobs/sha256-485cf5f0…` — a developer's Windows home directory and one Ollama blob hash | **fixed** (`LOCAL_GGUF`) |

The updater one is the serious one and deserves its own paragraph.

> `yantrik-update` fell back to `http://192.168.4.28` for **both** the manifest and the
> tarball. The sha256 that "verifies" the download is read out of that same manifest, from
> that same host, over that same plaintext connection — so it verifies nothing. On the
> author's LAN that address is the release server. On a stranger's LAN it is whatever is
> sitting at `192.168.4.28` on the network they joined. Anyone who could answer there, or
> sit on the path, could have a tarball of their choosing unpacked over `/opt/yantrik/bin`
> on any machine running this OS. That is remote code execution on every published install,
> and it would have shipped.

### Ships — NOT fixed, blocks publication

**The `yantrik-email` binary carries a demo mailbox with real-looking personal addresses.**
Found by running `strings` over every binary in the built image. `apps/email/src/main.rs`
holds a design fixture — gated at runtime behind `YANTRIK_EMAIL_DEMO` (line 1558), so it is
never *displayed* on a fresh machine — but the strings are compiled in regardless and ship in
the published ISO:

| `apps/email/src/main.rs` | Content |
|---|---|
| `:1563`, `:2328`, `:2357`, `:2359`, `:2363` | `pranab@yantrik.dev`, sender name `Pranab` |
| `:2306` | `Priya Raman`, `priya@lumen.dev` |
| `:2308` | `Ananya Sen`, `ananya@lumen.dev` |
| `:2312` | `Ravi Kulkarni`, `ravi@lumen.dev` |
| `:2310` | `Marcus Webb`, `marcus@webb.io` |
| `:2309` | `billing@hetzner.com`, an invoice number and `€14.28` for a named VPS |
| `:2311` | `hello@slint.dev` |

Anyone who downloads the ISO can read all of that with one command. Whether those people are
real is not something this audit can establish, and that is the point: they read as real, and
four of them share a domain. `apps/` is outside this change's ownership, so it is reported
rather than edited. The fix is to move the fixture to a file the demo loads at runtime, or to
replace the names with obviously-fictional ones.

| Where | What |
|---|---|
| `config/yantrik-ollama.yaml:8` | `user_name: "Pranab"` |
| `config/yantrik-ollama.yaml:14` | system prompt: "…on Pranab's computer. You remember everything he tells you." |
| `config/yantrik-ollama.yaml:23` | `api_base_url: "http://192.168.4.35:11434/v1"` — a private LAN address |

`build-release.sh` copies that file into the release tarball as `config.yaml`. The **ISO** is
clean, because step 6 overwrites it with the new `deploy/yantrik-os/config-default.yaml` —
but the **tarball** is the artifact `cloud-init` and `yantrik-update` consume, and it carries
all three. `build-release.sh` now prints a warning naming the hits rather than failing,
because the nightly channel feeds the author's own VMs and breaking that to fix a publishing
problem helps nobody. `config/` is outside this change's ownership; the fix is a decision
about what the dev config is for, not a line edit.

### Does not ship — tooling only, but worth knowing

| Where | What |
|---|---|
| `deploy/yantrik-os/build-release.sh:271` | `RELEASES_IP="${RELEASES_IP:-192.168.4.28}"` — the `--publish` path |
| `scripts/publish-components.sh:17,24` | `REGISTRY_HOST="192.168.4.28"`, `PROXMOX_HOST="192.168.4.152"` |
| `deploy/yantrik-os/proxmox-deploy.sh:32` | `192.168.4.151` |
| `deploy/yantrik-os/deploy-stack.sh:227`, `install.sh:435` | `MODEL_CACHE="http://192.168.4.92:8888"` |
| `deploy/yantrik-os/deploy-to-vm.sh:23` | `192.168.4.66` in a usage comment |
| `deploy/yantrik-os/cloud-init/user-data.yaml:135,253` | `YANTRIK_USER_NAME="Pranab"` |
| `deploy/yantrik-os/deploy-stack.sh:297` | `user_name: "Pranab"` |
| `config/yantrik-companion.yaml:8,14`, `config/yantrik-os.yaml:7,13` | `user_name: "Pranab"` and the same system prompt |
| `install.sh:37` | `GITHUB_REPO="spranab/yantrik-os"` — the author's GitHub handle (intentional) |

**No API keys, bearer tokens, private keys or e-mail addresses were found anywhere in
`deploy/`, `config/`, or the four dev scripts.** `deploy/yantrik-os/yos` and `yos-mcp`
contain one URL between them, `http://127.0.0.1:9222`. Nothing in this work read
`keys.env` or any `.env`.

---

## 4. Default credentials and remote access

| Thing | Before | After |
|---|---|---|
| `yantrik` user password | `yantrik` | `yantrik` (unchanged — see below) |
| `root` | locked | locked |
| sudo for `yantrik` | `NOPASSWD: ALL` | unchanged |
| sshd | **enabled**, `PasswordAuthentication yes` forced | **installed, not enabled**; `YANTRIK_ISO_SSH=1` to turn on, and it warns |
| `/opt/yantrik/logs` on an installed machine | `chmod 777` | `0755`, owned by the user |

The password is still `yantrik` on the live image. That is defensible for a live session that
autologins anyway and has passwordless sudo; it is *not* defensible with sshd listening,
which is why sshd is now off. `yantrik-install` asks for a real password for the installed
machine and re-prompts rather than aborting on a mismatch.

**Still open:** a live image whose sudo is passwordless and whose user password is a
published constant is one where physical access is root. Every live ISO has this property;
it should be said out loud in the release notes rather than left implied.

---

## 5. Line endings

The Windows working copy has CRLF in every shell script, despite `.gitattributes` saying
`* text=auto eol=lf`. The attributes were added after those blobs were committed, and
checkout converts LF to the platform ending — it does not strip CRs already in a blob.

Consequences found:

- `build-debian-iso.sh`, `build-release.sh`, `deploy.sh`, `install.sh`,
  `scripts/package-all.sh` and `scripts/publish-components.sh` were all CRLF.
  `bash -n` fails on three of them. **The ISO could not be built from this checkout.**
- `install.sh` additionally contained `for df in /opt/yantrik/desktop-files/*.desktop
  2>/dev/null; do` — a redirection inside a `for` list, which is a syntax error. That file
  has never parsed, so the `curl | sh` installer it advertises has never run.
- The files that actually *ship* — `yos`, `yos-mcp`, `yantrik-update`, `yantrik-session`,
  `yantrik-install.sh`, `config/labwc/*` — are LF, because `.gitattributes` names them
  explicitly. So the image itself was not affected. `build-release.sh` now checks anyway and
  strips CRs from any shebang file it stages, naming what it fixed.

The ISO was built from a `git worktree` at `HEAD`, which git checks out with LF.

---

## 6. What changed, and where

New files, all under ownership:

- `deploy/yantrik-os/shelved-bins.sh` — prints the shelved binary names, read out of the
  `SHELVED` table in `crates/yantrik-ui/src/wire/dock.rs`. Fails loudly rather than
  returning an empty list. This is the "one source" the brief asked for.
- `deploy/yantrik-os/config-default.yaml` — the config a published image ships. Loopback
  model endpoint, `server.host` `127.0.0.1` (was `0.0.0.0`), no name, no key, https update
  channel. Replaces a 130-line heredoc inside the build script.
- `deploy/yantrik-os/THIRD-PARTY-NOTICES.md` — what the image redistributes. Incomplete
  **on purpose**: entries whose licence was not read from the upstream artifact are marked
  `NEEDS VERIFICATION` rather than guessed.

`deploy/yantrik-os/build-debian-iso.sh`:

- `TARGET_DIR` asked of `cargo metadata` instead of `/home/yantrik/target-yantrik`
- version from `git describe` instead of the literal `0.3.0`
- hands `build-release.sh` the same target directory it checked
- step 6 installs `config-default.yaml` and refuses if it contains a private address or an
  e-mail address; also writes `/opt/yantrik/update.conf` from the same channel, so the
  desktop and the updater stop naming different channels
- installs `THIRD-PARTY-NOTICES.md` (required) and the repo `LICENSE` (warns if absent)
- stages the Hermes plugin
- adds `swaybg`, `xdg-utils`, `espeak-ng` — three programs the shell shells out to by name
  and did not have, each failing silently
- sshd off by default; `resolv.conf` left to NetworkManager; `LOCAL_GGUF` instead of a
  hardcoded Windows path
- GRUB: serial in and out, so a headless boot test can see the menu; a "Try live" entry that
  does not force installer mode; a verbose serial entry with no `quiet`
- `grub-mkrescue` stderr no longer discarded; writes `.sha256` and a `.manifest` beside the ISO
- `yantrik-install` is normalised to LF and `bash -n`-checked inside the chroot

`deploy/yantrik-os/build-release.sh`:

- `SHELVED_BINS` from `shelved-bins.sh`
- excludes dotfiles from binary discovery — **`.cargo-lock` is mode 755 in the release
  directory, so it was being staged into `bin/` and counted as a binary in every tarball
  built so far**
- warns, with line numbers, when the `config.yaml` it is about to ship carries a private
  address, an e-mail address or a key
- strips CRs from shipped shebang files and says which

`deploy/yantrik-os/yantrik-update`: the `192.168.4.28` fallbacks removed; `SCHEME` (https by
default) alongside `HOST` and `CHANNEL` in `update.conf`.

`deploy/yantrik-os/yantrik-install.sh`: runs `yantrik-session` instead of bare `labwc` and
stops hand-writing an autostart and `rc.xml` — **an installed machine was the only kind of
Yantrik machine not running the shipped session, so its launcher listed Chromium and Vim and
none of this OS's own apps**; `VERSION_ID` in `/etc/os-release` from `/opt/yantrik/BUILD`
instead of the literal `0.3.0`; `/opt/yantrik/logs` `0755` and owned, not `777`.

`deploy.sh`, `scripts/package-all.sh`, `scripts/publish-components.sh`: shelved binaries
filtered from every build and install list, from `shelved-bins.sh`. `package-all.sh` now
discovers binaries instead of naming them — its hardcoded list was stale in both directions
at once, missing `a11y-service` and `perception-service` while still naming both shelved apps.
`install.sh` cannot read the repository (it is fetched by `curl`), so it keeps a copy, now
commented as one, and its syntax error is fixed.

All nine files normalised to LF.

---

## 7. Which deploy scripts are alive

Not deleted, as instructed. Listed so nobody has to guess again.

**Alive** — on the ISO or release path: `build-debian-iso.sh`, `build-release.sh`,
`shelved-bins.sh`, `yantrik-install.sh`, `yantrik-session`, `yantrik-update`, `yos`,
`yos-mcp`, `cloud-init/user-data.yaml`, `deploy-to-vm.sh` (the rsync dev loop).

**Fossil** — Alpine-era or VirtualBox-era, content untouched since Feb–Mar 2026, targeting a
base OS this project left:

| Script | Last content change | Targets |
|---|---|---|
| `build-iso.sh` | 2026-02-28 | Alpine |
| `build-vbox-image.sh` | 2026-09-05 | Alpine + VirtualBox |
| `setup-alpine-vm.sh` | 2026-02-27 | Alpine |
| `setup-vbox.sh`, `deploy-vbox.sh` | 2026-03-01 | VirtualBox |
| `deploy-stack.sh` | 2026-03-04 | Alpine, LAN model cache |
| `quick-deploy.sh` | 2026-02-28 | root password `root` |
| `setup-wsl2.sh` | 2026-02-27 | pmbootstrap / postmarketOS |
| `boot-desktop.sh`, `yantrik-start.sh`, `bashrc-hook.sh`, `debug-*.sh`, `vm-ssh.sh` | 2026-02/03 | dev helpers |

`build-image.sh` (2026-09-15, Debian, `virt-builder`) and `proxmox-deploy.sh` (2026-09-15)
are recent but are neither the ISO nor the tarball path — a third and fourth way to make a
machine. Worth a decision, not a deletion.

---

## 8. Blockers for a public release

In the order they would stop a release.

1. **`yantrik-email` ships six personal-looking e-mail addresses and an invoice.** See §3.
   Readable from the published ISO with `strings`. `apps/email/src/main.rs`.
2. **`config/yantrik-ollama.yaml` ships in the release tarball with the author's name and a
   private LAN address.** The ISO is clean; the tarball is not, and the tarball is what
   `cloud-init` and `yantrik-update` install. Needs a public default config for the tarball
   too, or `build-release.sh` pointed at `config-default.yaml`.
3. **No GPL source offer.** The repository *is* licensed — `LICENSE` is GPL-3.0 and `README.md`
   §License agrees — and the ISO now installs it to `/opt/yantrik/LICENSE`. What is missing is
   the consequence: GPL-3 §6 says whoever receives these binaries can demand the corresponding
   source for the exact version they got. A download page needs the source beside the ISO, or a
   written offer pinned to the git revision in `/opt/yantrik/BUILD`. Same obligation applies
   separately to the Debian base. (The workspace `Cargo.toml` also declares no `license` field,
   so tooling reports the licence as unknown.)
4. **`THIRD-PARTY-NOTICES.md` is incomplete.** Four entries are `NEEDS VERIFICATION`: the
   `en_US-lessac-medium` voice licence, espeak-ng's GPL data inside the MIT Piper release,
   the `--with-llm` model's terms, and the missing `OFL.txt` beside the shipped fonts.
   There is also no written GPL source offer for the Debian base.
5. **No Rust dependency licence manifest.** `cargo about` or `cargo deny` over `Cargo.lock`;
   not wired in.
6. **The update channel has never been verified end to end from a public network.**
   `releases.yantrikos.com` now has to exist, serve `manifest.json` and the channel
   directories over **https**, and present a valid certificate — because the old private-IP
   fallback that used to paper over a failure is gone, by design.
7. **Live user password is a published constant with passwordless sudo.** Say so in the
   release notes.
8. **Models are fetched at build time, not vendored.** A build is not reproducible: Hugging
   Face and GitHub decide what a given ISO contains. No checksums are pinned.
9. **Size.** 1.31 GiB with models and no LLM. With models and no LLM it is already large for a download; `--with-llm`
   adds ~2.6 GB.
10. **`install.sh` advertises `https://get.yantrik.dev/install.sh` and targets Alpine.** The
   OS is Debian. Either retarget it or stop advertising it.

---

## 9. The build

Built in WSL2 (Ubuntu 24.04, rustc 1.92.0, 12 cores, 31 GB RAM) from a **clean `git worktree`
at `HEAD` (`48ac75c`)**, not from the working tree. That was not a precaution: a release build
of the working tree **failed** — `email-service` did not compile, against another agent's
in-progress OAuth work (`unresolved import sha2`, `unlinked crate ureq`, 9 errors). The
worktree carries `HEAD` plus this change's edits to the scripts under `deploy/yantrik-os/`,
which is why the version string ends `-dirty`.

```bash
# 1. clean checkout of HEAD, on a path where ../yantrikdb still resolves
git worktree add /mnt/c/Users/sync/codes/yantrik-os-iso HEAD --detach

# 2. the workspace, sharing the main target dir so the dependency graph is not rebuilt
cd /mnt/c/Users/sync/codes/yantrik-os-iso
CARGO_TARGET_DIR=/mnt/c/Users/sync/codes/yantrik-os/target \
  cargo build --offline --release --workspace -j 8     # Finished in 19m 03s, exit 0

# 3. the ISO, assembled on a WSL-native filesystem (/tmp), output in ~/iso-work
cd ~/iso-work
YANTRIK_ISO_WITHOUT_MIND=1 \
TARGET_DIR=/mnt/c/Users/sync/codes/yantrik-os/target \
  bash /mnt/c/Users/sync/codes/yantrik-os-iso/deploy/yantrik-os/build-debian-iso.sh
```

| | |
|---|---|
| **File** | `~/iso-work/yantrik-os-v0.1.0-217-g48ac75c-dirty.iso` |
| **Windows path** | `\\wsl.localhost\Ubuntu\home\yantrik\iso-work\yantrik-os-v0.1.0-217-g48ac75c-dirty.iso` |
| **Size** | 1 411 780 608 bytes (1.31 GiB) |
| **sha256** | `55e9e9d02785edc214ad7ba8380f25f4e3adcb0a7efea7efcb19af7d5c8f55ae` |
| **Debian packages** | 744 |
| **Yantrik binaries in `/opt/yantrik/bin`** | 31 (+ `yantrik-install` = 32 files) |
| **Base** | Debian 13 trixie, amd64 |
| **Offline LLM** | no |
| **Mind** | absent (`YANTRIK_ISO_WITHOUT_MIND=1`) |
| **sshd** | installed, not enabled |
| **Built** | 2026-09-20T23:31:24Z |
| **ISO assembly time** | ~34 min (debootstrap → apt → models → squashfs xz → grub-mkrescue) |

`.sha256` and `.manifest` are written beside the ISO by the build.

The `cloud-init`/ISO package parity check passed: *"Every package cloud-init installs is in
the image (30)"*. `grub-mkrescue` produced a hybrid image — `xorriso` lists `/boot/grub`,
`/efi/boot`, `/efi.img`, `/boot.catalog` and `/live/{vmlinuz,initrd,filesystem.squashfs}`.

### What was verified in the built rootfs, by reading it

- `/opt/yantrik/config.yaml`: `user_name: "User"`, `api_base_url: "http://127.0.0.1:8341/v1"`,
  `server.host: "127.0.0.1"`, `updates.server: "https://releases.yantrikos.com"`
- `/opt/yantrik/update.conf`: `CHANNEL=beta`, `HOST=releases.yantrikos.com`, `SCHEME=https`
- `/etc/resolv.conf` → symlink to `/run/NetworkManager/resolv.conf` (no baked DNS server)
- `root` in `/etc/shadow` is `!*` — locked
- sshd not in `multi-user.target.wants`; no `sshd_config.d` override
- 14 `.desktop` entries; no `yantrik-music-player` or `yantrik-spreadsheet` binary or entry
- `/opt/yantrik/LICENSE` (GPL-3.0) and `/opt/yantrik/THIRD-PARTY-NOTICES.md` present
- `/opt/yantrik/share/harnesses/hermes/` — `__init__.py`, `adapter.py`, `desktop.py`,
  `plugin.yaml`
- A `strings` sweep of all 31 binaries for `192.168.*` and the author's name found **one**
  hit — see the `yantrik-email` finding in §3.

## 10. The boot test

**It boots.** Everything below is the machine's own output, read off its serial port or taken
off its framebuffer — not inference from the build log.

QEMU 8.2.2, **TCG only — `/dev/kvm` does not exist in this WSL instance**, so every timing
below is software emulation and much slower than real hardware.

```bash
qemu-system-x86_64 -m 4096 -smp 4 \
  -cdrom yantrik-os-v0.1.0-217-g48ac75c-dirty.iso -boot d \
  -display none -device virtio-vga \
  -chardev socket,id=ser0,path=…/ser.sock,server=on,wait=off -serial chardev:ser0 \
  -monitor unix:…/mon.sock,server,nowait -net none
```

The guest's serial console is a unix socket on this host, so the probe logs in to the **QEMU
guest** and to nothing else. Nothing was ssh'd to; no host outside this machine was contacted.

### What was seen

**Bootloader.** GRUB 2.12 drew its menu *on the serial line* — the change that made a headless
boot check possible. All four entries present and correctly named:

```
GNU GRUB  version 2.12
   *Install Yantrik OS
    Try Yantrik OS (live, no install)
    Install Yantrik OS (Safe Mode — software rendering)
    Try Yantrik OS (verbose, serial console)
   The highlighted entry will be executed automatically in 5s.
```

**Kernel and userspace.** Booted the default entry (`Install Yantrik OS`). First systemd
output at t≈20 s; reached multi-user and a serial getty:

```
Debian GNU/Linux 13 yantrik ttyS0
yantrik login:
```

**The session, asked of the running machine** (logged in as `yantrik`, password `yantrik`):

```
--- build ---       version=v0.1.0-217-g48ac75c-dirty git=48ac75c binaries=27 models=excluded
--- compositor ---  790 labwc -s /opt/yantrik/bin/yantrik-ui /opt/yantrik/config.yaml
--- shell ---       847 /opt/yantrik/bin/yantrik-ui /opt/yantrik/config.yaml
--- services ---    a11y-service network-service notifications-service
                    system-monitor-service weather-service
--- binaries ---    32
--- .desktop ---    14
--- shelved ---     0
--- sshd ---        disabled / inactive
--- update.conf --- CHANNEL=beta  HOST=releases.yantrikos.com  SCHEME=https
--- llm ---         api_base_url: "http://127.0.0.1:8341/v1"
--- yos ---         yos — talk to Yantrik OS from a command line.
```

Those five services are exactly the five registered with `autostart = true` in
`crates/yantrik-ui/src/main.rs`. The four on-demand ones (`notes`, `calendar`, `email`,
`perception`) are correctly *not* running.

`/opt/yantrik/logs/yantrik-os.log` shows the shell's companion initialising inside the VM:
50 built-in recipe templates registered, connector tools registered, and

```
Model capability profile detected profile=medium(~qwenB) family=4 tools=25
  mode=NativeFunctionCall ctx=32K steps=12 model_id="yantrik-4b"
Native tool calling: 8 always-on + dynamic per-query selection always=8 total=229
```

**The screen.** A framebuffer dump at t≈60 s after login, converted to PNG and looked at:
the first-boot onboarding, fullscreen, no title bar, in the shipped dark palette and the
Barlow typeface — the orb, *"Hey. I'm Yantrik."*, and, because the default GRUB entry passes
`yantrik.install=true`, the installer-mode fields: Full Name, Username, Password, Confirm
Password, Computer Name (`yantrik`), Companion Name, **Next**, and a **Skip** in the corner.
Saved at `~/iso-work/probe1/shot-060.png`.

So: bootloader → kernel → live squashfs root → systemd → autologin → `yantrik-session` →
labwc → `yantrik-ui` → onboarding, with the shipped theme and fonts, and five services up.

### What was NOT verified

- **Real hardware.** Nothing was booted on a physical machine. No UEFI firmware other than
  QEMU's default SeaBIOS path was exercised — **the EFI boot path is untested**, even though
  `/efi/boot` and `/efi.img` are present in the ISO.
- **KVM.** TCG only. Timing on real hardware is unknown; GPU paths entirely untested.
- **The installer.** `yantrik-install` was **not run**. No disk was partitioned, no installed
  system was booted. The installer fixes in §6 are unexercised code.
- **Onboarding past the first screen.** No text was typed into it; `Next` was never clicked.
  Whether the flow completes, whether a model endpoint can be saved, and whether the desktop
  appears behind it are all unverified.
- **The apps.** Not one of the 14 was opened. Their presence is verified; their behaviour is not.
- **The updater.** `yantrik-update check` was not run — it needs
  `https://releases.yantrikos.com`, which the rules for this work forbid contacting and which
  may not exist yet.
- **Networking.** Booted with `-net none`. NetworkManager, wifi and the network service's
  behaviour with a real interface are untested.
- **Voice, TTS, the embedder.** Models are on the image; none was loaded.
- **`--with-llm`.** Never built. That path is unexercised.
- **The Safe Mode and verbose GRUB entries.** Only the default entry was booted to completion.

### One thing the boot test found

`/etc/os-release` inside the live image still says `PRETTY_NAME="Debian GNU/Linux 13
(trixie)"`. Only `yantrik-install` rewrites it, so **the live ISO identifies itself as Debian**
— in the getty banner, to any tool that asks, and in bug reports. `build-debian-iso.sh` should
write `/etc/os-release` the way the installer does. Not changed here: it was found after the
image was built, and it is a one-line addition that deserves its own build to prove.

---

## 11. Publish checklist

- [ ] Publish the corresponding source, or a written GPL-3 §6 offer pinned to the git revision
      in `/opt/yantrik/BUILD`, for Yantrik OS itself — and separately for the Debian base
- [ ] Add `license = "GPL-3.0"` to the workspace `Cargo.toml`
- [ ] Resolve every `NEEDS VERIFICATION` in `THIRD-PARTY-NOTICES.md`
- [ ] Ship `OFL.txt` beside the fonts
- [ ] Generate a Rust dependency licence manifest (`cargo about`) and ship it
- [ ] Give the release tarball a config with no name and no private address
- [ ] Stand up `https://releases.yantrikos.com` with a valid certificate; verify
      `yantrik-update check` from a network that is not the author's
- [ ] Pin model checksums, or vendor the models, so a build is reproducible
- [ ] Decide and document what `--with-llm` ships and under what licence
- [ ] `git add --renormalize .` and commit, so the repository stops carrying CRLF blobs
- [ ] Boot the ISO on real hardware — UEFI and BIOS, at least one AMD and one Intel GPU
      (the EFI path in this image has never been booted, only built)
- [ ] Run `yantrik-install` to a real disk and boot the result — the installer changes in §6
      are unexercised
- [ ] Complete the onboarding flow once, end to end, and confirm the desktop behind it
- [ ] Make `build-debian-iso.sh` write `/etc/os-release` — the live image says "Debian"
- [ ] Move the `yantrik-email` demo mailbox out of the binary (§3)
- [ ] Write release notes that state: live password, passwordless sudo, no LLM by default,
      what data leaves the machine when an onboarding cloud provider is chosen
- [ ] Decide what happens to the eleven fossil scripts in §7
