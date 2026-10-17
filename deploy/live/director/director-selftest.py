#!/usr/bin/env python3
"""The Director's choices, with the desktop, the Mind and the page check stood in for.

Holds what the live machine depends on: a stopped build is resumed, not restarted; a FINISHED
over a page that throws is refused; the page's errors reach the Mind whole, braces and all; and
once every build is finished the Director goes round them in polish rounds instead of idling
(561 sat idle for two weeks after Starfall, 2 to 15 Oct 2026). Exit 0 when every check holds.
"""
import html.parser
import importlib.util
import json
import os
import re
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("director", os.path.join(HERE, "director.py"))
d = importlib.util.module_from_spec(spec)
spec.loader.exec_module(d)

tmp = tempfile.mkdtemp()
d.STATE_DIR = tmp
for k in ("LOG", "DONE", "STARTED", "POLISHED", "POLISH_DUE", "LAST", "SHOWING"):
    setattr(d, k, os.path.join(tmp, os.path.basename(getattr(d, k))))
d.PROJECTS = os.path.join(tmp, "Projects")
d.SHOWCASE_DIR = os.path.join(tmp, "showcase")
# Builds in order first; interleaving with polish rounds has its own checks further down.
d.POLISH_PER_BUILD = 0
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
# Every turn writes something, unless a check says the Mind wrote nothing.
real_folder_print = d.folder_print
prints, scripted = [0], []
d.folder_print = lambda m: scripted.pop(0) if scripted else (prints.__setitem__(0, prints[0] + 1), prints[0])[1]
chats = []
d.new_chat = lambda: (chats.append(len(sent)), (True, ""))[1]

failed = 0


def unit_lines(name):
    with open(os.path.join(HERE, name), encoding="utf-8") as f:
        return [line.strip() for line in f]


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
d.write_state(d.LAST, specs[1]["id"])
replies[:] = [("FINISHED", "answered")]
chats.clear()
d.main()
check("a build resumed straight after itself keeps its conversation",
      chats == [] and sent[-1].startswith("Please keep going"), repr(chats))
d.write_state(d.DONE, set())
d.write_state(d.STARTED, {specs[0]["id"]})
os.makedirs(os.path.join(d.PROJECTS, specs[0]["folder"]), exist_ok=True)
open(os.path.join(d.PROJECTS, specs[0]["folder"], "PLAN.md"), "w").close()
d.write_state(d.LAST, "polish:" + specs[1]["id"])
replies[:] = [("FINISHED", "answered")]
chats.clear()
before = len(sent)
d.main()
check("a build resumed after another mission gets a fresh conversation and is told again what it is",
      chats == [before] and sent[-1].startswith("Please pick " + specs[0]["name"] + " up again")
      and specs[0]["details"] in sent[-1] and "{" not in sent[-1], sent[-1])

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

d.write_state(d.DONE, set())
d.write_state(d.STARTED, set())
scripted[:] = [-1, -1, -2]  # at the start, after turn 1 (nothing written), after turn 2
replies[:] = [("FINISHED\n\nI wrote the save/load code and the check came back clean.", "answered"),
              ("Done for real this time.\n\nFINISHED", "answered")]
before = len(sent)
d.main()
log = [json.loads(line) for line in open(d.LOG, encoding="utf-8")]
refused = [e for e in log if e.get("event") == "finished refused" and e.get("why") == "nothing in the folder changed"]
check("a FINISHED over a folder nothing was written to is refused, and the Mind is told so",
      refused and len(sent) - before == 2 and "none of its code changed" in sent[-1], sent[-1])
check("a FINISHED once something was written finishes the round", specs[0]["id"] in d.finished_missions())

root = os.path.join(d.PROJECTS, "printcheck")
os.makedirs(os.path.join(root, ".git"), exist_ok=True)
m = {"fields": {"folder": "printcheck"}}
open(os.path.join(root, "a.js"), "w").write("1")
first = real_folder_print(m)
open(os.path.join(root, ".git", "x"), "w").write("hidden")
same = real_folder_print(m) == first
open(os.path.join(root, "IDEAS.md"), "w").write("- [x] done")
open(os.path.join(root, "a.js.bak"), "w").write("1")
open(os.path.join(root, "a.js"), "w").write("1")
notes_only = real_folder_print(m) == first
open(os.path.join(root, "a.js"), "w").write("12")
check("the folder print changes when code is written, not for notes, backups, rewrites or hidden entries",
      same and notes_only and real_folder_print(m) != first
      and real_folder_print({"fields": {"folder": "missing"}}) == ())

