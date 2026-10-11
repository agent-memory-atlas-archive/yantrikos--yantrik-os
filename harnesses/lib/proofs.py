#!/usr/bin/env python3
"""The three proofs that a harness is installed, seen rather than assumed: proofs.py HARNESS [--wait S] [--json]

    Can it answer?            a real call through the model gateway, when the install wired the
                              model (#675); "model: its own" when it did not, which is not a failure
    Can it remember?          a temporary note written through the harness's own memory path,
                              recalled, and deleted: "memory · YantrikDB · recalled"
    Can it reach the desktop? an attach seen on the harness socket: the desktop lists the harness
                              as attached (`yos describe shell`, harnesses[].state)

A harness is never reported installed until all three pass (design/onboarding-one-model-one-
memory-2026-10-10.md, "The flow" 2.4). Every installer in lib/install ends with this, and the
Settings → Harnesses row and the onboarding summary call the same thing:

    python3 /opt/yantrik/share/harnesses/lib/proofs.py hermes --json

which prints one JSON document and exits 0 only when every proof passed:

    {"harness": "hermes", "passed": true,
     "proofs": [{"id": "answer", "question": "Can it answer?", "ok": true,
                 "status": "connected", "says": "connected · deepseek-v4-pro",
                 "fix": null}, ...]}

`status` is the word a program matches on (connected, own, recalled, paired, failed); `says` is
the line a person reads; `fix`, on a failure, is the one press that puts it right: {"label",
"action"} where action is `install` (run the row's Install again), `configure` (the row's own
setup) or `restart` (with the `command` to run). Nothing here prints a secret: the probe note is a
fixed sentence with a random number, and the model probe's own output is reduced to one line.

Stdlib only, and run with the image's Python, so it works before any harness has its own.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
import uuid
from pathlib import Path
from typing import Any, Callable, Dict, List, Optional

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

ANSWER, REMEMBER, DESKTOP = "Can it answer?", "Can it remember?", "Can it reach the desktop?"

# Where the baked memory lives (build-debian-iso.sh), as common.sh has it.
MEMORY_DIR = Path(os.environ.get("YANTRIK_MEMORY_DIR", "/opt/yantrik/memory"))
YOS = os.environ.get("YOS_BIN", "/opt/yantrik/bin/yos")

# The harnesses that reach YantrikDB through yantrikdb-mcp, and the one that has a provider of
# its own inside the harness program (Hermes). The Mind keeps its own memory (yantrik-memory).
MCP_MEMORY = ("pi", "deepseek", "openclaw")


def state_dir(harness: str) -> Path:
    base = os.environ.get("XDG_STATE_HOME") or str(Path.home() / ".local" / "state")
    return Path(base) / "yantrik" / "harnesses" / harness


def memory_db(harness: str) -> Path:
    base = os.environ.get("XDG_DATA_HOME") or str(Path.home() / ".local" / "share")
    return Path(base) / "yantrik" / "memory" / ("%s.db" % harness)


def row(id_: str, question: str, ok: bool, status: str, says: str,
        fix: Optional[Dict[str, str]] = None) -> Dict[str, Any]:
    return {"id": id_, "question": question, "ok": ok, "status": status, "says": says,
            "fix": None if ok else fix}


def one_line(text: str, limit: int = 160) -> str:
    text = " ".join(str(text or "").split())
    return text if len(text) <= limit else text[: limit - 1] + "…"


def restart_fix(harness: str) -> Dict[str, str]:
    if harness == "hermes":
        command = "systemctl --user restart hermes-gateway"
    elif harness == "mind":
        command = "systemctl start yantrik-mind.service"
    else:
        command = "systemctl --user restart yantrik-%s.service" % harness
    return {"label": "Restart it", "action": "restart", "command": command}


INSTALL_FIX = {"label": "Install again", "action": "install"}


# ── Can it answer? ──────────────────────────────────────────────────────────────────────────────

def prove_answer(harness: str, run: Callable[..., subprocess.CompletedProcess] = subprocess.run
                 ) -> Dict[str, Any]:
    """The model gateway, when the install wired it; otherwise the harness's own model, said so.

    The probe is the model half's own (#675): lib/install/model.sh's `yantrik_model_probe HARNESS`
    makes one real call with the harness's gateway token and prints the model that answered.
    """
    try:
        wired = (state_dir(harness) / "model").read_text(encoding="utf-8").strip()
    except OSError:
        wired = ""
    if not wired:
        return row("answer", ANSWER, True, "own", "model: its own")
    fix = {"label": "Use Yantrik models", "action": "configure"}
    model_sh = HERE / "install" / "model.sh"
    if wired != "picked" or not model_sh.is_file():
        return row("answer", ANSWER, False, "failed",
                   "not connected: the model you picked was not wired in", fix)
    try:
        done = run(["sh", "-c", '. "$1" && yantrik_model_probe "$2"', "sh", str(model_sh), harness],
                   capture_output=True, text=True, timeout=90, stdin=subprocess.DEVNULL)
    except (OSError, subprocess.TimeoutExpired) as exc:
        return row("answer", ANSWER, False, "failed", "not connected: %s" % one_line(exc), fix)
    if done.returncode != 0:
        said = (done.stderr or done.stdout or "").strip().splitlines()
        reason = one_line(said[-1]) if said else "the gateway did not answer"
        return row("answer", ANSWER, False, "failed", "not connected: %s" % reason, fix)
    model = one_line((done.stdout or "").strip().splitlines()[0] if done.stdout.strip() else "picked", 80)
    return row("answer", ANSWER, True, "connected", "connected · %s" % model)


# ── Can it remember? ────────────────────────────────────────────────────────────────────────────

def probe_note() -> str:
    return "Yantrik install check %s: this note is written, recalled and deleted at once." % uuid.uuid4().hex[:12]


def remember_through_mcp(harness: str, command: Optional[List[str]] = None,
                         env: Optional[Dict[str, str]] = None) -> Dict[str, Any]:
    """Write, recall and delete one note through yantrikdb-mcp with the harness's own settings,
    the same server and the same file the harness's memory tools use."""
    from yantrik_harness import McpTools

    command = command or [str(MEMORY_DIR / "bin" / "yantrikdb-mcp")]
    if not Path(command[0]).exists():
        return row("remember", REMEMBER, False, "failed",
                   "no memory: YantrikDB is not in this image (%s)" % command[0], INSTALL_FIX)
    env = dict(env or mcp_env(harness))
    tools = McpTools("Yantrik install check", "1", command=command, env=env, timeout=60,
                     log=lambda _m: None)
    note = probe_note()
    try:
        text, error = tools.call("remember", {"text": note, "importance": 0.05, "source": harness,
                                              "metadata": {"provenance": harness, "probe": True}})
        if error:
            return row("remember", REMEMBER, False, "failed", "could not write: %s" % one_line(text),
                       INSTALL_FIX)
        try:
            rid = json.loads(text).get("rid") or ""
        except (ValueError, AttributeError):
            rid = ""
        found, error = tools.call("recall", {"query": note, "top_k": 5})
        recalled = (not error) and bool(rid) and rid in found
        if rid:
            tools.call("forget", {"rid": rid})
    finally:
        tools.close()
    if not recalled:
        return row("remember", REMEMBER, False, "failed",
                   "wrote a note but could not recall it", INSTALL_FIX)
    return row("remember", REMEMBER, True, "recalled", "memory · YantrikDB · recalled")


