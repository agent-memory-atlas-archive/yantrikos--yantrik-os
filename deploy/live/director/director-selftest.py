#!/usr/bin/env python3
"""The Director's choices, with the desktop, the Mind and the page check stood in for.

Holds what the live machine depends on: a stopped build is resumed, not restarted; a FINISHED
over a page that throws is refused; the page's errors reach the Mind whole, braces and all; and
once every build is finished the Director goes round them in polish rounds instead of idling
(561 sat idle for two weeks after Starfall, 2 to 15 Oct 2026). Exit 0 when every check holds.
"""
import importlib.util
import json
import os
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("director", os.path.join(HERE, "director.py"))
d = importlib.util.module_from_spec(spec)
spec.loader.exec_module(d)

tmp = tempfile.mkdtemp()
d.STATE_DIR = tmp
for k in ("LOG", "DONE", "STARTED", "POLISHED"):
    setattr(d, k, os.path.join(tmp, os.path.basename(getattr(d, k))))
d.BETWEEN_TURNS_SECS = 0
d.time.sleep = lambda s: None
d.show_mind_view = lambda: None
d.shell_state = lambda: {"thinking": False}
d.print = lambda *a, **k: None
sent, replies, pages = [], [], []
d.send = lambda text: (sent.append(text), (True, ""))[1]
d.wait_for_turn = lambda _: ({"conversation": [{"role": "assistant", "text": replies[0][0]}]}, replies.pop(0)[1])
d.check_page = lambda m: pages.pop(0) if pages else (True, "")

failed = 0


def check(name, ok, said=""):
    global failed
    print(("ok    " if ok else "FAIL  ") + name + ("" if ok else ": " + said[:300]))
    failed += not ok


with open(os.path.join(HERE, "missions.json"), encoding="utf-8") as f:
    specs = json.load(f)
check("every mission has the fields its wording needs",
      all({"id", "title", "name", "folder", "what", "details", "plan"} <= set(s) for s in specs))
check("mission ids and folders are unique",
      len({s["id"] for s in specs}) == len(specs) == len({s["folder"] for s in specs}))

d.write_state(d.DONE, {specs[0]["id"]})
second = specs[1]
replies[:] = [("working on it", "card unanswered")]
d.main()
check("the first unfinished build starts with its brief",
      sent[-1].startswith("Please build") and second["name"] in sent[-1] and "{" not in sent[-1], sent[-1])

replies[:] = [("step done", "answered"), ("FINISHED", "answered")]
pages[:] = [(False, "Uncaught TypeError: x is {not} a function"), (True, "")]
d.main()
check("a stopped build is resumed with keep going", sent[-2].startswith("Please keep going on " + second["name"]), sent[-2])
check("a page that throws puts its errors, braces and all, at the head of the next prompt",
      "x is {not} a function" in sent[-1] and "PLAN.md side by side" in sent[-1], sent[-1])
check("a FINISHED over a clean page finishes the build", second["id"] in d.finished_missions())

replies[:] = [("FINISHED", "answered"), ("FINISHED", "answered")]
pages[:] = [(False, "SyntaxError: Unexpected token"), (True, "")]
before = len(sent)
d.main()
check("a FINISHED over a page that throws is refused, and the mission goes on",
      len(sent) - before == 2 and "SyntaxError" in sent[-1] and specs[2]["id"] in d.finished_missions())

d.write_state(d.DONE, {s["id"] for s in specs})
names = []
for _ in range(len(specs) + 2):
    replies[:] = [("FINISHED", "answered")]
    d.main()
    names.append(next(s["name"] for s in specs if sent[-1].startswith("Please make " + s["name"] + " better")))
check("with every build finished, polish rounds go round every build in turn",
      names == [s["name"] for s in specs] + [specs[0]["name"], specs[1]["name"]], repr(names))
check("a polish round works from IDEAS.md", "IDEAS.md" in sent[-1] and "{" not in sent[-1], sent[-1])
check("a polish round is never recorded as a finished mission",
      not any(m.startswith("polish:") for m in d.finished_missions()))

sys.exit(1 if failed else 0)
