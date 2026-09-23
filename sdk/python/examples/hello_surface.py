#!/usr/bin/env python3
"""The smallest complete surface: a list a mind can read, add to and clear.

    python3 hello_surface.py          # binds app-hello.sock and answers until Ctrl-C

    yos describe hello                # the summary, the state, the actions and their grades
    yos act hello add text=milk
    yos act hello add text=eggs count=2
    yos act hello clear               # sensitive: in ask mode the person is asked first

Everything a caller can rely on — the envelopes, the revision, the refusals, the grades,
the ceiling, the mode and the grant — comes from `yantrik_surface`. What is written here is
only what this app is.
"""

import os
import sys
import threading
import time
from typing import Annotated

# Run from a checkout without installing: the package is one directory up.
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))

from yantrik_surface import Refusal, Surface  # noqa: E402

items = []
clearing = threading.Event()

surface = Surface(
    "hello",
    summary=lambda: "Hello — %d item%s%s" % (
        len(items), "" if len(items) == 1 else "s", ", clearing" if clearing.is_set() else ""),
)


@surface.view
def state():
    return {"items": list(items), "clearing": clearing.is_set()}


@surface.action("add", grade="standard")
def add(text: Annotated[str, "what to put on the list"],
        count: Annotated[int, "how many times, 1 to 100"] = 1) -> dict:
    """Add an item to the list, `count` times."""
    text = text.strip()
    if not text:
        raise Refusal("`text` is empty; say what to add")
    if not 1 <= count <= 100:
        raise Refusal("`count` must be from 1 to 100; %d is not" % count)
    items.extend([text] * count)
    return {"added": text, "count": count, "items": len(items)}


@surface.action("clear", grade="sensitive", settles="later", expected_seconds=5)
def clear() -> dict:
    """Empty the list, item by item, within about five seconds. The answer comes at once and
    says `settled: false`; describe shows `clearing` until it is done. It cannot be undone."""
    if clearing.is_set():
        raise Refusal("the list is already being cleared")
    clearing.set()
    pause = min(0.25, 5.0 / max(1, len(items)))

    def work():
        while items:
            time.sleep(pause)
            items.pop()
        clearing.clear()

    threading.Thread(target=work, name="hello-clear", daemon=True).start()
    return {"clearing": len(items)}


if __name__ == "__main__":
    surface.serve()
