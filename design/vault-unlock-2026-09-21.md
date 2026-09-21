# The vault protected nobody, and the machine had nothing to protect it with

The engine's vault encrypts credentials with AES-256-GCM under a data key, and since the fix
pinned at `9aa5899` it can *wrap* that key under a passphrase with Argon2id. With no passphrase
set it does not: the key stays in `memory.db`, in the `vault_security` table, in the clear, next to
the ciphertext it decrypts. `yantrikdb_core::vault` says so at the top of its own file — "that is
not encryption at rest; it is obfuscation with an extra step."

Nothing in this OS had ever called `set_passphrase`. The only caller was the `vault_set_pin` tool,
which required a mind to decide to run it. So every shipped machine's vault was the legacy
unprotected kind, and the desktop said nothing about it: `vault_list` reported
`PIN protection: DISABLED (set one with vault_set_pin for security)`, which reads like a feature
nobody switched on rather than a key sitting in a file.

Fixing that needs a passphrase, and the first question is where one could possibly come from. The
answer on this machine turned out to be more interesting than expected.

## Which paths actually hold a password

Four places in this OS receive a password a person typed. Three of them are not what they look
like.

**The lock screen holds a string, and checks it against a file.** `lock.slint:182` is a real input
and `wire/callbacks.rs` receives the plaintext, but `lock.rs`'s whole authenticator was a string
comparison against `~/.yantrik/lock_pin` — a file created with the literal contents `0000`, written
under the default umask so every account on the machine could read it, with no UI anywhere to
change it, and ending:

```rust
Err(_) => {
    // If we can't read the PIN file, allow unlock (fail-open for dev)
    tracing::warn!("Cannot read PIN file — allowing unlock");
    true
}
```

`rm ~/.yantrik/lock_pin` was the entire bypass. This is not a secret that can wrap anything.

**The login screen holds a real one — and nothing shows it.** `wire/login.rs:27` receives the
plaintext and `verify_password` checks it against `/etc/shadow` through `unix_chkpwd` or `openssl
passwd`. That is a genuine verified secret in this process. But `current-screen` defaults to `0`
and nothing in the Rust sets it to `32` except `main.rs:198`, reading `YANTRIK_START_SCREEN` — and
only `wire/installer.rs:581`, the graphical installer, ever writes that env var. A machine
installed by `deploy/yantrik-os/yantrik-install.sh` never gets it.

**The shell installer holds the password for a different machine.** `wire/installer.rs:51` receives
one and hashes it into the *target* disk's `/etc/shadow`. It is not this session's secret and the
session it belongs to does not exist yet.

**`yantrik-install.sh` asks, and then arranges for nobody to ask again.** Line 27 reads a password
with `read -rs`, line 161 hashes it into `/etc/shadow` — and line 202 writes
`agetty --autologin $USERNAME`, while line 182 writes the labwc environment *without*
`YANTRIK_START_SCREEN=32`. The person types a password twice and the machine boots straight to the
desktop. The live ISO is the same, minus the pretence: `build-debian-iso.sh:316` sets
`yantrik:yantrik` and `:763` autologins it.

So: **on every machine this project actually ships, no password reaches the shell.** The one path
that would deliver one exists, works, and is only reachable from the graphical installer.

### Summary

| path | plaintext in the shell? | what checks it | reached on a shipped machine? |
|---|---|---|---|
| lock screen — `lock.slint:182` → `wire/callbacks.rs:38` → `lock.rs:59` | yes | a plaintext file, default `0000`, was fail-open | yes |
| login screen — `login.slint:258` → `wire/login.rs:27` → `login.rs:131` | yes | `/etc/shadow` via `unix_chkpwd` / `openssl` | only if installed by the GUI installer |
| GUI installer — `onboarding.slint:1889` → `wire/installer.rs:51` | yes | nothing; it *sets* the target's `/etc/shadow` | yes, for the machine being built |
| `yantrik-install.sh:27` | n/a (shell script) | sets `/etc/shadow`, then autologins past it | yes |
| live ISO | no | nothing is ever asked | yes |

## The two tiers, as built

`crates/yantrik-ui/src/vault_unlock.rs` holds the whole state machine. Which tier a session is in
is decided by evidence — a static set by `wire/login.rs` when `/etc/shadow` has agreed — and not by
configuration, because a configuration flag saying "this machine has a password" is exactly the
kind of thing that stays `true` after somebody enables autologin.