# ── The showcase ──────────────────────────────────────────────────────────────────────────────


class Page(html.parser.HTMLParser):
    """Every tag, every attribute that loads something, and the text, of a frame page."""

    def __init__(self):
        super().__init__()
        self.tags, self.loads, self.handlers, self.text = [], [], [], []

    def handle_starttag(self, tag, attrs):
        self.tags.append(tag)
        for k, v in attrs:
            if k in ("src", "href", "action", "data", "poster"):
                self.loads.append((tag, v))
            if k.startswith("on"):
                self.handlers.append((tag, k, v))

    def handle_data(self, data):
        self.text.append(data)


def frame():
    with open(os.path.join(d.SHOWCASE_DIR, "frame.html"), encoding="utf-8") as f:
        page = Page()
        page.feed(f.read())
    return page


TEMPLATE_TAGS = {"html", "head", "meta", "title", "style", "body", "header", "div", "span", "b", "p", "script",
                 "iframe"}
os.makedirs(os.path.join(d.PROJECTS, specs[0]["folder"]), exist_ok=True)
open(os.path.join(d.PROJECTS, specs[0]["folder"], "index.html"), "w").write("<!doctype html><title>game</title>")
hostile = 'Done: <img src=x onerror="alert(1)"> it\'s "quoted" & <script>alert(2)</script></header><b>'
d.write_frame('Star"fall <i>x</i>', "turn 3 <b>", hostile, specs[0]["folder"])
page = frame()
text = "".join(page.text)
check("the frame page shows a reply with HTML and quotes as text, never as markup",
      set(page.tags) <= TEMPLATE_TAGS and page.tags.count("iframe") == 1 and "img" not in page.tags
      and page.tags.count("script") == 1 and page.handlers == [("body", "oncontextmenu", "return false")]
      and '<img src=x onerror="alert(1)">' in text and 'Star"fall <i>x</i>' in text and "turn 3 <b>" in text,
      repr((page.tags, page.handlers)))
page_url = "file://" + os.path.join(os.path.realpath(d.PROJECTS), specs[0]["folder"], "index.html")
check("the frame page loads the game's own index.html, in its demo mode, and its version file, nothing else",
      sorted(page.loads) == [("iframe", page_url + "?demo=1")], repr(page.loads))
with open(os.path.join(d.SHOWCASE_DIR, "frame.html"), encoding="utf-8") as f:
    raw = f.read()
version = open(os.path.join(d.SHOWCASE_DIR, "version.js"), encoding="utf-8").read()
check("the frame page reloads itself when the Director writes a new version",
      re.fullmatch(r'showcaseVersion\("[0-9a-f]{16}"\);\n', version) is not None
      and version.split('"')[1] in raw and '"version.js?"' in raw, version)
check("the game runs sandboxed: scripts and its own storage, no downloads, pop-ups, dialogs or navigating away",
      'sandbox="allow-scripts allow-same-origin"' in raw and "Content-Security-Policy" in raw)
d.write_frame("Missing", "turn 1", "", "no-such-game")
check("a game with no page yet shows a line instead of a frame", "iframe" not in frame().tags)

check("the caption is the reply's first sentence, plain text, with markup and paths taken out",
      d.caption_summary("## Step 3\n\n**Done:** I wrote the `collision` code in ~/Projects/starfall/game.js. "
                        "Next comes the score.\n\nFINISHED") == "Step 3 Done: I wrote the collision code in."
      and d.caption_summary("- [x] Added <b>sound</b> &amp; [a mute button](http://x/y). Then more.")
      == "Added sound & a mute button."
      and d.caption_summary("```js\nlet x = 1;\n```\nThe page loads clean now!") == "The page loads clean now!"
      and d.caption_summary("FINISHED") == "" and d.caption_summary("") == "",
      repr([d.caption_summary("## Step 3\n\n**Done:** I wrote the `collision` code in ~/Projects/starfall/game.js.")]))
long = d.caption_summary("word " * 80 + ".")
check("the caption is never longer than 120 characters", len(long) <= 120 and long.endswith("…"), repr(long))

