# A Python surface to copy

A to-do list a mind can read, add to, tick off and search, built on the `yantrik_surface`
package (standard library only, Python 3.11 or newer). It is the same surface as
[`templates/rust-surface`](../rust-surface/README.md), action for action and refusal for refusal,
because the two SDKs are one dispatch in two languages.

| file | what it is |
| --- | --- |
| `my_surface.py` | the surface and the program: what it reports, what it offers (four actions, each graded), the handlers, and `surface.serve()` |
| `my-surface.desktop` | how the desktop finds it while it is closed: `X-Yantrik-Surface`, `-Purpose`, `-Aliases` |
| `tests/test_my_surface.py` | what it publishes, what each action does, what the dispatch refuses, and the program held to the protocol by `yos check` |

The guide that explains each choice is [`docs/sdk/`](../../docs/sdk/README.md), starting with the
[Python quickstart](../../docs/sdk/python-quickstart.md).

## Try it first

From the root of the yantrik-os repository, on a Yantrik machine or any Linux:

```sh
python3 -m unittest discover -s templates/python-surface/tests -v
PYTHONPATH=sdk/python python3 templates/python-surface/my_surface.py    # ctrl-c to stop
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
mv my_surface.py tide_clock.py
mv my-surface.desktop tide-clock.desktop
```

That renames the surface id, the program, the name a person reads, the alias, and the `.desktop`
file. A test fails if the `.desktop` file and the surface disagree about the id, which is the
mistake a rename makes.

Then replace the to-do list with what your program actually holds: the summary, the view, the
actions and their grades. [Choosing a grade](../../docs/sdk/grades.md) and
[Designing a describe a mind can use](../../docs/sdk/describe.md) are the two pages to read before
you do. If your program's state belongs to one thread (a GTK or Qt main loop), read
[Wrap an app you did not write](../../docs/sdk/wrap-an-app.md) as well: the surface has to hop
onto that thread.

## Outside this repository

The program needs `yantrik_surface` importable. Either install it:

```sh
pip install <path-to-yantrik-os>/sdk/python
```

or copy `sdk/python/yantrik_surface/` beside `tide_clock.py` (it has no dependencies; this is
how the Blender add-on carries it). The tests find the SDK in the repository when they are inside
it, and an installed one otherwise; they look for `yos` in `$YOS`, the repository,
`/opt/yantrik/bin/yos` and `PATH`, and skip the `yos check` test when there is none.

## Install it

```sh
sudo install -m755 tide_clock.py /usr/local/bin/tide-clock
install -Dm644 tide-clock.desktop ~/.local/share/applications/tide-clock.desktop
```

The shell notices the `.desktop` file within a few seconds (`yos act shell refresh_apps` makes it
look at once). From then on `yos ls` lists `tide-clock` as closed with its purpose, and
`yos act shell open_app name=tide-clock` — or `name=tides` — starts it.