### Tier 1 — the shell saw a verified password

Nothing new is derived. The password **is** the passphrase.

- first successful login on an unprotected vault → `set_passphrase`. The engine keeps the data key
  already in force and wraps *that*, then deletes the plaintext row, so every credential already
  stored survives. `first_login_protects_the_vault_in_place` reads one back to prove it.
- every login after → `unlock`.
- screen lock → `vault::lock()`, which overwrites the key bytes rather than dropping them.
- password change → `rewrap(old, new)`: the old passphrase must produce the key before the new one
  is allowed to wrap anything, so a mistyped current password cannot leave a vault wrapped under a
  passphrase nobody knows.
- a login password that does not open the vault means the account password was changed since the
  vault was wrapped. It stays locked, nothing is overwritten, and the prompt can re-open it.

The password is borrowed for the call and never stored, logged, or put in a message. Every string
this module can produce is a fixed sentence from `Outcome::message`, and `scrub()` takes the secret
out of anything that came from the engine. `secret_never_reaches_a_message` pushes a distinctive
passphrase through every path — protect, unlock, wrong, re-wrap, failed re-wrap, empty, the status
JSON, the prompt, the tier sentences — and asserts it appears in none of the resulting strings.

### Tier 2 — nothing to derive from

The live image, and any machine installed by `yantrik-install.sh`. There is no secret, and
pretending otherwise is theatre. So the machine says so instead:

```
yos describe shell | jq .state.vault
{
  "protected": false,
  "unlocked": false,
  "why": "this session signs in without a password, so there is nothing to lock the vault with"
}
```

Settings → Privacy & Security shows the same three facts off the same cached status, with
"Set a vault passphrase". Both read `vault_unlock::cached_status()`, so the panel and the control
surface cannot tell a person two different things about their own machine.

The passphrase is typed into a card the shell draws — `VaultUnlockCard` in
`components/intent_lens.slint`, deliberately the same opaque card, corner radius and warning-edge language as
the approval card, because it is the same kind of moment: the machine has stopped and is waiting
for a person.

### The screen lock, either way

`on_lock_screen` calls `vault::lock()` before the screen changes. A locked screen with the key
still in memory protects a screen.

On unlock, the typed secret is offered to the vault — but only if the vault is protected and shut,
and silently: if it opens, good; if not, nothing is said, because the person was unlocking a screen
and telling them they failed at something they were not attempting is worse than saying nothing.
Screen unlock never *depends* on the vault. A desktop that cannot be unlocked is a worse failure
than a vault that stays shut.

`lock.rs`'s fail-open is also gone. A missing PIN file is restored to the default and checked
against; anything else refuses. The file is written `0600`.

## What a mind hears

`crates/yantrik-companion-tools/src/vault.rs`. A protected, unopened vault answers, from every
vault tool:

```
VAULT_LOCKED: the vault is locked; the person has to unlock it. Do not ask for the
passphrase — you cannot carry it. Tell them the desktop is asking for it.
```

and the shell raises the prompt. Distinguishable from a generic failure on purpose — `Error: the
vault is locked; unlock it with its passphrase` was true, arrived in the same shape as every
transient error a model has learned to retry, and said nothing about the one thing that resolves
it.

The `pin` argument is gone from `vault_get`, `vault_store` and `vault_delete`. It was written when
the PIN was a hash in a table that gated the tools while the key sat beside it in the clear — so
relaying it cost nothing that was not already lost. It is not a formality now: the PIN *is* what
the key is wrapped under, and a model that asks for it puts it in the transcript, the context
window, on the wire to whatever provider is answering, and in whatever that provider keeps. A
companion answering over Telegram would have carried it across a third party's servers to unlock a
vault on a desk.

`vault_set_pin` can still start the flow — "this vault is not protected and it should be" is a
useful thing for a companion to notice — but it can no longer finish it: it takes no passphrase and
only raises the prompt. Passing `new_pin` anyway is refused *loudly*, telling the model that a
secret it was given should now be changed, because it is in the conversation. `remove` is refused
outright: putting the key back in the file in the clear is the one vault operation that makes the
machine less safe, and it should not be a one-click suggestion a mind can put in front of someone.

## How "only a person can type it" is enforced

Structurally, in four places, then checked mechanically.

1. **The passphrase has no serialisable form on any surface.** It travels as
   `vault_unlock::Op`, a typed enum nothing deserialises. `CompanionCommand::RunTool` — the door
   every mind and the whole control surface come through — takes a `serde_json::Value`; `Op` cannot
   be built from one.
