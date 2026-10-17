#!/usr/bin/env python3
"""The live instance's Director: gives its Mind long work, one turn at a time, on stream.

Without it the live machine sits on whatever screen it was left on. This is the person at the
keyboard, nothing more: it types a mission into the Lens through `yos act shell send_message`,
the same path a person's typing takes, waits until the Mind has finished its turn, and then says
"keep going" with the mission's next instruction, until the Mind says it is finished, the
mission's turn or time limit is reached, or something needs a person.

It never answers an approval card and never changes the Mind's mode. A card waiting means a
person is needed; the Director logs that, leaves the card up for whoever is watching, and waits.

It does check the work. A mission may name a page (`check_page`), and after every turn the
Director loads it with `check-page`, the same check the Mind is told to run. A page that throws
puts its errors at the head of the next prompt, and a FINISHED is not taken while it throws. On
561 the Mind ticked all eight Starfall steps, and said every feature was in, over a game.js that
did not parse (2 Oct 2026). A tick is the Mind's word; the page loading is the evidence.

It never runs dry. Missions are builds, taken in order; a mission that stopped (a card, a limit,
the screen locked) is picked up again with "keep going", not started over. Each build ends with an
IDEAS.md of improvements, and once every build is finished the Director goes round them in
polish rounds, one improvement a round, the project polished longest ago first. After Starfall the
live machine sat idle for two weeks with every mission finished (2 to 15 Oct 2026).

It puts the work on screen. The Director owns a showcase window beside Mind View: a locked-down
Chromium (showcase-browser, run by yantrik-live-showcase.service) that shows a frame page the
Director writes, with a caption bar (the mission, the turn, the Mind's latest reply in one line)
over the game being built, reloaded after every turn. Between missions the showcase service goes
round the finished games in their self-playing demo mode. On 561 the stream showed only the Mind's
terminal, half the screen was empty, and viewers never saw a game it built (17 Oct 2026). The
showcase is the Director's, never the Mind's: the Mind's own browser still refuses file:// and
new local addresses (check-page says why), and nothing here changes that.

Builds and polish rounds are interleaved: each finished build is followed by POLISH_PER_BUILD
polish rounds before the next build starts, so the finished games keep getting better while new
ones are made, and the first idea every game gets in polish is its demo mode (DEMO_IDEA).

Runs on the live instance (VM 561) as the desktop's account, from yantrik-live-director.service.
Missions are in missions.json beside it. Progress goes to ~/director/log.jsonl, one line a turn.
Stdlib only; the image ships no Python packages for it.
"""
import difflib
import hashlib
import html
import json
import os
import re
import subprocess
import sys
import time
import urllib.parse

HERE = os.path.dirname(os.path.abspath(__file__))
YOS = "/opt/yantrik/bin/yos"
STATE_DIR = os.path.expanduser("~/director")
LOG = os.path.join(STATE_DIR, "log.jsonl")
DONE = os.path.join(STATE_DIR, "done.json")
STARTED = os.path.join(STATE_DIR, "started.json")
POLISHED = os.path.join(STATE_DIR, "polished.json")
# How many polish rounds are still owed before the next build: set after every finished build.
POLISH_DUE = os.path.join(STATE_DIR, "polish-due.json")
# The mission run last, so a build picked up after others knows its conversation is gone.
LAST = os.path.join(STATE_DIR, "last.json")
# What the showcase is showing: a mission, or nothing (the showcase service cycles the games).
SHOWING = os.path.join(STATE_DIR, "showing.json")
CHECK_PAGE = os.path.join(HERE, "check-page")
PROJECTS = os.path.expanduser("~/Projects")

POLL_SECS = 5
# A beat between turns, so someone watching sees the result of one step before the next begins.
BETWEEN_TURNS_SECS = 20
# A turn that has not ended in this long is stuck; the mission stops rather than piling on.
TURN_LIMIT_SECS = 20 * 60
FINISHED = "FINISHED"
# A polish round is one improvement: shorter limits than a build.
POLISH_TURNS = 10
POLISH_HOURS = 1
# Polish rounds after each finished build, before the next build begins.
POLISH_PER_BUILD = 2

# The showcase: the frame page the Director writes, and the window that shows it.
SHOWCASE_DIR = "/opt/yantrik-live/showcase"
SHOWCASE_TITLE = "Yantrik Live showcase"
SHOWCASE_BROWSER = os.path.join(HERE, "showcase-browser")
# Between missions, each finished game is shown this long in its demo mode.
CYCLE_SECS = 75
# A mission whose showing has not been refreshed in this long has died with the Director; the
# showcase goes back to cycling the games rather than showing a stopped one for ever.
SHOWING_STALE_SECS = TURN_LIMIT_SECS + 10 * 60
CAPTION_CHARS = 120
# A mission folder, as missions.json may name one: nothing that could climb out of ~/Projects.
FOLDER = re.compile(r"[a-z0-9][a-z0-9-]{0,63}\Z")