# Nothing outside ~/Projects/<mission folder>/index.html is ever put in the showcase.
outside = os.path.join(tmp, "private.html")
open(outside, "w").write("secret")
os.makedirs(os.path.join(d.PROJECTS, "linked"), exist_ok=True)
os.symlink(outside, os.path.join(d.PROJECTS, "linked", "index.html"))
os.symlink(tmp, os.path.join(d.PROJECTS, "via-link"))
os.makedirs(os.path.join(tmp, "escaped"), exist_ok=True)
open(os.path.join(tmp, "escaped", "index.html"), "w").write("x")
refused = [f for f in ("../escaped", "..", "/etc", "linked", "via-link", "Starfall", "a/b", "", None, "x" * 80)
           if d.game_page(f) is not None]
check("the showcase opens nothing outside ~/Projects/<mission folder>/index.html: no climbing out, no links",
      refused == [] and d.game_page(specs[0]["folder"]) == page_url[len("file://"):], repr(refused))
for bad in ("../escaped", "linked"):
    d.write_frame("x", "y", "z", bad)
    check(f"a frame for the folder {bad!r} loads nothing", frame().loads == [], repr(frame().loads))
check("every mission's folder is one the showcase can open",
      all(d.FOLDER.match(s["folder"]) for s in specs), repr([s["folder"] for s in specs]))
with open(os.path.join(HERE, "showcase-browser"), encoding="utf-8") as f:
    browser = f.read()
# What it runs, its comments left out.
browser = "\n".join(line for line in browser.splitlines() if not line.lstrip().startswith("#"))
flags = re.findall(r"--[a-z-]+", browser)
check("the showcase browser opens only the frame page it is given",
      browser.count("--app=") == 1 and '--app="file://$frame"' in browser and "http" not in
      browser.replace('--proxy-server="http://127.0.0.1:9"', "") and d.SHOWCASE_BROWSER.endswith("showcase-browser"))
check("the showcase browser has no network, a throwaway profile, no extensions and no debugging port",
      {"--proxy-server", "--host-resolver-rules", "--proxy-bypass-list", "--user-data-dir", "--disable-extensions",
       "--disable-component-extensions-with-background-pages", "--disable-background-networking"} <= set(flags)
      and '"MAP * ~NOTFOUND"' in browser and "mktemp -d" in browser and 'rm -rf "$profile"' in browser
      and "remote-debugging" not in browser and "load-extension" not in browser)
with open(os.path.join(HERE, "yantrik-live-showcase.service"), encoding="utf-8") as f:
    showcase_unit = [line.strip() for line in f]
check("the showcase's service has no IP either",
      "IPAddressDeny=any" in showcase_unit and not any(x.startswith("IPAddressAllow") for x in showcase_unit)
      and "User=yantrik" in showcase_unit)

# While a mission runs the showcase shows its game; between missions it goes round the finished ones.
d.write_state(d.DONE, set())
d.write_state(d.STARTED, set())
seen = []
real_show = d.show_mission


def watching_show(m, turn=0, reply="", clean=None):
    real_show(m, turn, reply, clean)
    with open(os.path.join(d.SHOWCASE_DIR, "version.js"), encoding="utf-8") as f:
        seen.append((m["id"], turn, frame().loads, "".join(frame().text), f.read()))


d.show_mission = watching_show
replies[:] = [("Step one is in. More to come.", "answered"), ("FINISHED", "answered")]
d.main()
d.show_mission = real_show
game = [("iframe", page_url + "?demo=1")]
check("the showcase shows the running mission's game, with a new version after every turn",
      [x[:3] for x in seen] == [(specs[0]["id"], t, game) for t in (0, 1, 2)]
      and "Step one is in." in seen[1][3] and "turn 1" in seen[1][3] and specs[0]["title"] in seen[1][3]
      and len({x[4] for x in seen}) == 3, repr([x[:3] for x in seen]))
check("once the mission ends the showcase is handed back to the cycle", d.read_state(d.SHOWING, {}) == {})
for s in specs[:3]:
    os.makedirs(os.path.join(d.PROJECTS, s["folder"]), exist_ok=True)
    open(os.path.join(d.PROJECTS, s["folder"], "index.html"), "w").write("x")