2. **`CompanionBridge::vault` is on the bridge, not on `CompanionHandle`.** The handle is the part
   that travels: `companion_rpc` holds one and answers other processes with it. Every method on it
   is reachable, eventually, by something on a socket. This one is not on it.
3. **`on_vault_unlock_submit` is a Slint callback**, wired in `wire/vault.rs` and nowhere else. The
   only thing that can fire it is a person pressing Enter or clicking in a window this shell drew —
   the same property `on_approval_allow` has, for the same reason.
4. **Settings does not collect it**, though it has the panel and could. `show_screen settings
   section=privacy` is a published action, so an agent can put that panel in front of a person, and
   a passphrase field on a screen something else can navigate to is a field something else can be
   standing in front of. The button raises the one card.

Tested the way `approvals_published_actions_cannot_grant` tests its own property — by reading the
source of every `control*.rs`, not a list somebody maintains beside them:

`no_published_action_can_carry_a_passphrase` scans every `Action::new("…")` and every `Param::…`
for `passphrase`, `password`, `passwd`, `pin`, `secret`, `credential`, `unlock`. Two exceptions,
each named in a constant with its justification: the action `pin_app` and its flag `pinned`, which
pin an app tile to START and match only because "pin" is what this passphrase used to be called —
which is precisely why the word is still watched. The test caught both on its first run, which is
the evidence that it reads the real surface.

`the_vault_tools_do_not_ask_a_mind_for_the_passphrase` asserts no `"pin"`, `"new_pin"`,
`"current_pin"` or `"passphrase"` parameter has come back into the tool definitions, and that
`VAULT_LOCKED:` is still the answer a locked vault gives.

## What this protects against, and what it does not

**Protected, once a passphrase is set.** Someone who obtains the memory database file and not the
running machine:

- a backup, an rsync, a Time Machine, a `scp` of `/opt/yantrik/data/`;
- a synced folder — Dropbox, Drive, Syncthing — that the database happens to sit inside;
- a stolen disk, or a copied VM image, of a machine that is powered off or whose session is locked;
- anyone who can read the file as another user on a shared box.

They get ciphertext and an Argon2id-wrapped key. A wrong passphrase fails AEAD authentication
rather than producing plausible-looking garbage, so there is nothing to attack offline except
Argon2id itself at 19 MiB and 2 passes — a fraction of a second for one guess, a very long time for
a billion.

**Not protected. None of this helps against any of the following, and the design note says so
rather than leaving it to be discovered.**

- **Anything running as the user while the session is unlocked.** The key is in the shell's memory
  by design — that is what "unlocked" means. Any process with the same uid can read `/proc/<pid>/
  mem`, attach a debugger, or just call `vault_get` through the companion socket. The socket
  directory's `0700` keeps *other* users out; it was never a boundary against the user's own code.
- **An autologin machine where nobody set a vault passphrase.** That is the default on the live ISO
  and on every `yantrik-install.sh` machine, and it is exactly the state this note exists to
  describe: the key is in the file. The desktop now says so in `describe shell` and in Settings
  instead of reporting a feature as off.
- **A machine that is on, unlocked and unattended.** The screen lock is a PIN compared against a
  file. It now fails closed and the file is `0600`, but it is still not a cryptographic boundary,
  and it only covers the shell's own canvas — any labwc toplevel stays visible and interactive
  beside it, and the compositor's keybinds are still live.
- **The account password itself.** On tier 1 the vault is exactly as strong as the login password,
  because it *is* the login password. A weak one is a weak vault.
- **A forgotten passphrase.** There is no recovery. `set_passphrase` deletes the plaintext key row;
  that is the entire point. The card says so before the person commits to one.
- **Credentials after they leave the vault.** `vault_get` returns a password in a tool result,
  which goes into a model's context and possibly to a remote provider. That is the next thing to
  fix and it is not fixed here.

## Item 2 — the socket files were 0755

`bind(2)` creates the socket node under the process umask, which is `022` everywhere this ships, so
every control surface and service socket came out `srwxr-xr-x`. `design/approvals-2026-09-21.md`
recorded this and deferred it: "the socket file's own mode (`0700` after bind, or a `umask(0o077)`
around it) is the belt to the directory's braces, and that is a deliberate decision to take
separately." Taken now.

