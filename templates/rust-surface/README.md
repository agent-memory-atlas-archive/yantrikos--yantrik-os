# A Rust surface to copy

A to-do list a mind can read, add to, tick off and search, built on the `yantrik-surface` crate.
It is small on purpose: every part of it is a part your own surface needs, and nothing else.

| file | what it is |
| --- | --- |
| `src/lib.rs` | the surface: what it reports (`describe`), what it offers (four actions, each graded), and the handlers |
| `src/main.rs` | the program: checks its own declarations, then serves `app-my-surface.sock` until stopped |
| `my-surface.desktop` | how the desktop finds it while it is closed: `X-Yantrik-Surface`, `-Purpose`, `-Aliases` |
| `tests/surface.rs` | in-process tests: what it publishes, what each action does, what the dispatch refuses |
| `tests/yos_check.rs` | the program served on a private machine and held to the protocol by `yos check` |

The guide that explains each choice is [`docs/sdk/`](../../docs/sdk/README.md), starting with the
[Rust quickstart](../../docs/sdk/rust-quickstart.md).

## Try it first

From the root of the yantrik-os repository, on a Yantrik machine or any Linux:

```sh
cargo test -p my-surface          # both test files
cargo run -p my-surface           # serves app-my-surface.sock; ctrl-c to stop
```

and from another terminal:

```sh
yos describe my-surface
yos act my-surface add title="Water the plants" priority=high
yos act my-surface find query=plants
yos act my-surface remove index=0     # sensitive: in ask mode the person is asked first
yos check my-surface
```

(`yos` is `deploy/yantrik-os/yos` in the repository and `/opt/yantrik/bin/yos` on a Yantrik machine.)

## Make it yours

Pick an id: lowercase words joined by `-` (`tide-clock`, not `TideClock` or `tide_clock`). It is
what the surface publishes as `app`, the socket it binds (`app-tide-clock.sock`), and what a mind
types. Then, from the directory you copied this one to:

```sh
grep -rlE 'my-surface|my_surface|My Surface|my-tasks' . \
  | xargs sed -i -e 's/my-surface/tide-clock/g' -e 's/my_surface/tide_clock/g' \
                 -e 's/My Surface/Tide Clock/g' -e 's/my-tasks/tides/g'
mv my-surface.desktop tide-clock.desktop
```

That renames the package and the program (`tide-clock`), the crate (`tide_clock`), the surface id,
the name a person reads, the alias, and the `.desktop` file. `tests/surface.rs` fails if the
`.desktop` file and the surface disagree about the id, which is the mistake a rename makes.

Then replace the to-do list with what your program actually holds: the `describe` closure, the
actions, their grades. [Choosing a grade](../../docs/sdk/grades.md) and
[Designing a describe a mind can use](../../docs/sdk/describe.md) are the two pages to read before
you do.

## Outside this repository

Inside yantrik-os, `Cargo.toml` finds `yantrik-surface` next door. Copied anywhere else, point it
at the repository instead:

```sh
sed -i 's|yantrik-surface = { path = "../../crates/yantrik-surface" }|yantrik-surface = { git = "https://github.com/yantrikos/yantrik-os" }|' Cargo.toml
cargo test
```

`tests/yos_check.rs` looks for `yos` in `$YOS`, the repository, `/opt/yantrik/bin/yos` and `PATH`,
and says it skipped when it finds none.

## Install it

```sh
cargo build --release
sudo install -m755 target/release/tide-clock /usr/local/bin/
install -Dm644 tide-clock.desktop ~/.local/share/applications/tide-clock.desktop
```

The shell notices the `.desktop` file within a few seconds (`yos act shell refresh_apps` makes it
look at once). From then on `yos ls` lists `tide-clock` as closed with its purpose, and
`yos act shell open_app name=tide-clock` — or `name=tides` — starts it.
