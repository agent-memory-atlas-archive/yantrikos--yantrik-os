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

Runs on the live instance (VM 561) as the desktop's account, from yantrik-live-director.service.
Missions are in missions.json beside it. Progress goes to ~/director/log.jsonl, one line a turn.
Stdlib only; the image ships no Python packages for it.
"""
import json
import os
import re
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
YOS = "/opt/yantrik/bin/yos"
STATE_DIR = os.path.expanduser("~/director")
LOG = os.path.join(STATE_DIR, "log.jsonl")
DONE = os.path.join(STATE_DIR, "done.json")
STARTED = os.path.join(STATE_DIR, "started.json")
POLISHED = os.path.join(STATE_DIR, "polished.json")
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
    "each an unticked checklist line. Then say FINISHED on its own line.")
KEEP_GOING = (
    "Please keep going on {name}. Read PLAN.md in the {folder} folder through the shell's agent_run "
    "action. If the folder or the plan does not exist yet, make them first, as the brief said. Otherwise "
    "take the first step that is not ticked, do it, and check the page by running `{check}` through "
    "agent_run. Fix any error it prints, and tick the step off in PLAN.md only when its code is really "
    "there and the check is clean. If every step is ticked and the check is clean, write IDEAS.md if it "
    "is not there yet (five improvements, each an unticked checklist line), then say FINISHED on its own line.")
FIX_FIRST = (
    "Before anything else on {name}: the page throws an error when it loads. The Director just loaded it "
    "in a headless browser and it said:\n\n{errors}\n\nFix that first, through the shell's agent_run "
    "action, and check it again by running `{check}`. Then read the code and {notes} side by side, and "
    "untick any line whose code is not really there yet: a line is ticked only when it works. Then carry "
    "on with the first line that is not ticked.")
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


def show_mind_view():
    """Put Mind View in front, so the stream shows the Mind's work. A person can minimise it or
    open something over it between turns; the person watching should not see an idle desktop
    while the Mind builds. Best effort: Mind View may not exist yet before the Mind opens an app.

    Never while a card is waiting: the card is drawn in the shell's Mind panel, and Mind View
    maximised covers the whole shell. On 561 a waiting agent_run card sat behind it and the
    person at the machine looked for it and could not see it (2 Oct 2026)."""
    state = shell_state()
    if state is not None and state.get("pending_approvals"):
        return
    subprocess.run([YOS, "act", "shell", "focus_window", "title=Mind View"],
                   capture_output=True, text=True, timeout=30)


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


def prompt(template, m, errors=""):
    return template.format(errors=errors, **m["fields"])


def next_mission(builds):
    """The first unfinished build; once all are finished, a polish round on the build polished
    longest ago (never polished first, then in the missions' order)."""
    done = finished_missions()
    todo = [b for b in builds if b["id"] not in done]
    if todo:
        return todo[0]
    if not builds:
        return None
    # Each build's last polish round, by number: rounds are counted, not timed, so two rounds in
    # the same second still go round in turn.
    polished = read_state(POLISHED, {})
    b = min(builds, key=lambda b: (polished.get(b["id"], 0), builds.index(b)))
    polished[b["id"]] = max(polished.values(), default=0) + 1
    write_state(POLISHED, polished)
    return polish(b)


def run_mission(m):
    started = time.time()
    # A build stopped part way (a card, a limit, the screen locked) is picked up where it is, not
    # started over: the brief would have the Mind write a fresh plan over its own ticks.
    begun = read_state(STARTED, set())
    # Only once the plan exists: a start that never reached the Mind (the model unreachable, a
    # card) left nothing to keep going on, and "keep going" carries none of the brief. On 561 the
    # Mind built Orrery from its name alone and said it had never seen the brief (15 Oct 2026).
    planned = os.path.exists(os.path.join(PROJECTS, m["fields"]["folder"], "PLAN.md"))
    resuming = m["id"] in begun and planned and not m["id"].startswith("polish:")
    write_state(STARTED, begun | {m["id"]})
    log(mission=m["id"], event="resuming" if resuming else "starting", title=m["title"])
    text = prompt(m["keep_going"] if resuming else m["brief"], m)
    cards = 0
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
        if said_finished(reply):
            if clean is False:
                # The Mind's word against the page's: the page wins, and the mission goes on.
                log(mission=m["id"], event="finished refused", turn=turn, why="the page throws")
            else:
                if not m["id"].startswith("polish:"):
                    mark_finished(m["id"])
                log(mission=m["id"], event="finished", turns=turn, page_checked=clean is True,
                    minutes=round((time.time() - started) / 60))
                return
        time.sleep(BETWEEN_TURNS_SECS)
        text = prompt(FIX_FIRST, m, errors) if clean is False else prompt(m["keep_going"], m)
    log(mission=m["id"], event="stopped", why="turn limit")


def main():
    with open(os.path.join(HERE, "missions.json"), encoding="utf-8") as f:
        builds = [build(spec) for spec in json.load(f)]
    m = next_mission(builds)
    if m is None:
        log(event="idle", why="there are no missions")
        return 0
    run_mission(m)
    return 0


if __name__ == "__main__":
    sys.exit(main())