One bind site in the whole repo — `crates/yantrik-ipc-transport/src/server.rs:237`, which every
service, all fourteen apps, the companion and the harness go through. `private_socket_file` chmods
the node to `0600` immediately after.

| socket | before | after |
|---|---|---|
| `$XDG_RUNTIME_DIR/yantrik/app-*.sock` (14 apps) | `srwxr-xr-x` | `srw-------` |
| `$XDG_RUNTIME_DIR/yantrik/*.sock` (9 services) | `srwxr-xr-x` | `srw-------` |
| `$XDG_RUNTIME_DIR/yantrik/companion.sock` | `srwxr-xr-x` | `srw-------` |
| `$XDG_RUNTIME_DIR/yantrik/harness.sock` | `srwxr-xr-x` | `srw-------` |
| `/run/yantrik/perception.sock` (root) | `srwxr-xr-x` | `srw-------` |
| the directory, all cases | `0700` (`harden`) | `0700`, unchanged |

Three things worth saying plainly.

**0755 was never what let anyone in.** Connecting to a unix socket needs *write* on the node, and
`x` on a socket means nothing. The directory's `0700` is the real boundary and still is. This is
defence in depth, not a fix for a live hole.

**The window between `bind` and `chmod` is not closed.** For those microseconds the node exists at
0755 — inside a directory no other uid may traverse, so nobody else can name it, let alone open it.
Closing it properly would mean `umask(0o177)` around the bind, which is process-global state in a
process that binds sockets from several threads; that would be a worse bug than the one being
fixed. The comment at the function says this.

**It is best-effort.** A failure warns and continues, leaving the node exactly as every shipped
machine has had it. Returning the error would take down a service over a defence-in-depth measure,
and a desktop that will not start is worse than a socket mode no better than yesterday's. This
matters specifically for perception-service, whose Landlock ruleset (`scope.rs:315`) grants
`READ_FILE | READ_DIR | WRITE_FILE | MAKE_SOCK | REMOVE_FILE` on its socket directory and nothing
about changing modes.

`a_bound_socket_is_private_and_its_owner_can_still_connect` asserts both halves — the mode is 0600
*and* a same-uid `connect` succeeds, because a mode that locked out the owner would be
indistinguishable from a dead service. `tests/conformance/lib.py` reads no socket mode anywhere
(only `Path.exists()`, which needs directory search, and `connect`, which needs owner-write); it is
unaffected.

### perception-service: left as it is, deliberately

The brief's premise was that perception's socket lives in `/run/yantrik` "so that the user's
session can reach it". **It cannot, and never could.** Run as root with no `XDG_RUNTIME_DIR`,
`socket_dir()` picks `/run/yantrik` and `harden()` makes it `root:root 0700`. The desktop user
cannot traverse it, so the mode on the node inside has never mattered. `yos` already admits this
when it tries: *"perception runs as root; try sudo"* (`deploy/yantrik-os/yos:140`). There is also no
systemd unit for perception-service anywhere in the repo, and `main.rs:256` registers it
`autostart: false` because it needs `CAP_NET_ADMIN` and `CAP_SYS_ADMIN` that a desktop session
cannot grant.

Nothing was changed to open that path, because every way of doing it is unverifiable from here:
`harden()` is shared by *every* socket directory, so loosening it would loosen the session's own;
a `chown`/`setfacl` on the directory would be denied by the Landlock ruleset applied over it on the
next line (this is the same class of bug `harden`'s early return at `server.rs:94` exists to work
around); and none of it can be exercised without `CAP_NET_ADMIN` on a real kernel. The node is now
`0600` there too, and that is deliberate *including* there: if the session is ever to reach this
service, the grant must be a named one — a POSIX ACL for the desktop uid, or a socket-activated
unit that passes the fd — and not the `o+rwx` `bind` used to hand out by accident. A world bit is
not an access-control decision; it is the absence of one. The comment at
`services/perception-service/src/main.rs:138` says all of this at the call site.

## Verifying it on the machine

As the desktop user, `XDG_RUNTIME_DIR=/run/user/1000`.

**1. The socket modes.**

```sh
ls -l /run/user/1000/yantrik/
#   srw-------  …  app-notes.sock       — every one of them, was srwxr-xr-x
ls -ld /run/user/1000/yantrik
#   drwx------  …                       — unchanged; still the real boundary

# And the session still works, which is the half that matters:
yos describe shell   >/dev/null && echo shell ok
yos describe notes   >/dev/null && echo app ok
python3 tests/conformance/run.py        # unchanged
```