# The first improvement every game gets, ahead of its other ideas: something to watch on the
# stream between missions, when nobody is playing it.
DEMO_IDEA = ("a self-playing demo mode: when opened with ?demo=1 it plays itself (attract mode), "
             "visibly, and keeps the score/state changing")

# What every mission is told, filled from its fields in missions.json. One wording for all of them,
# so a lesson learned on one build (agent_run, not the Terminal app; a tick only over a clean
# check) reaches every build.
TERMINAL = ("Do all the work in your own terminal, through the shell's agent_run action: that is how "
            "you make the folder, write each file and look at what you wrote. Do not use the Terminal app.")
BRIEF = (
    "Please build {what} from scratch, in a folder called {folder} inside the Projects folder in your "
    "home folder. {details} Use plain HTML, CSS and JavaScript only, all in that one folder, with no "
    "libraries and no internet. " + TERMINAL + " Start by writing a short plan into a file called "
    "PLAN.md in that folder: a checklist of about {steps} steps, from an empty page to {plan}. Then work "
    "through the plan one step at a time: write or change the files, then check the page by running "
    "`{check}` through agent_run. It loads the page in a headless browser and prints any error the page "
    "throws; no output means it loaded clean. Fix every error it shows. Tick a step off in PLAN.md only "
    "when the code for it is really there and the check is clean: a tick is a promise to the people "
    "watching. People are watching the screen, so keep each step small and visible. When every step is "
    "ticked and the check is clean, write IDEAS.md in the folder: five improvements you would make next, "
    "each an unticked checklist line, the first of them " + DEMO_IDEA + ". Then say FINISHED on its own line.")
KEEP_GOING = (
    "Please keep going on {name}. Read PLAN.md in the {folder} folder through the shell's agent_run "
    "action. If the folder or the plan does not exist yet, make them first, as the brief said. Otherwise "
    "take the first step that is not ticked, do it, and check the page by running `{check}` through "
    "agent_run. Fix any error it prints, and tick the step off in PLAN.md only when its code is really "
    "there and the check is clean. If every step is ticked and the check is clean, write IDEAS.md if it "
    "is not there yet (five improvements, each an unticked checklist line, the first of them " + DEMO_IDEA
    + "), then say FINISHED on its own line.")
# A build picked up after other missions ran in between: on a fresh conversation, which has not
# seen the brief, so it says again what the build is. "Keep going" alone had the Mind build Orrery
# from its name (15 Oct 2026).
RESUME = (
    "Please pick {name} up again where it was left: {what}, in the {folder} folder inside the Projects "
    "folder in your home folder. {details} " + TERMINAL + " Read PLAN.md there, take the first step "
    "that is not ticked, do it, and check the page by running `{check}` through agent_run. Fix any error "
    "it prints, and tick the step off in PLAN.md only when its code is really there and the check is "
    "clean. If every step is ticked and the check is clean, write IDEAS.md if it is not there yet (five "
    "improvements, each an unticked checklist line, the first of them " + DEMO_IDEA + "), then say "
    "FINISHED on its own line.")
FIX_FIRST = (
    "Before anything else on {name}: the page throws an error when it loads. The Director just loaded it "
    "in a headless browser and it said:\n\n{errors}\n\nFix that first, through the shell's agent_run "
    "action, and check it again by running `{check}`. Then read the code and {notes} side by side, and "
    "untick any line whose code is not really there yet: a line is ticked only when it works. Then carry "
    "on with the first line that is not ticked.")
NOTHING_CHANGED = (
    "You said FINISHED on {name}, but nothing in the {folder} folder has changed since this round "
    "started: none of its code changed (ticking a line or copying a file to a backup is not a "
    "change). Reading files is not the step. Do the step now through the shell's "
    "agent_run action, writing the code into the files, check the page by running `{check}`, tick the "
    "line only when its code is really there, and only then say FINISHED on its own line.")
POLISH = (
    "Please make {name} better by one step. It is in the {folder} folder inside the Projects folder in "
    "your home folder. Read IDEAS.md there through the shell's agent_run action. If it is missing, or "
    "every idea in it is ticked, first add three new ideas for improving {name}, each an unticked "
    "checklist line. Take the first idea that is not ticked and do it, keeping the change small and "
    "visible. " + TERMINAL + " Check the page by running `{check}` through agent_run and fix any error "
    "it prints. Tick the idea only when its code is really there and the check is clean. Then say "
    "FINISHED on its own line.")
POLISH_KEEP_GOING = (
    "Please keep going on the {name} idea you are working on, from IDEAS.md in the {folder} folder. "
    "Check the page by running `{check}` through agent_run and fix any error it prints. Tick the idea "
    "only when its code is really there and the check is clean, then say FINISHED on its own line.")


