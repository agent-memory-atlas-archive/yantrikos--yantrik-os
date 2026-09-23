#!/usr/bin/env python3
"""My Surface: a to-do list a mind can read, add to, tick off and search.

A starting point to copy — README.md says how to rename it. Everything that makes it a surface
is below: what it reports (the summary and the view), what it offers (four actions, each
graded), and the handlers. The dispatch around them is `yantrik_surface`'s: it checks the
arguments against the handlers' type hints, refuses a call decided on a view that has moved
(`STALE:`), holds each action to the machine's ceiling and the person's mode, spends a person's
grant, and answers every act with the view after it. So the handlers only do the work.

    python3 my_surface.py             # serves app-my-surface.sock until Ctrl-C or SIGTERM
    yos describe my-surface
    yos act my-surface add title="Water the plants" priority=high
    yos check my-surface
"""

import sys
from typing import Annotated, Literal

try:
    from yantrik_surface import Refusal, Surface
except ImportError:
    sys.exit("my-surface needs the yantrik_surface package: `pip install <yantrik-os>/sdk/python`, "
             "or copy sdk/python/yantrik_surface beside this file")

# The id this surface publishes as `app`, and binds as `app-my-surface.sock`. The `.desktop`
# file's `X-Yantrik-Surface` says the same; a test holds the two together.
APP_ID = "my-surface"

# Everything the list knows. A real program keeps its state however it likes; the surface reads
# it in the view and changes it only through the handlers.
tasks = []


def summary():
    """One line a person could read: what a mind reads first."""
    done = sum(1 for t in tasks if t["done"])
    return "My Surface — %d task%s, %d done" % (len(tasks), "" if len(tasks) == 1 else "s", done)


surface = Surface(APP_ID, summary=summary)


@surface.view
def state():
    """The state object, in the app's own words. It and the summary make the revision."""
    return {"tasks": [dict(task, index=i) for i, task in enumerate(tasks)]}


def task_at(index):
    """The task at `index`, or the refusal a caller reads when there is none."""
    if not tasks:
        raise Refusal("the list is empty; `add` a task first")
    if not 0 <= index < len(tasks):
        raise Refusal("there is no task %d; the list has %d, indexed 0 to %d"
                      % (index, len(tasks), len(tasks) - 1))
    return tasks[index]


# `standard` (the default grade): it changes the list, and `remove` takes it back.
@surface.action("add")
def add(title: Annotated[str, "What needs doing, in a few words"],
        priority: Annotated[Literal["low", "normal", "high"], "How soon it matters"] = "normal"
        ) -> dict:
    """Put a task on the list"""
    # Present and text: the dispatch checked. Whether it says anything is ours to check.
    title = title.strip()
    if not title:
        raise Refusal("`title` is empty; say what needs doing")
    tasks.append({"title": title, "priority": priority, "done": False})
    return {"index": len(tasks) - 1, "title": title, "priority": priority}


# `standard`: `done=false` undoes it.
@surface.action("complete")
def complete(index: Annotated[int, "The task's `index`, as describe lists it"],
             done: Annotated[bool, "false to mark it not done"] = True) -> dict:
    """Mark a task done, or not done again with `done` false"""
    task = task_at(index)
    task["done"] = done
    return {"title": task["title"], "done": done}


# `safe`: it reads and changes nothing, so it runs in every mode, plan included.
@surface.action("find", grade="safe")
def find(query: Annotated[str, "Words to look for, in any case"]) -> dict:
    """Search the tasks' titles, changing nothing"""
    query = query.lower()
    return {"query": query,
            "matches": [{"index": i, "title": t["title"], "done": t["done"]}
                        for i, t in enumerate(tasks) if query in t["title"].lower()]}


# `sensitive`, and its description says it cannot be undone: in `ask` mode the person sees a
# card first, and — because of those words — in `auto` mode too.
@surface.action("remove", grade="sensitive")
def remove(index: Annotated[int, "The task's `index`, as describe lists it"]) -> dict:
    """Take a task off the list. It cannot be undone"""
    title = task_at(index)["title"]
    del tasks[index]
    return {"removed": title, "left": len(tasks)}


if __name__ == "__main__":
    surface.serve()