**2. Perception, if it is running at all.** Expect it not to be; this is the check that the
situation is what the note says, not that it was fixed.

```sh
ls -ld /run/yantrik ; sudo ls -l /run/yantrik/perception.sock
#   drwx------ root root  /run/yantrik
#   srw------- root root  perception.sock
yos describe perception
#   …perception runs as root; try sudo   — the status quo, stated
sudo -E env XDG_RUNTIME_DIR= yos describe perception   # this is the way in today
```

To grant the session access deliberately — **not done here, verify before adopting**:

```sh
sudo setfacl -m u:yantrik:x  /run/yantrik
sudo setfacl -m u:yantrik:rw /run/yantrik/perception.sock
yos describe perception     # should now answer without sudo
# then restart perception-service and confirm Landlock did not reject the ruleset:
journalctl -u perception-service -n 40 | grep -i 'landlock\|socket dir'
```

**3. The vault, on an autologin machine (the live image, or a `yantrik-install.sh` install).**

```sh
yos describe shell | python3 -c 'import json,sys; print(json.load(sys.stdin)["state"]["vault"])'
#   {'protected': False, 'unlocked': False,
#    'why': 'this session signs in without a password, so there is nothing to lock the vault with'}

# Ask a mind for a credential. The answer names the person, not an error class:
yos act shell run_tool name=vault_get args_json='{"service":"github.com"}'
#   (unprotected vault: it answers normally — nothing is locked yet)
```

Then set one: **Settings → Privacy & Security → Set a vault passphrase**. The card appears in the
top-right corner, warning-edged, over whatever screen is up. Type a passphrase and press Enter.

```sh
yos describe shell | python3 -c 'import json,sys; print(json.load(sys.stdin)["state"]["vault"])'
#   {'protected': True, 'unlocked': True}          — no `why`; there is nothing to explain

# The key is gone from the file, and the wrapped one is there instead:
sqlite3 /opt/yantrik/data/memory.db "SELECT key FROM vault_security;"
#   kdf
#   kdf_salt
#   dek_wrapped            — and NO `vault_dek`. That row is the whole exposure.
```

**4. Screen lock closes it, and a mind is told the truth.**

```sh
yos act shell lock          # or Super+L
# unlock with the screen PIN (not the vault passphrase)
yos describe shell | python3 -c 'import json,sys; print(json.load(sys.stdin)["state"]["vault"])'
#   {'protected': True, 'unlocked': False}

yos act shell run_tool name=vault_get args_json='{"service":"github.com"}'
#   VAULT_LOCKED: the vault is locked; the person has to unlock it. Do not ask for the
#   passphrase — you cannot carry it. Tell them the desktop is asking for it.
```

…and within a second the unlock card is on screen, saying *"a mind asked for a saved credential"*.
Type the passphrase there; the same `vault_get` then answers.

**5. Nothing on the socket can supply it.** There is no action to try, which is the point — the
test enumerates the surface rather than trusting this. The closest thing a caller can reach:

```sh
yos act shell run_tool name=vault_set_pin args_json='{"action":"set","new_pin":"1234"}'
#   REFUSED: `new_pin` is not an argument of this tool and was ignored. …
#   If a person just told you one, tell them to change it: it is now in this conversation.

yos act shell run_tool name=vault_set_pin args_json='{"action":"set"}'
#   The desktop is now asking the person for a vault passphrase. You will not see it …
#   — and the card appears. A caller can make the machine ask; it cannot answer.
```

**6. On a machine installed by the graphical installer** (`YANTRIK_START_SCREEN=32` in
`~/.config/labwc/environment`), the login screen appears at boot. Sign in with the account
password, then:

```sh
sqlite3 /opt/yantrik/data/memory.db "SELECT key FROM vault_security;"   # dek_wrapped, first sign-in
yos describe shell | python3 -c 'import json,sys; print(json.load(sys.stdin)["state"]["vault"])'
#   {'protected': True, 'unlocked': True}
```

Change the account password with `passwd` and sign in again: the vault will *not* open, the log
says so (`The vault did not open with this login password — it is wrapped under an earlier one`),
nothing is overwritten, and the unlock prompt takes the old one.

## Tests