d.write_state(d.DONE, {specs[0]["id"], specs[2]["id"], specs[3]["id"]})  # specs[3] has no page
builds = d.load_builds()
shown, index = [], 0
for _ in range(3):
    index = d.cycle_step(builds, index)
    shown.append(frame().loads[0][1])
want = ["file://" + os.path.join(os.path.realpath(d.PROJECTS), specs[i]["folder"], "index.html") + "?demo=1"
        for i in (0, 2, 0)]
check("between missions the showcase goes round the finished games with a page, in their demo mode",
      shown == want, repr(shown))
d.write_state(d.DONE, set())
d.cycle_step(builds, 0)
check("with no game finished the showcase says so rather than showing nothing", frame().loads == [])
check("the cycle is a service of its own, so the Director's oneshot still ends with its mission",
      "Restart=always" in showcase_unit and "ExecStart=/usr/bin/python3 /opt/yantrik-live/director/director.py showcase"
      in showcase_unit and "Type=oneshot" in unit_lines("yantrik-live-director.service"))

# The showcase service: opens the window once, goes round the games, hands the screen to a mission.
class Stop(Exception):
    pass


class Browser:
    def poll(self):
        return None


launched, placed, ticks = [], [], [0]
d.write_state(d.DONE, {specs[0]["id"], specs[2]["id"]})
d.write_state(d.SHOWING, {})
real_popen, real_place, real_sleep = d.subprocess.Popen, d.place_windows, d.time.sleep
d.subprocess.Popen = lambda argv: (launched.append(argv), Browser())[1]
d.place_windows = lambda state, showcase_front=False: placed.append(showcase_front)
clock = [1000.0]
real_time, d.time.time = d.time.time, lambda: clock[0]


def tick(_):
    ticks[0] += 1
    clock[0] += d.POLL_SECS if ticks[0] != 3 else d.CYCLE_SECS
    if ticks[0] == 5:
        d.write_state(d.SHOWING, {"mission": "polish:x", "at": clock[0]})
    if ticks[0] == 8:
        raise Stop


d.time.sleep = tick
shown = []
real_cycle = d.cycle_step
d.cycle_step = lambda b, i: (shown.append(i), real_cycle(b, i))[1]
try:
    d.main(["showcase"])
except Stop:
    pass
d.subprocess.Popen, d.place_windows, d.time.sleep, d.time.time, d.cycle_step = \
    real_popen, real_place, real_sleep, real_time, real_cycle
check("the showcase service opens one window on the frame page and goes round the games until a mission shows",
      launched == [[d.SHOWCASE_BROWSER, os.path.join(d.SHOWCASE_DIR, "frame.html")]] and shown == [0, 1]
      and placed == [True], repr((launched, shown, placed)))

# Builds and polish rounds take turns.
d.POLISH_PER_BUILD = 2
for k in (d.DONE, d.STARTED, d.POLISHED, d.POLISH_DUE, d.LAST):
    if os.path.exists(k):
        os.remove(k)
order = []
for _ in range(8):
    replies[:] = [("FINISHED", "answered")]
    d.main()
    order.append(next(("polish " if sent[-1].startswith("Please make") else "build ") + s["id"] for s in specs
                      if re.search(r"\b%s\b" % re.escape(s["name"]), sent[-1])))
check("builds and polish rounds are interleaved, each finished build followed by its polish rounds",
      order == ["build " + specs[0]["id"], "polish " + specs[0]["id"], "polish " + specs[0]["id"],
                "build " + specs[1]["id"], "polish " + specs[1]["id"], "polish " + specs[0]["id"],
                "build " + specs[2]["id"], "polish " + specs[2]["id"]], repr(order))
d.POLISH_PER_BUILD = 0

# Every game's first polish idea is its demo mode.
check("every brief asks for the demo mode as the first idea in IDEAS.md",
      all(d.DEMO_IDEA in d.prompt(t, d.build(specs[0])) for t in (d.BRIEF, d.KEEP_GOING, d.RESUME))
      and "?demo=1" in d.DEMO_IDEA and "the first of them " + d.DEMO_IDEA in d.BRIEF)
