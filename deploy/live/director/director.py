#!/usr/bin/env python3
"""The live instance's Director: gives its Mind long work, one turn at a time, on stream.

Without it the live machine sits on whatever screen it was left on. This is the person at the
keyboard, nothing more: it types a mission into the Lens through `yos act shell send_message`,
the same path a person's typing takes, waits until the Mind has finished its turn, and then says
"keep going" with the mission's next instruction, until the Mind says it is finished, the
mission's turn or time limit is reached, or something needs a person.

It never answers an approval card and never changes the Mind's mode. A card waiting means a
person is needed; the Director logs that, leaves the card up for whoever is watching, and stops
the mission rather than talking past it.

Runs on the live instance (VM 561) as the desktop's account, from yantrik-live-director.service.
Missions are in missions.json beside it. Progress goes to ~/director/log.jsonl, one line a turn.
Stdlib only; the image ships no Python packages for it.
"""
import json
import os
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
YOS = "/opt/yantrik/bin/yos"
STATE_DIR = os.path.expanduser("~/director")
LOG = os.path.join(STATE_DIR, "log.jsonl")
DONE = os.path.join(STATE_DIR, "done.json")

POLL_SECS = 5
# A beat between turns, so someone watching sees the result of one step before the next begins.
BETWEEN_TURNS_SECS = 20
# A turn that has not ended in this long is stuck; the mission stops rather than piling on.
TURN_LIMIT_SECS = 20 * 60
FINISHED = "FINISHED"


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


def mind_idle(state):
    """The Mind has finished its turn: not thinking, and the last word is its own, complete."""
    last = last_reply(state)
    return (not state.get("thinking")) and last is not None and last.get("role") == "assistant" \
        and not last.get("streaming")


def show_mind_view():
    """Put Mind View in front, so the stream shows the Mind's work. A person can minimise it or
    open something over it between turns; the person watching should not see an idle desktop
    while the Mind builds. Best effort: Mind View may not exist yet before the Mind opens an app."""
    subprocess.run([YOS, "act", "shell", "focus_window", "title=Mind View"],
                   capture_output=True, text=True, timeout=30)


def send(text):
    done = subprocess.run([YOS, "act", "shell", "send_message", f"text={text}"],
                          capture_output=True, text=True, timeout=60)
    return done.returncode == 0, (done.stdout + done.stderr).strip()[:300]


def wait_for_turn(sent_at):
    """Wait until the Mind answers what was just sent. Returns (state, why it stopped waiting)."""
    seen_busy = False
    while True:
        time.sleep(POLL_SECS)
        state = shell_state()
        if state is None:
            if time.time() - sent_at > TURN_LIMIT_SECS:
                return None, "desktop not answering"
            continue
        if state.get("locked"):
            return state, "locked"
        if state.get("pending_approvals"):
            return state, "card"
        if state.get("thinking"):
            seen_busy = True
        if (seen_busy or time.time() - sent_at > 30) and mind_idle(state):
            return state, "answered"
        if time.time() - sent_at > TURN_LIMIT_SECS:
            return state, "turn too long"


def finished_missions():
    try:
        with open(DONE, encoding="utf-8") as f:
            return set(json.load(f))
    except (OSError, ValueError):
        return set()


def mark_finished(mission_id):
    done = finished_missions() | {mission_id}
    os.makedirs(STATE_DIR, exist_ok=True)
    with open(DONE, "w", encoding="utf-8") as f:
        json.dump(sorted(done), f)


def run_mission(m):
    started = time.time()
    prompt = m["brief"]
    for turn in range(1, m.get("max_turns", 40) + 1):
        if time.time() - started > m.get("max_hours", 3) * 3600:
            log(mission=m["id"], event="stopped", why="time limit", turns=turn - 1)
            return
        # Someone (or the Mind on its own) may be mid-turn: let it finish before typing over it.
        for _ in range(TURN_LIMIT_SECS // POLL_SECS):
            state = shell_state()
            if state is not None and not state.get("thinking"):
                break
            time.sleep(POLL_SECS)
        show_mind_view()
        ok, said = send(prompt)
        if not ok:
            log(mission=m["id"], event="send failed", turn=turn, said=said)
            return
        state, why = wait_for_turn(time.time())
        reply = (last_reply(state) or {}).get("text", "") if state else ""
        log(mission=m["id"], event="turn", turn=turn, outcome=why, reply=reply[:400])
        if why != "answered":
            log(mission=m["id"], event="stopped", why=why, turns=turn)
            return
        if FINISHED in reply.split():
            mark_finished(m["id"])
            log(mission=m["id"], event="finished", turns=turn, minutes=round((time.time() - started) / 60))
            return
        time.sleep(BETWEEN_TURNS_SECS)
        prompt = m["keep_going"]
    log(mission=m["id"], event="stopped", why="turn limit")


def main():
    with open(os.path.join(HERE, "missions.json"), encoding="utf-8") as f:
        missions = json.load(f)
    done = finished_missions()
    todo = [m for m in missions if m["id"] not in done]
    if not todo:
        log(event="idle", why="every mission is finished")
        return 0
    m = todo[0]
    log(mission=m["id"], event="starting", title=m["title"])
    run_mission(m)
    return 0


if __name__ == "__main__":
    sys.exit(main())
