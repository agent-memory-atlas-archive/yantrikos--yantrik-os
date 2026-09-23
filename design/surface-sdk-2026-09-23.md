# The surface SDK — so anyone can put something where a mind can find it

*23 September 2026. Pranab: "We need to make sure we are making a strong SDK for anyone who wants to
expose stuff for minds to find."*

Yantrik OS is Debian underneath, so every Debian app installs and runs. What makes this OS different
is not the apps; it is that a mind can **find** an app, **read** what it shows, and **act** in it
under the person's grades. Today only the apps in this repo can do that, because everything an
outside author would need is missing or private to the repo.

## Where it stands (read from `main` at 02992f6)

- **No normative spec.** The wire contract is consistent in code (`control_surface.rs`,
  `control.rs`, `gate.rs`) but `docs/app-control.md` is out of date: it omits `revision`,
  `expect_revision`, `grant`, `agent_token`, `settled`, the error codes, and the `CEILING:` /
  `GRANT:` / `STALE:` refusals, and its example refusal is not the dispatch's wording.
- **Weak parameter types.** `Param` has text, number and flag. No integer, enum, array, object or
  default; the dispatch checks presence, not type.
- **One policy, five copies.** The ceiling/mode/grant decision lives in `gate.rs`, Blender's
  `surface.py`, the shell's `mind_mode.rs`, `yos-mcp`, and the companion's `app_ui.rs`. The
  checked-in vectors tie only the shell to the bridge. The copies already disagree: in Auto mode
  the "not recoverable" rule is applied by the bridge and the shell but not by `gate::decide`, so
  `yos act` or a raw socket runs what the bridge would have asked about.
- **Discovery is a closed list.** A running app is found by its socket. A closed app is known only
  through the hardcoded `ROUTES`/`PURPOSES`/`SURFACES` tables. A third-party app cannot be listed
  while closed, cannot declare aliases, and its notification buttons do not reach it.
