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
d.PROJECTS = os.path.join(tmp, "Projects")
d.BETWEEN_TURNS_SECS = 0
d.time.sleep = lambda s: None
d.show_mind_view = lambda: None
d.shell_state = lambda: {"thinking": False}
d.print = lambda *a, **k: None
sent, replies, pages = [], [], []
d.send = lambda text: (sent.append(text), (True, ""))[1]
d.wait_for_turn = lambda _: ({"conversation": [{"role": "assistant", "text": replies[0][0]}]}, replies[0][1],
                              replies.pop(0)[2:] == ("card",))
d.check_page = lambda m: pages.pop(0) if pages else (True, "")
chats = []
d.new_chat = lambda: (chats.append(len(sent)), (True, ""))[1]

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

replies[:] = [("still nothing", "card unanswered")]
d.main()
check("a stopped build with no plan yet is started again with its brief, not keep going",
      sent[-1].startswith("Please build") and second["name"] in sent[-1], sent[-1])

os.makedirs(os.path.join(d.PROJECTS, second["folder"]))
open(os.path.join(d.PROJECTS, second["folder"], "PLAN.md"), "w").close()
replies[:] = [("step done", "answered"), ("FINISHED", "answered")]
pages[:] = [(False, "Uncaught TypeError: x is {not} a function"), (True, "")]
d.main()
check("a stopped build with a plan is resumed with keep going", sent[-2].startswith("Please keep going on " + second["name"]), sent[-2])
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

check("FINISHED counts on a line of its own, bare or in markdown, never inside a sentence",
      d.said_finished("All done.\n\nFINISHED") and d.said_finished("**FINISHED**")
      and not d.said_finished("Orrery is finished, every step is ticked") and not d.said_finished("UNFINISHED")
      and not d.said_finished("So nothing is finished, and I'm not going to write FINISHED on a line for work that isn't there."))

# A reply past describe's clip is read whole with read_message, where its FINISHED is.
calls = []


class Done:
    def __init__(self, stdout):
        self.stdout, self.returncode = stdout, 0


def fake_run(argv, **_):
    calls.append(argv)
    whole = json.dumps({"index": 9, "text": "x" * 700 + chr(10) + chr(10) + "FINISHED"}, indent=2)
    return Done("Yantrik, desktop\naccepted: True\n" + whole + "\n{\"state\": 1}\n")


real_run, d.subprocess.run = d.subprocess.run, fake_run
whole = d.whole_text({"index": 9, "text": "x" * 600 + "… (709 characters total)", "clipped": True})
d.subprocess.run = real_run
check("a clipped reply is read whole, so a FINISHED past the clip is seen",
      d.said_finished(whole) and calls and calls[0][-2:] == ["read_message", "index=9"], whole[-40:])
check("an unclipped reply is taken as it is, with no extra call",
      d.whole_text({"index": 3, "text": "short FINISHED"}) == "short FINISHED")

d.write_state(d.DONE, set())
replies[:] = [("the command needs your OK", "answered", "card"), ("still waiting on you", "answered", "card"),
              ("unused", "answered")]
before = len(sent)
d.main()
check("two turns running with a card stop the mission for a person",
      len(sent) - before == 2 and len(replies) == 1 and specs[0]["id"] not in d.finished_missions())
replies[:] = [("asked, then allowed", "answered", "card"), ("FINISHED", "answered")]
d.main()
check("one card that a person answers does not stop the mission", specs[0]["id"] in d.finished_missions())

d.write_state(d.DONE, set())
d.write_state(d.STARTED, set())
replies[:] = [("FINISHED", "answered")]
chats.clear()
before = len(sent)
d.main()
check("a mission started from its brief gets a fresh conversation first",
      chats == [before], repr(chats))
os.makedirs(os.path.join(d.PROJECTS, specs[1]["folder"]), exist_ok=True)
open(os.path.join(d.PROJECTS, specs[1]["folder"], "PLAN.md"), "w").close()
d.write_state(d.STARTED, {specs[1]["id"]})
replies[:] = [("FINISHED", "answered")]
chats.clear()
d.main()
check("a resumed build keeps its conversation", chats == [] and sent[-1].startswith("Please keep going"), repr(chats))

d.write_state(d.DONE, set())
d.write_state(d.STARTED, set())
replies[:] = [("I can't make progress: every command needs your approval.", "answered"),
              ("I still can't make progress: every command needs your approval.", "answered"),
              ("unused", "answered")]
before = len(sent)
d.main()
log = [json.loads(line) for line in open(d.LOG, encoding="utf-8")]
check("the same answer twice stops the mission",
      len(sent) - before == 2 and len(replies) == 1 and log[-1].get("why") == "the same answer twice", repr(log[-1]))
check("answers that differ are not taken for a repeat",
      not d.same_answer("Step 1 done: layers.js written.", "Step 2 done: the palette now saves.")
      and not d.same_answer("", ""))

with open(os.path.join(HERE, "yantrik-live-director.service"), encoding="utf-8") as f:
    unit = [line.strip() for line in f]
check("the Director's service has no IP, so a page the Mind wrote reaches no network when checked",
      "IPAddressDeny=any" in unit and not any(line.startswith("IPAddressAllow") for line in unit))

sys.exit(1 if failed else 0)