def log(**entry):
    entry["at"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    os.makedirs(STATE_DIR, exist_ok=True)
    with open(LOG, "a", encoding="utf-8") as f:
        f.write(json.dumps(entry, ensure_ascii=False) + "\n")
    print(json.dumps(entry, ensure_ascii=False), flush=True)


def shell_state():
    """The shell's describe state, or None when the desktop does not answer (restarting, locked)."""
    try:
        out = subprocess.run([YOS, "describe", "shell"], capture_output=True, text=True, timeout=30).stdout
    except (OSError, subprocess.TimeoutExpired):
        return None
    start, end = out.find("\n{"), out.rfind("\n}")
    if start < 0 or end < 0:
        return None
    try:
        return json.loads(out[start + 1 : end + 2])
    except json.JSONDecodeError:
        return None


def last_reply(state):
    turns = state.get("conversation") or []
    return turns[-1] if turns else None


def whole_text(turn):
    """A turn's whole text. `describe` clips each message to 600 characters and marks it
    `clipped`; the Mind ends its reply with FINISHED, which is the part the clip cuts. On 561 every
    Orrery reply ran past the clip and the mission never finished (15 Oct 2026). `read_message`
    returns the rest; if it cannot, the clipped text is all there is."""
    if not turn:
        return ""
    text = turn.get("text") or ""
    if not turn.get("clipped") or turn.get("index") is None:
        return text
    try:
        out = subprocess.run([YOS, "act", "shell", "read_message", f"index={turn['index']}"],
                             capture_output=True, text=True, timeout=30).stdout
        start = out.find("\n{")
        whole, _ = json.JSONDecoder().raw_decode(out[start + 1:]) if start >= 0 else ({}, 0)
    except (OSError, subprocess.TimeoutExpired, ValueError):
        return text
    return whole.get("text") or text if isinstance(whole, dict) else text


def said_finished(reply):
    """FINISHED on a line of its own, as every brief asks, bare or in markdown (`**FINISHED**`).
    Not the word inside a sentence: on 561 a polish round was taken as done over "I'm not going to
    write FINISHED on a line for work that isn't there" (16 Oct 2026)."""
    return re.search(rf"(?m)^[^A-Za-z0-9]*{FINISHED}[^A-Za-z0-9]*$", reply) is not None


def mind_idle(state):
    """The Mind has finished its turn: not thinking, and the last word is its own, complete."""
    last = last_reply(state)
    return (not state.get("thinking")) and last is not None and last.get("role") == "assistant" \
        and not last.get("streaming")


def act(action, *args):
    """One shell action, as `yos act shell` takes it. Best effort: (whether it went, what it said)."""
    try:
        done = subprocess.run([YOS, "act", "shell", action, *args], capture_output=True, text=True, timeout=30)
    except (OSError, subprocess.TimeoutExpired) as why:
        return False, str(why)[:300]
    return done.returncode == 0, (done.stdout + done.stderr).strip()[:300]


def show_mind_view():
    """Put Mind View in front, so the stream shows the Mind's work, with the showcase beside it.
    A person can minimise it or open something over it between turns; the person watching should
    not see an idle desktop while the Mind builds. Best effort: Mind View may not exist yet before
    the Mind opens an app.

    Never while a card is waiting: the card is drawn in the shell's Mind panel, and Mind View
    maximised covers the whole shell. On 561 a waiting agent_run card sat behind it and the
    person at the machine looked for it and could not see it (2 Oct 2026)."""
    state = shell_state()
    if state is not None and state.get("pending_approvals"):
        return
    act("focus_window", "title=Mind View")
    place_windows(state)


def snap_action(view):
    """The shell's snap action, from what `describe shell` publishes: (its name, the argument that
    names the window, the argument that says where, the values that one takes), or None when the
    shell publishes none. Looked up rather than assumed, so a shell that renames it or has none
    costs a log line, not a failed mission."""
    for a in (view or {}).get("actions") or []:
        name = a.get("name") or ""
        if not re.search(r"snap(?!shot)", name.lower()):
            continue
        props = (a.get("parameters") or {}).get("properties") or {}
        title = next((k for k in props if "title" in k or k == "window"), None)
        where = next((k for k in props if k != title), None)
        if title and where:
            return name, title, where, [str(v) for v in (props[where] or {}).get("enum") or []]
    return None


def snap_args(snap, title, side):
    """The `yos act shell` arguments that put the window called `title` on `side` (left or right)
    half of the screen, or None when the snap action offers no such place."""
    name, title_arg, where_arg, values = snap
    if values:
        value = next((v for v in values if side in v.lower() and "half" in v.lower()), None) \
            or next((v for v in values if v.lower() == side), None)
    else:
        value = f"{side}-half"
    return None if value is None else [name, f"{title_arg}={title}", f"{where_arg}={value}"]


UNSNAPPED = []


def place_windows(state=None, showcase_front=False):
    """Mind View on the left half of the screen and the showcase on the right, then the showcase
    focused, so focusing Mind View never leaves it covered. Returns whether the windows were
    snapped. Without a snap action they stay where the compositor put them, overlapping: then the
    showcase is brought in front only when `showcase_front` asks (between missions, when it is
    the thing to watch), and during a mission Mind View stays in front."""
    try:
        out = subprocess.run([YOS, "describe", "shell", "--json"], capture_output=True, text=True, timeout=30).stdout
        snap = snap_action(json.loads(out))
    except (OSError, subprocess.TimeoutExpired, ValueError):
        snap = None
    snapped = False
    if snap is None:
        # Said once a run, not every turn.
        if not UNSNAPPED:
            UNSNAPPED.append(True)
            log(event="windows not snapped", why="the shell publishes no snap action")
    else:
        for title, side in (("Mind View", "left"), (SHOWCASE_TITLE, "right")):
            args = snap_args(snap, title, side)
            snapped = bool(args) and act(*args)[0] and (snapped or side == "left")
    if snapped or showcase_front:
        act("focus_window", f"title={SHOWCASE_TITLE}")
    fill_mind_view(state)
    return snapped


def fill_mind_view(state):
    """Maximise the Mind's terminals inside Mind View. A terminal smaller than Mind View left the
    picture an empty Mind View shows (config/labwc-mind/empty.png, "Apps your companion opens
    appear here, so they stay out of your way.") showing round it, on stream. The picture is right
    for a person's empty Mind View; on 561 the terminal should simply fill it. Mind View is a
    nested labwc on its own display, which `describe shell` names; the Director is its account."""
    display = ((state or {}).get("mind_view") or {}).get("display")
    if not isinstance(display, str) or not re.fullmatch(r"wayland-[0-9]+", display):
        return
    try:
        subprocess.run(["wlrctl", "toplevel", "maximize", "app_id:foot"], capture_output=True, timeout=10,
                       env=dict(os.environ, WAYLAND_DISPLAY=display))
    except (OSError, subprocess.TimeoutExpired):
        pass


def caption_summary(reply):
    """The Mind's reply in one line for the caption: its first sentence, plain text, no longer
    than CAPTION_CHARS. Code blocks, HTML, markdown and paths are taken out; what is left is
    still only text, and the frame page escapes it again."""
    text = html.unescape(re.sub(r"```.*?(```|\Z)", "\n", reply or "", flags=re.S))
    text = re.sub(r"<[^>]*>?", " ", text)
    lines = []
    for line in text.splitlines():
        line = re.sub(r"^\s*(#{1,6}|>|[-*+]|\d+[.)])\s+", "", line)
        line = re.sub(r"^\s*\[[ xX]\]\s*", "", line)
        if line.strip() and not said_finished(line):
            lines.append(line.strip())
    text = " ".join(lines)
    text = re.sub(r"!?\[([^\]]*)\]\([^)]*\)", r"\1", text)
    text = text.replace("`", " ")
    # A path or an address is any word with a slash in it, whatever punctuation follows it.
    text = re.sub(r"[^\s]*/[^\s]*?(?=[.,;:!?)\]]*(\s|$))", " ", text)
    text = re.sub(r"\*+|~~|(?<!\w)_+|_+(?!\w)", "", text)
    text = re.sub(r"[\x00-\x1f\x7f-\x9f]", " ", text)
    text = re.sub(r"\s+([.,;:!?])", r"\1", " ".join(text.split()))
    text = re.sub(r"(\w)\s*:\s*([.!?])", r"\1\2", text)
    first = re.match(r"(.+?[.!?])(\s|$)", text)
    text = first.group(1) if first else text
    if len(text) > CAPTION_CHARS:
        text = text[: CAPTION_CHARS - 1].rsplit(" ", 1)[0].rstrip(" ,;:") + "…"
    return text


def game_page(folder):
    """A mission's page, ~/Projects/<folder>/index.html, as a real path, or None. Nothing else is
    ever put in the showcase: not a folder name that climbs out (`..`), not an index.html that is
    a link to somewhere else, not a page that is not there."""
    if not isinstance(folder, str) or not FOLDER.match(folder):
        return None
    want = os.path.join(os.path.realpath(PROJECTS), folder, "index.html")
    page = os.path.join(PROJECTS, folder, "index.html")
    if os.path.realpath(page) != want or os.path.islink(page) or not os.path.isfile(want):
        return None
    return want


def game_url(folder, demo):
    page = game_page(folder)
    if page is None:
        return None
    return "file://" + urllib.parse.quote(page) + ("?demo=1" if demo else "")


FRAME = """<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline' file:; frame-src file:">
<title>@WINDOW@</title>
<style>
html, body { margin: 0; height: 100%; background: #0c0c14; color: #e8e8f0; font: 16px/1.35 sans-serif; overflow: hidden; }
body { display: flex; flex-direction: column; }
header { padding: 10px 16px 12px; background: #161624; border-bottom: 2px solid #6c5ce7; }
header .top { display: flex; gap: 12px; align-items: baseline; white-space: nowrap; overflow: hidden; }
header .live { background: #e5484d; color: #fff; font-weight: 700; font-size: 12px; padding: 1px 6px; border-radius: 3px; }
header b { font-size: 19px; overflow: hidden; text-overflow: ellipsis; }
header .turn { color: #a8a8c0; }
header p { margin: 6px 0 0; color: #c8c8dc; min-height: 1.35em; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
iframe { flex: 1; width: 100%; border: 0; background: #000; }
.empty { flex: 1; display: flex; align-items: center; justify-content: center; color: #8888a0; font-size: 20px; padding: 24px; text-align: center; }
</style></head>
<body oncontextmenu="return false">
<header><div class="top"><span class="live">LIVE</span><b>@TITLE@</b><span class="turn">@LINE@</span></div><p>@SUMMARY@</p></header>
@GAME@
<script>
var shown = "@VERSION@";
window.showcaseVersion = function (v) { if (v !== shown) location.reload(); };
setInterval(function () {
  var s = document.createElement("script");
  s.src = "version.js?" + Date.now();
  s.onload = s.onerror = function () { s.remove(); };
  document.head.appendChild(s);
}, 3000);
</script>
</body></html>
"""


def frame_page(title, line, summary, url, empty, version):
    """The frame page's HTML. Every word in it that came from a mission or the Mind is escaped,
    quotes too, so a reply with markup in it is shown as text and can never become markup."""
    if url:
        game = ('<iframe src="%s" sandbox="allow-scripts allow-same-origin" title="%s"></iframe>'
                % (html.escape(url, quote=True), html.escape(title, quote=True)))
    else:
        game = '<div class="empty">%s</div>' % html.escape(empty, quote=True)
    page = FRAME
    # The version last: it is hex, and nothing filled in before it can be mistaken for its marker.
    for mark, value in (("@WINDOW@", html.escape(SHOWCASE_TITLE)), ("@TITLE@", html.escape(title, quote=True)),
                        ("@LINE@", html.escape(line, quote=True)), ("@SUMMARY@", html.escape(summary, quote=True))):
        page = page.replace(mark, value, 1)
    return page.replace("@GAME@", game, 1).replace("@VERSION@", version, 1)


def write_atomically(path, text):
    with open(path + ".new", "w", encoding="utf-8") as f:
        f.write(text)
    os.replace(path + ".new", path)


def write_frame(title, line, summary, folder, demo=True, empty="Nothing to show yet."):
    """Rewrite the showcase's frame page for `folder`'s game, then its version, which the page
    polls: a new version is the page reloading itself, game and all."""
    url = game_url(folder, demo)
    version = hashlib.sha256(f"{title}|{line}|{summary}|{url}|{time.time()}".encode()).hexdigest()[:16]
    os.makedirs(SHOWCASE_DIR, exist_ok=True)
    write_atomically(os.path.join(SHOWCASE_DIR, "frame.html"), frame_page(title, line, summary, url, empty, version))
    write_atomically(os.path.join(SHOWCASE_DIR, "version.js"), 'showcaseVersion("%s");\n' % version)
    return url


def show_mission(m, turn=0, reply="", clean=None):
    """Put the mission's game in the showcase, with the caption for this turn. Never stops a
    mission: a showcase that cannot be written costs the stream, not the work."""
    line = f"turn {turn}" if turn else "starting"
    if clean is False:
        line += " · the page throws an error, being fixed"
    try:
        write_frame(m["title"], line, caption_summary(reply), m["fields"]["folder"],
                    empty=f"{m['fields']['name']} has no page yet: the Mind is starting on it.")
        write_state(SHOWING, {"mission": m["id"], "at": time.time()})
    except OSError as why:
        log(mission=m["id"], event="showcase not written", why=str(why)[:200])


def new_chat():
    """Start the Mind on a fresh conversation, so a mission begins from its brief and not from
    the day's history. On 561 the Mind answered nine "keep going" turns in a second each with
    "every command needs your approval", calling nothing, while a session rule allowed every
    command: earlier missions' lapsed cards were still in the conversation (16 Oct 2026).
    Best effort: a desktop that will not start one leaves the old conversation, which only costs
    what it cost before."""
    done = subprocess.run([YOS, "act", "shell", "new_chat"], capture_output=True, text=True, timeout=60)
    return done.returncode == 0, (done.stdout + done.stderr).strip()[:300]


def same_answer(a, b):
    """Whether two replies are the same answer: the Mind saying again what it just said, give or
    take a word ("I can't make progress" / "I still can't make progress"). Only the opening is
    compared, where the answer is; a long reply that starts alike and goes on differently is a
    different turn."""
    a, b = " ".join(a.split())[:400], " ".join(b.split())[:400]
    return bool(a) and bool(b) and difflib.SequenceMatcher(None, a, b).ratio() >= 0.9


def send(text):
    done = subprocess.run([YOS, "act", "shell", "send_message", f"text={text}"],
                          capture_output=True, text=True, timeout=60)
    return done.returncode == 0, (done.stdout + done.stderr).strip()[:300]


def wait_for_turn(sent_at):
    """Wait until the Mind answers what was just sent. Returns (state, why it stopped waiting,
    whether a card rose during the turn)."""
    seen_busy = False
    card_seen = False
    while True:
        time.sleep(POLL_SECS)
        state = shell_state()
        if state is None:
            if time.time() - sent_at > TURN_LIMIT_SECS:
                return None, "desktop not answering", card_seen
            continue
        if state.get("locked"):
            return state, "locked", card_seen
        if state.get("pending_approvals"):
            # A card is the Mind waiting for a person, not the turn ending: keep waiting (up to
            # the turn limit) so a person who answers it finds the mission still going. On 561
            # the Director stopped the moment the first card rose, before Pranab could answer.
            if not card_seen:
                card_seen = True
                log(event="card waiting", actions=[a.get("action") for a in state["pending_approvals"]])
            if time.time() - sent_at > TURN_LIMIT_SECS:
                return state, "card unanswered", card_seen
            continue
        if state.get("thinking"):
            seen_busy = True
        if (seen_busy or time.time() - sent_at > 30) and mind_idle(state):
            return state, "answered", card_seen
        if time.time() - sent_at > TURN_LIMIT_SECS:
            return state, "turn too long", card_seen


def check_page(m):
    """Load the mission's page and return (clean, errors). clean is None when there is no page to
    check or it could not be checked (no chromium, a hang), which neither holds up nor passes."""
    page = m.get("check_page")
    if not page:
        return None, ""
    try:
        done = subprocess.run([CHECK_PAGE, os.path.expanduser(page)], capture_output=True, text=True, timeout=90)
    except (OSError, subprocess.TimeoutExpired) as why:
        return None, str(why)[:200]
    said = done.stdout.strip()[:1500]
    if done.returncode == 0:
        return True, ""
    if done.returncode == 1:
        return False, said
    return None, said


def read_state(path, empty):
    try:
        with open(path, encoding="utf-8") as f:
            return type(empty)(json.load(f))
    except (OSError, ValueError, TypeError):
        return empty


def write_state(path, value):
    os.makedirs(STATE_DIR, exist_ok=True)
    with open(path + ".new", "w", encoding="utf-8") as f:
        json.dump(sorted(value) if isinstance(value, set) else value, f)
    os.replace(path + ".new", path)


def finished_missions():
    return read_state(DONE, set())


def mark_finished(mission_id):
    write_state(DONE, finished_missions() | {mission_id})
    # A finished build earns the finished games their polish rounds before the next build.
    write_state(POLISH_DUE, POLISH_PER_BUILD)


def build(spec):
    """A build mission from its entry in missions.json: the shared wording, filled in."""
    folder = spec["folder"]
    page = f"~/Projects/{folder}/index.html"
    fields = dict(name=spec["name"], what=spec["what"], details=spec["details"], plan=spec["plan"],
                  steps=spec.get("steps", 8), folder=folder, check=f"{CHECK_PAGE} {page}", notes="PLAN.md")
    return dict(id=spec["id"], title=spec["title"], fields=fields, check_page=page, brief=BRIEF,
                keep_going=KEEP_GOING, max_turns=spec.get("max_turns", 40), max_hours=spec.get("max_hours", 3))


def polish(b):
    """A polish round on a finished build: one improvement from its IDEAS.md."""
    fields = dict(b["fields"], notes="IDEAS.md")
    return dict(b, id="polish:" + b["id"], title="Polishing " + b["fields"]["name"], fields=fields,
                brief=POLISH, keep_going=POLISH_KEEP_GOING, max_turns=POLISH_TURNS, max_hours=POLISH_HOURS)


def is_code(name):
    """Whether a file is part of what the page is made of, rather than notes or a backup."""
    lower = name.lower()
    if lower.endswith((".md", "~", ".orig", ".tmp", ".swp")) or ".bak" in lower:
        return False
    return True


def folder_print(m):
    """What the mission's code holds, by name and content: two prints differ when a file the
    page is made of was really changed. Notes (`.md`), backups (`.bak`, `~`) and hidden entries
    are not counted: on 561 a Labyrinth round was taken as done when the Mind ticked IDEAS.md
    and copied maze.js to maze.js.bak, while maze.js itself was byte for byte what it had been
    (16 Oct 2026). Content, not modification time, so rewriting a file unchanged is not a
    change either. A missing folder prints empty."""
    root = os.path.join(PROJECTS, m["fields"]["folder"])
    seen = []
    for top, dirs, files in os.walk(root):
        dirs[:] = sorted(x for x in dirs if not x.startswith("."))
        for name in sorted(files):
            if name.startswith(".") or not is_code(name):
                continue
            try:
                with open(os.path.join(top, name), "rb") as f:
                    digest = hashlib.sha256(f.read()).hexdigest()
            except OSError:
                continue
            seen.append((os.path.relpath(os.path.join(top, name), root), digest))
    return tuple(seen)


def prompt(template, m, errors=""):
    return template.format(errors=errors, **m["fields"])


def next_mission(builds):
    """The first unfinished build, unless polish rounds are owed: after each finished build, the
    next POLISH_PER_BUILD missions are polish rounds, so builds and polish take turns rather than
    every build first and then polish only. Once all builds are finished, polish rounds only. A
    polish round goes to the finished build polished longest ago (never polished first, then in
    the missions' order)."""
    done = finished_missions()
    todo = [b for b in builds if b["id"] not in done]
    finished = [b for b in builds if b["id"] in done]
    due = read_state(POLISH_DUE, 0)
    if todo and (due <= 0 or not finished):
        return todo[0]
    if not finished:
        return None
    if todo:
        write_state(POLISH_DUE, due - 1)
    # Each build's last polish round, by number: rounds are counted, not timed, so two rounds in
    # the same second still go round in turn.
    polished = read_state(POLISHED, {})
    b = min(finished, key=lambda b: (polished.get(b["id"], 0), builds.index(b)))
    polished[b["id"]] = max(polished.values(), default=0) + 1
    write_state(POLISHED, polished)
    return polish(b)


def ensure_demo_idea(m):
    """Put DEMO_IDEA at the head of the game's IDEAS.md, unticked, unless the file already has a
    line about ?demo=1 (ticked or not), so the polish round takes it before the game's other
    ideas. The Director's one write into a mission's folder, and only to its notes: a note is not
    code, so it never counts as the Mind having changed anything (folder_print)."""
    folder = m["fields"]["folder"]
    if not FOLDER.match(folder) or not os.path.isdir(os.path.join(PROJECTS, folder)):
        return False
    path = os.path.join(PROJECTS, folder, "IDEAS.md")
    if os.path.islink(path):
        return False
    try:
        with open(path, encoding="utf-8") as f:
            lines = f.read().splitlines()
    except FileNotFoundError:
        lines = []
    except (OSError, ValueError):
        return False
    if any("demo=1" in line for line in lines):
        return False
    at = next((i for i, line in enumerate(lines) if re.match(r"\s*[-*+]\s+\[[ xX]\]", line)), len(lines))
    lines.insert(at, "- [ ] " + DEMO_IDEA[0].upper() + DEMO_IDEA[1:])
    write_atomically(path, "\n".join(lines) + "\n")
    return True


def run_mission(m):
    started = time.time()
    if m["id"].startswith("polish:"):
        try:
            if ensure_demo_idea(m):
                log(mission=m["id"], event="demo idea added", to="IDEAS.md")
        except OSError as why:
            log(mission=m["id"], event="demo idea not added", why=str(why)[:200])
    # A build stopped part way (a card, a limit, the screen locked) is picked up where it is, not
    # started over: the brief would have the Mind write a fresh plan over its own ticks.
    begun = read_state(STARTED, set())
    # Only once the plan exists: a start that never reached the Mind (the model unreachable, a
    # card) left nothing to keep going on, and "keep going" carries none of the brief. On 561 the
    # Mind built Orrery from its name alone and said it had never seen the brief (15 Oct 2026).
    planned = os.path.exists(os.path.join(PROJECTS, m["fields"]["folder"], "PLAN.md"))
    resuming = m["id"] in begun and planned and not m["id"].startswith("polish:")
    # Picked up after other missions ran: their conversation, not this one's, is on screen, so it
    # starts afresh and is told again what the build is.
    elsewhere = resuming and read_state(LAST, "") != m["id"]
    write_state(STARTED, begun | {m["id"]})
    write_state(LAST, m["id"])
    log(mission=m["id"], event="resuming" if resuming else "starting", title=m["title"])
    text = prompt(RESUME if elsewhere else m["keep_going"] if resuming else m["brief"], m)
    show_mission(m)
    if not resuming or elsewhere:
        ok, said = new_chat()
        if not ok:
            log(mission=m["id"], event="fresh chat refused", said=said)
    cards = 0
    previous = ""
    # A FINISHED counts only over a folder that changed. On 561 the Mind read Beatgrid's files
    # eight times, wrote nothing, and said FINISHED over a description of code it had not
    # written; the page, untouched, checked clean (16 Oct 2026).
    before = folder_print(m)
    for turn in range(1, m["max_turns"] + 1):
        if time.time() - started > m["max_hours"] * 3600:
            log(mission=m["id"], event="stopped", why="time limit", turns=turn - 1)
            return
        # Someone (or the Mind on its own) may be mid-turn: let it finish before typing over it.
        for _ in range(TURN_LIMIT_SECS // POLL_SECS):
            state = shell_state()
            if state is not None and not state.get("thinking"):
                break
            time.sleep(POLL_SECS)
        show_mind_view()
        ok, said = send(text)
        if not ok:
            log(mission=m["id"], event="send failed", turn=turn, said=said)
            return
        state, why, card = wait_for_turn(time.time())
        reply = whole_text(last_reply(state)) if state else ""
        clean, errors = check_page(m)
        log(mission=m["id"], event="turn", turn=turn, outcome=why, reply=reply[:400],
            page={True: "loads clean", False: "throws", None: "not checked"}[clean], errors=errors or None)
        # The game reloaded in the showcase after every turn, its page checked first.
        show_mission(m, turn, reply, clean)
        if why != "answered":
            log(mission=m["id"], event="stopped", why=why, turns=turn)
            return
        # A card the Mind gave up waiting on ends its turn as if answered, and "keep going" only
        # raises the next one. Two turns running with a card is a person needed: stop, and the
        # timer tries again after its rest. On 561 the first agent_run of a new Mind session asks,
        # and each card lapsed in 110 s while the Director kept typing (15 Oct 2026).
        cards = cards + 1 if card else 0
        if cards >= 2:
            log(mission=m["id"], event="stopped", why="cards are waiting for a person", turns=turn)
            return
        unchanged = said_finished(reply) and folder_print(m) == before
        if unchanged:
            log(mission=m["id"], event="finished refused", turn=turn, why="nothing in the folder changed")
        elif said_finished(reply):
            if clean is False:
                # The Mind's word against the page's: the page wins, and the mission goes on.
                log(mission=m["id"], event="finished refused", turn=turn, why="the page throws")
            else:
                if not m["id"].startswith("polish:"):
                    mark_finished(m["id"])
                log(mission=m["id"], event="finished", turns=turn, page_checked=clean is True,
                    minutes=round((time.time() - started) / 60))
                return
        # The same answer twice is a Mind that is not working the mission, whatever the reason;
        # typing "keep going" at it again only fills the stream with the same words.
        if same_answer(reply, previous):
            log(mission=m["id"], event="stopped", why="the same answer twice", turns=turn)
            return
        previous = reply
        time.sleep(BETWEEN_TURNS_SECS)
        if clean is False:
            text = prompt(FIX_FIRST, m, errors)
        elif unchanged:
            text = prompt(NOTHING_CHANGED, m)
        else:
            text = prompt(m["keep_going"], m)
    log(mission=m["id"], event="stopped", why="turn limit")


def cycle_step(builds, index):
    """Show the next finished game in the showcase, in its demo mode, between missions. Returns
    the index to show next time. Only finished builds with a page of their own are shown."""
    done = finished_missions()
    games = [b for b in builds if b["id"] in done and game_page(b["fields"]["folder"])]
    if not games:
        write_frame("Yantrik Live", "between missions", "The Mind's next mission starts soon.", None,
                    empty="No game is finished yet: the Mind is building the first one.")
        return 0
    b = games[index % len(games)]
    write_frame(b["title"], "between missions · demo", "Built earlier by the Mind; its next mission starts soon.",
                b["fields"]["folder"])
    return (index + 1) % len(games)


def showcase_main(builds):
    """yantrik-live-showcase.service: keep the showcase window open and, while no mission is
    showing, go round the finished games one every CYCLE_SECS. A service of its own so that the
    Director's oneshot ends with its mission and its timer goes on as before."""
    browser, index, shown_at, place_at, was_live = None, 0, 0.0, 0.0, True
    while True:
        if browser is None or browser.poll() is not None:
            if browser is not None:
                log(event="showcase closed", code=browser.returncode)
                time.sleep(10)
            if not os.path.exists(os.path.join(SHOWCASE_DIR, "frame.html")):
                index = cycle_step(builds, index)
            browser = subprocess.Popen([SHOWCASE_BROWSER, os.path.join(SHOWCASE_DIR, "frame.html")])
            place_at = time.time() + 10
        showing = read_state(SHOWING, {})
        live = bool(showing.get("mission")) and time.time() - float(showing.get("at") or 0) < SHOWING_STALE_SECS
        if not live and was_live:
            # A mission just ended: the games are the thing to watch now.
            place_at = place_at or time.time()
        if place_at and time.time() >= place_at:
            state = shell_state()
            # Never while a card is waiting: the shell holds window actions then, and the card
            # must stay where the person can see it (show_mind_view).
            if state is not None and not state.get("pending_approvals"):
                place_windows(state, showcase_front=not live)
                place_at = 0.0
        if not live and (was_live or time.time() - shown_at >= CYCLE_SECS):
            try:
                index = cycle_step(builds, index)
            except OSError as why:
                log(event="showcase not written", why=str(why)[:200])
            shown_at = time.time()
        was_live = live
        time.sleep(POLL_SECS)


def load_builds():
    with open(os.path.join(HERE, "missions.json"), encoding="utf-8") as f:
        return [build(spec) for spec in json.load(f)]


def main(argv=()):
    builds = load_builds()
    if list(argv[:1]) == ["showcase"]:
        showcase_main(builds)
        return 0
    m = next_mission(builds)
    if m is None:
        log(event="idle", why="there are no missions")
        return 0
    try:
        run_mission(m)
    finally:
        # The mission is over, finished or not: the showcase goes back to the finished games.
        try:
            write_state(SHOWING, {})
        except OSError:
            pass
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