- **Services re-implement dispatch** (fixed action ids, -32601 for unknown actions, no STALE or
  unexpected-argument checks), and their own JSON-RPC methods skip the gate entirely (#161).
- **Four socket-directory chains** (Rust, `yos`, Blender, the conformance probes).
- **Nothing owns a name.** Binding unlinks whatever is at the path; clients do not check who
  answers — including `app-shell`, through which grants are spent.
- **No developer experience.** No example, template, Python package, guide, or checker an outside
  author can run. Harnesses have all of these; surfaces have none.

## What the SDK is

### 1. A written protocol, versioned

`docs/surface-protocol.md`, normative, with JSON Schemas beside it (`docs/schema/describe.json`,
`act.json`): framing (newline-delimited JSON-RPC 2.0 over a unix socket), the one socket directory
chain, naming and aliases, `app.describe` / `app.act` / `rpc.ping`, the envelopes, `revision` and
how it is computed (with test vectors), `settles`, `expect_revision`, `grant`, `agent_token`, every
error code and refusal prefix with its exact meaning, and the parameter types. `describe` gains
`protocol: 1`, so a client can tell what it is talking to. The old `app-control.md` becomes the
guide that links to it.

### 2. One decision, proven identical everywhere

The policy is defined once (`gate::decide`, extended with the "not recoverable" rule so every door
decides alike) and **generated into vectors** (`deploy/yantrik-os/surface-vectors.json`): grade,
ceiling, mode, session rules, grant presence, recoverability → allow / `CEILING` / `GRANT`, with the
sentence. Every implementation — Rust gate, Python SDK, `yos-mcp`, the companion, the shell's mode
table — replays them in CI. A copy that drifts fails its own build.

### 3. SDKs in two languages first

- **Rust — `yantrik-surface`**: the dispatch (`Registry`, actions, params, the gate, revision,
  STALE, argument checking) with **no UI dependency**, so services and apps share it.
  `yantrik-app-runtime` keeps the UI-thread hop and builds on it; the three services that answer
  `app.act` themselves move onto it. `Param` gains integer, enum (with its values), array, object
  and a default, and the dispatch checks types.
- **Python — `yantrik_surface`**: stdlib only, one import:

  ```python
  from yantrik_surface import Surface

  s = Surface("libreoffice", summary=lambda: f"{doc.title()}, {doc.pages()} pages")

  @s.action("export_pdf", grade="standard", settles="later", expected_seconds=20)
  def export_pdf(path: str) -> dict:
      ...
  s.serve()
  ```

  Grades, params from type hints, the gate, revision and the socket all come with it. Blender's
  add-on moves onto it, so it stops carrying its own copy. This is the SDK for wrapping other
  people's apps: LibreOffice through UNO, GIMP through its Python, anything with a script API.
- TypeScript later, for VS Code and Electron apps, on the same spec and vectors.

### 4. Findable while closed

A surface declares itself in its `.desktop` file, which every Debian app already ships:

    X-Yantrik-Surface=libreoffice
    X-Yantrik-Purpose=Documents, spreadsheets and slides: open, read, edit and export them
    X-Yantrik-Aliases=writer;calc;impress
    X-Yantrik-Adapter=/usr/lib/yantrik/adapters/libreoffice

The shell's catalogue reads those keys, so a third-party surface is listed in `describe shell` →
`apps`, openable by `open_app`, reachable by its aliases, and routable from a notification button,
whether or not it is running. Our own apps ship `.desktop` files with the same keys, and the
hardcoded `ROUTES`/`PURPOSES`/`SURFACES` tables shrink to what cannot be expressed that way (the
shell's own screens). `X-Yantrik-Adapter` names a separate process that provides the surface for an
app that cannot host one itself.

### 5. A checker any author can run

`yos check <surface>` (also run by `release-check` for every surface on the machine): the describe
envelope against the schema; every action graded; parameter types valid; unknown action, missing
argument, unexpected argument, wrong type and stale revision each refused with the right code and
prefix; `revision` stable for an unchanged view and changing when it changes; describe answers
within 500 ms; no parameter named like a secret; the refusals match the vectors. It prints what it
saw and exits non-zero on a failure, so an author runs it in their own CI.

### 6. Names are owned

A surface refuses to bind over a live socket (it pings first) and only replaces a dead one. Clients
that matter — `yos` spending a grant, `yos-mcp`, the approval path — check the peer behind
`app-shell` is the shell's own binary (`SO_PEERCRED` + `/proc/<pid>/exe`) before trusting it. Same-uid
limits stand (see #154): this stops accidents and casual impersonation, not hostile code running as
the person.

### 7. Guide, templates, examples

`docs/sdk/`: quickstart in Rust, quickstart in Python, "wrap an app you did not write" (Blender as
the worked example), and how to choose a grade and design a `describe` a mind can use.
`templates/rust-surface/`, `templates/python-surface/`. `examples/hello-surface` (Rust) and
`examples/hello_surface.py`. And a flagship built only with the public SDK: **a LibreOffice
adapter** (open, read text and cells, write, export PDF), shipped as its own package.

## Order

| # | Piece | Touches |
|---|---|---|
| A | Spec + schemas + generated policy vectors (with the "not recoverable" rule moved into the gate) + `yos check` | docs, `gate.rs`, `yos`, `yos-mcp` |
| B | Rust `yantrik-surface`: UI-free dispatch, richer params with type checks; services moved onto it | `crates/yantrik-ipc-contracts`, new crate, `yantrik-app-runtime`, `services/*` |
| C | Python `yantrik_surface`; Blender moved onto it; replays the vectors | new `sdk/python/`, `apps/blender/addon` |
| D | Discovery from `.desktop` keys; aliases; closed apps listed; notification routing; approvals for services (#161) | `yantrik-shell-core`, `dock.rs`, `control.rs`, `control_approvals.rs` |
| E | Owned names (bind-if-dead; clients verify `app-shell`) | transport, `yos`, `yos-mcp` |
| F | Guide, templates, examples, and the LibreOffice adapter | docs, `templates/`, `examples/`, a new adapter package |

A, B, C, E can start together. D waits for the shell work in flight (the desk pieces and the Agents
glue touch the same files). F follows B and C.