ideas = os.path.join(d.PROJECTS, specs[0]["folder"], "IDEAS.md")
open(ideas, "w").write("# Ideas for Starfall\n\n- [x] Sound effects\n- [ ] Power-ups\n")
d.write_state(d.DONE, {s["id"] for s in specs})
replies[:] = [("FINISHED", "answered")]
d.write_state(d.POLISHED, {s["id"]: 1 for s in specs[1:]})
d.main()
got = open(ideas, encoding="utf-8").read().splitlines()
check("a polish round puts the demo idea at the head of IDEAS.md, unticked, ahead of the game's own ideas",
      got[:3] == ["# Ideas for Starfall", "", "- [ ] " + d.DEMO_IDEA[0].upper() + d.DEMO_IDEA[1:]]
      and got[3:] == ["- [x] Sound effects", "- [ ] Power-ups"] and sent[-1].startswith("Please make Starfall"),
      repr(got))
m0 = d.polish(d.build(specs[0]))
check("the demo idea is added once, and not over a demo the game already has",
      not d.ensure_demo_idea(m0) and open(ideas).read().count("demo=1") == 1)
open(ideas, "w").write("- [x] A demo mode on ?demo=1\n")
check("a game whose demo is ticked gets no second one", not d.ensure_demo_idea(m0))
os.remove(ideas)
check("a game with no IDEAS.md gets one, the demo first", d.ensure_demo_idea(m0)
      and open(ideas).read().startswith("- [ ] A self-playing demo mode"))
os.remove(ideas)
os.symlink(outside, ideas)
check("an IDEAS.md that is a link is left alone", not d.ensure_demo_idea(m0) and open(outside).read() == "secret")

# Mind View left, the showcase right, and the showcase never left behind Mind View.
view = {"actions": [{"name": "focus_window", "parameters": {"properties": {"title": {}}}},
                    {"name": "snap_window", "parameters": {"properties": {
                        "title": {"type": "string"}, "region": {"enum": ["left-half", "right-half", "top-half"]}}}}]}
snap = d.snap_action(view)
check("the shell's snap action is found from what it publishes, and its halves chosen",
      d.snap_args(snap, "Mind View", "left") == ["snap_window", "title=Mind View", "region=left-half"]
      and d.snap_args(snap, d.SHOWCASE_TITLE, "right") == ["snap_window", "title=Yantrik Live showcase", "region=right-half"]
      and d.snap_action({"actions": view["actions"][:1]}) is None, repr(snap))
acts, ran = [], []
real_act, d.act = d.act, lambda *a: (acts.append(a), (True, ""))[1]
d.subprocess.run = lambda argv, **k: (ran.append((argv, (k.get("env") or {}).get("WAYLAND_DISPLAY"))),
                                      Done(json.dumps(view)))[1]
d.place_windows({"mind_view": {"display": "wayland-1"}})
d.subprocess.run = real_run
check("Mind View is snapped left and the showcase right, and the showcase is brought forward last",
      acts == [("snap_window", "title=Mind View", "region=left-half"),
               ("snap_window", "title=Yantrik Live showcase", "region=right-half"),
               ("focus_window", "title=Yantrik Live showcase")], repr(acts))
check("the Mind's terminal fills Mind View, so the empty-view picture is not seen round it",
      ran[-1] == (["wlrctl", "toplevel", "maximize", "app_id:foot"], "wayland-1"), repr(ran))
acts.clear()
ran.clear()
d.subprocess.run = lambda argv, **k: (ran.append(argv), Done(json.dumps({"actions": [{"name": "taint_snapshot"}]})))[1]
snapped = d.place_windows({"mind_view": {"display": "../wayland-0"}})
d.subprocess.run = real_run
check("without a snap action Mind View stays in front during a mission, and no display but Mind View's is touched",
      not snapped and acts == [] and not any(a[0] == "wlrctl" for a in ran), repr((acts, ran)))
d.subprocess.run = lambda argv, **k: Done(json.dumps({"actions": []}))
d.place_windows(None, showcase_front=True)
d.subprocess.run = real_run
check("without a snap action the showcase is brought in front between missions",
      acts == [("focus_window", "title=Yantrik Live showcase")], repr(acts))
d.act = real_act

with open(os.path.join(HERE, "yantrik-live-director.service"), encoding="utf-8") as f:
    unit = [line.strip() for line in f]
check("the Director's service has no IP, so a page the Mind wrote reaches no network when checked",
      "IPAddressDeny=any" in unit and not any(line.startswith("IPAddressAllow") for line in unit))

sys.exit(1 if failed else 0)