| where | what |
|---|---|
| `crates/yantrik-ui/src/vault_unlock.rs` | 13, against a real SQLite file and the real engine functions: a new vault is unprotected and says why; first login protects it in place with the credentials readable afterwards; a later login unlocks rather than re-wrapping; screen lock closes it and the vault then refuses with "locked"; a wrong passphrase neither opens nor corrupts and the right one still works after; a password change re-wraps and the old one stops working; a wrong *current* password changes nothing; an empty passphrase is not a passphrase; the tier comes from what happened, not from configuration; a protected vault carries no `why`; **`secret_never_reaches_a_message`** — a distinctive passphrase through every path, asserted absent from all 19 strings the module can produce; `scrub` redacts without throwing the message away; the prompt does not stack |
| `crates/yantrik-ui/src/control_approvals.rs` | 2: **`no_published_action_can_carry_a_passphrase`** — every `Action::new` and every `Param::` in every `control*.rs` scanned for seven secret-shaped words, with two named exceptions it caught on its first run; `the_vault_tools_do_not_ask_a_mind_for_the_passphrase` — no `pin`/`new_pin`/`current_pin`/`passphrase` parameter has come back, and `VAULT_LOCKED:` is still the answer |
| `crates/yantrik-ui/src/lock.rs` | 3: a missing PIN file does not unlock the screen (and is restored, so nobody is locked out); the PIN file is `0600`; the right PIN unlocks and the wrong one does not |
| `crates/yantrik-ipc-transport/src/server.rs` | 1 more: a freshly bound socket is `0600` **and** its owner can still connect |

```
cargo test --offline --profile fast -p yantrik-ui --bin yantrik-ui       # 233 passed
cargo test --offline --profile fast -p yantrik-ipc-transport             # 8 passed
cargo test --offline --profile fast -p yantrik-companion-tools           # 48 passed
cargo check --offline --profile fast --workspace
python3 tests/app-lints/run.py                                            # 0 new
```

## Not built, and why

**Mail secrets into the vault.** `services/email-service` keeps passwords and OAuth tokens in
plaintext in `~/.config/yantrik/email.json` (0600) because it is a separate process from the shell,
which owns the vault. The caller-identity work of 2026-09-21 makes the broker possible and it is
designed below, but it was not built: Item 1 and Item 2 came first and the broker is a new IPC
surface holding the machine's most-used credentials, which is not something to land at the end of a
session on an untested path.

The shape it should take:

- The shell serves `credentials.get {service, account}` and `credentials.put {service, account,
  secret}` on its existing control socket, and answers **only** when
  `yantrik_app_runtime::control::caller()` resolves — through `/proc/<pid>/exe`, the same uid, and
  the start-time bracketing `caller_identity.rs` already does — to `/opt/yantrik/bin/email-service`
  itself. Not "a process whose name contains email-service": the resolved exe path, compared whole.
- A locked vault answers with the `VAULT_LOCKED` sentinel and raises the prompt, exactly as the
  tools do. email-service then says *"mail is locked until you unlock the vault"* in the Email app
  and **never falls back to a file**. That last clause is the one that needs a test: a fallback
  path added later "so mail keeps working" would put the secrets back on disk and nobody would
  notice, because mail would keep working.
- On first unlock, migrate: read `email.json`, `vault::store` each password and token, rewrite the
  file with the non-secret settings only (servers, ports, display name, provider) and a comment
  saying where the secrets went. Keep a one-shot marker so a half-finished migration resumes rather
  than re-reading a file it already emptied.
- The migration must be idempotent and must not delete anything until the vault write has been read
  back successfully. Losing someone's mail password to a migration is worse than leaving it in a
  0600 file for another week.

**Closing the bind/chmod window with `umask`.** Described above: process-global state in a
multi-threaded binder, for microseconds inside a directory nobody else can traverse.

**Granting the session access to perception-service.** Described above: unverifiable from here, and
the commands to try it on a VM are in step 2.

**Making the login screen reachable on `yantrik-install.sh` machines.** That script and
`deploy/yantrik-os/yos*` are outside this change's ownership, and the two installers disagreeing
about whether a machine has a login screen is a separate decision — a real one, since it is the
difference between tier 1 and tier 2 for every machine installed from the command line. It is
recorded here because it is the single change that would do the most for the vault on shipped
machines: one line adding `YANTRIK_START_SCREEN=32` to `yantrik-install.sh:182` and removing the
autologin drop-in at `:201`, at the cost of asking people to type their password at boot.