def mcp_env(harness: str) -> Dict[str, str]:
    """What every yantrikdb-mcp started for a harness is given: its own file, the engine's bundled
    embedder (offline, nothing fetched), and the tools a mind needs rather than the full set."""
    return {"YANTRIKDB_DB_PATH": str(memory_db(harness)), "YANTRIKDB_EMBEDDER": "bundled"}


HERMES_PROBE = r"""
import json, os, sys
env = os.path.expanduser(os.environ.get("HERMES_HOME", "~/.hermes")) + "/.env"
try:
    for line in open(env, encoding="utf-8"):
        key, sep, value = line.strip().partition("=")
        if sep and key.startswith("YANTRIKDB_"):
            os.environ[key] = value.strip().strip('"').strip("'")
except OSError:
    pass
from yantrikdb_hermes_plugin.client import YantrikDBConfig
from yantrikdb_hermes_plugin.embedded import make_backend
config = YantrikDBConfig.from_env()
client = make_backend(config)
note = sys.argv[1]
rid = client.remember(note, importance=0.05, metadata={"source": "hermes", "probe": True}).get("rid") or ""
found = json.dumps(client.recall(note, top_k=5), default=str)
if rid:
    client.forget(rid)
print(json.dumps({"mode": config.mode, "recalled": bool(rid) and rid in found}))
"""


def hermes_python() -> Optional[str]:
    """The Python Hermes runs on, beside the `hermes` it put in ~/.local/bin."""
    for candidate in (Path.home() / ".local" / "bin" / "hermes",):
        try:
            real = candidate.resolve(strict=True)
        except OSError:
            continue
        python = real.parent / "python"
        if python.exists():
            return str(python)
    return None


def remember_through_hermes(run: Callable[..., subprocess.CompletedProcess] = subprocess.run
                            ) -> Dict[str, Any]:
    """Hermes's own word first (`hermes memory status`), then a note through the provider's own
    backend, built from Hermes's own settings, in Hermes's own Python."""
    hermes = str(Path.home() / ".local" / "bin" / "hermes")
    try:
        status = run([hermes, "memory", "status"], capture_output=True, text=True, timeout=60,
                     stdin=subprocess.DEVNULL)
        said = (status.stdout or "") + (status.stderr or "")
    except (OSError, subprocess.TimeoutExpired) as exc:
        return row("remember", REMEMBER, False, "failed", "Hermes did not answer: %s" % one_line(exc),
                   INSTALL_FIX)
    lowered = said.lower()
    if "yantrikdb" not in lowered or "available" not in lowered or "not available" in lowered:
        return row("remember", REMEMBER, False, "failed",
                   "Hermes's memory is not YantrikDB, or not available", INSTALL_FIX)
    python = hermes_python()
    if not python:
        return row("remember", REMEMBER, False, "failed", "cannot find the Python Hermes runs on",
                   INSTALL_FIX)
    try:
        done = run([python, "-c", HERMES_PROBE, probe_note()], capture_output=True, text=True,
                   timeout=120, stdin=subprocess.DEVNULL)
    except (OSError, subprocess.TimeoutExpired) as exc:
        return row("remember", REMEMBER, False, "failed", "could not write: %s" % one_line(exc),
                   INSTALL_FIX)
    try:
        answer = json.loads((done.stdout or "").strip().splitlines()[-1])
    except (ValueError, IndexError):
        answer = {}
    if done.returncode != 0 or not answer.get("recalled"):
        reason = one_line((done.stderr or "").strip().splitlines()[-1]) if (done.stderr or "").strip() \
            else "wrote a note but could not recall it"
        return row("remember", REMEMBER, False, "failed", reason, INSTALL_FIX)
    return row("remember", REMEMBER, True, "recalled", "memory · YantrikDB · recalled")


def prove_remember(harness: str) -> Dict[str, Any]:
    if harness == "hermes":
        return remember_through_hermes()
    if harness in MCP_MEMORY:
        return remember_through_mcp(harness)
    if harness == "mind":
        # The Mind's memory is the person's YantrikDB itself, kept by yantrik-memory under the
        # Mind's own account; this process may not open it, and must not.
        return row("remember", REMEMBER, True, "own", "memory · YantrikDB (the Mind's own)")
    return row("remember", REMEMBER, False, "failed", "no memory path for %s" % harness, INSTALL_FIX)


# ── Can it reach the desktop? ───────────────────────────────────────────────────────────────────

def desktop_state(harness: str, run: Callable[..., subprocess.CompletedProcess] = subprocess.run
                  ) -> str:
    """The desktop's own word for the harness: `attached`, `answering`, or whatever else it is.

    The desktop lists a harness as attached only once it has attached on the harness socket,
    whose peer credentials the host checked at accept; nothing a harness says makes it so."""
    try:
        done = run([YOS, "describe", "shell", "--json"], capture_output=True, text=True, timeout=20,
                   stdin=subprocess.DEVNULL)
        if done.returncode != 0:
            return "no desktop"
        view = json.loads(done.stdout or "{}")
    except (OSError, ValueError, subprocess.TimeoutExpired):
        return "no desktop"
    rows = ((view.get("state") or {}).get("harnesses") or []) if isinstance(view, dict) else []
    for entry in rows:
        if isinstance(entry, dict) and entry.get("id") == harness:
            return str(entry.get("state") or "")
    return "not listed"


def prove_desktop(harness: str, wait: float,
                  state: Callable[[str], str] = desktop_state,
                  sleep: Callable[[float], None] = time.sleep) -> Dict[str, Any]:
    end = time.monotonic() + max(wait, 0.0)
    seen = state(harness)
    while seen not in ("attached", "answering") and time.monotonic() < end:
        sleep(2.0)
        seen = state(harness)
    if seen in ("attached", "answering"):
        return row("desktop", DESKTOP, True, "paired", "paired")
    return row("desktop", DESKTOP, False, "failed",
               "not paired: the desktop has not seen it attach (%s)" % one_line(seen, 60),
               restart_fix(harness))


# ── All three ───────────────────────────────────────────────────────────────────────────────────

def prove(harness: str, wait: float = 0.0, only: Optional[List[str]] = None) -> Dict[str, Any]:
    """The function the Settings row and the onboarding summary call."""
    wanted = only or ["answer", "remember", "desktop"]
    proofs = []
    if "answer" in wanted:
        proofs.append(prove_answer(harness))
    if "remember" in wanted:
        proofs.append(prove_remember(harness))
    if "desktop" in wanted:
        proofs.append(prove_desktop(harness, wait))
    return {"harness": harness, "passed": all(p["ok"] for p in proofs), "proofs": proofs}


def render(result: Dict[str, Any]) -> str:
    lines = []
    for p in result["proofs"]:
        mark = "✓" if p["ok"] else "✗"
        line = "%s %s %s" % (mark, p["question"], p["says"])
        if p["fix"]:
            line += "  →  %s" % p["fix"]["label"]
            if p["fix"].get("command"):
                line += " (%s)" % p["fix"]["command"]
        lines.append(line)
    return "\n".join(lines)


def main(argv: Optional[List[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("harness")
    parser.add_argument("--wait", type=float, default=0.0,
                        help="seconds to wait for the attach (an install that just restarted it)")
    parser.add_argument("--only", default="", help="comma-separated: answer,remember,desktop")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    only = [w for w in args.only.split(",") if w] or None
    result = prove(args.harness, args.wait, only)
    print(json.dumps(result) if args.json else render(result), flush=True)
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
