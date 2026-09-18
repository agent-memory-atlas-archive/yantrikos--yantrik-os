#!/usr/bin/env python3
"""Build ~/projects/yos-smoke/inventory.json from the live `yos` control surface.

Read-only: we only call `yos ls` and `yos describe <app> --full`.
Everything is parsed in-process; nothing is pasted into the conversation.
"""
import json
import re
import subprocess
import datetime
import sys

YOS = "/opt/yantrik/bin/yos"
OUT = "/home/yantrik/projects/yos-smoke/inventory.json"
SUMMARY = "/home/yantrik/projects/yos-smoke/summary.txt"

# app id -> /opt/yantrik/bin binary that serves it (None = no dedicated process)
BINARY = {
    "a11y": "a11y-service",
    "app-calendar": "yantrik-calendar",
    "app-download-manager": "yantrik-download-manager",
    "app-email": "yantrik-email",
    "app-notes": "yantrik-notes",
    "app-shell": "yantrik-ui",
    "app-terminal": "yantrik-terminal",
    "network": "network-service",
    "notifications": "notifications-service",
    "system-monitor": "system-monitor-service",
    "weather": "weather-service",
    "companion": None,   # RPC-live surface, embedded (no dedicated binary)
    "harness": None,     # RPC-live surface, embedded (no dedicated binary)
}

SAFE_TIER = {"safe", "standard"}   # "safe & no-arg" = low-risk tier, zero required args


def run(cmd, timeout=30):
    try:
        p = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        return (p.stdout or "") + (("\n" + p.stderr) if p.stderr else "")
    except subprocess.TimeoutExpired:
        return "<timeout>"


def parse_surface_list(text):
    apps = []
    for line in text.splitlines():
        s = line.strip()
        if not s:
            continue
        if s.endswith(":"):          # section header e.g. /run/user/1000/yantrik:
            continue
        if " " in s:
            continue                 # skip anything that isn't a bare token
        apps.append(s)
    # de-dupe, preserve order
    seen, out = set(), []
    for a in apps:
        if a not in seen:
            seen.add(a)
            out.append(a)
    return out


ACT_RE = re.compile(
    r"^\s*act:\s*(?P<name>[A-Za-z0-9_]+)\s*\(\s*(?P<args>[^)]*)\s*\)"
    r"\s*\[\s*(?P<grade>\w+)\s*,\s*settles\s+(?P<settles>on return|later)\s*\]",
    re.MULTILINE,
)
METHODS_RE = re.compile(r"this service (?:serves|speaks):?\s*(?P<m>[A-Za-z0-9_.,\s]+)$", re.MULTILINE)


def describe(app):
    raw = run([YOS, "describe", app, "--full"])
    lines = raw.splitlines()
    header = lines[0].strip() if lines and not lines[0].lstrip().startswith("{") else None
    revision = None
    for ln in lines:
        if ln.strip().startswith("revision:"):
            revision = ln.strip().split(":", 1)[1].strip()
            break

    actions = []
    for m in ACT_RE.finditer(raw):
        args = [a.strip() for a in m.group("args").split(",") if a.strip()]
        settles_on_return = (m.group("settles") == "on return")
        actions.append({
            "name": m.group("name"),
            "grade": m.group("grade"),
            "required_args": args,
            "settles_on_return": settles_on_return,
            "settles_raw": "settles " + m.group("settles"),
        })

    result = {
        "id": app,
        "protocol": "app",
        "header": header,
        "revision": revision,
        "actions": actions,
        "methods": None,
        "describe_error": None,
    }

    if not actions:
        # companion/harness-style: refuses app.describe and advertises its own methods
        err_line = next((ln for ln in lines if "refused" in ln or "unknown method" in ln), None)
        if err_line:
            result["protocol"] = app if app in ("companion", "harness") else "unknown"
            result["describe_error"] = err_line.strip()
            mm = METHODS_RE.search(err_line)
            if mm:
                result["methods"] = [x.strip() for x in mm.group("m").split(",") if x.strip()]
    return result


def process_table():
    out = run(["ps", "-eo", "pid=,args="])
    rows = []
    for ln in out.splitlines():
        parts = ln.strip().split(None, 1)
        if len(parts) == 2:
            rows.append((parts[0], parts[1]))
    return rows


def main():
    procs = process_table()  # snapshot BEFORE we touch any surface
    apps = parse_surface_list(run([YOS, "ls"]))

    def running_for(app):
        binname = BINARY.get(app)
        if not binname:
            return False, None, "no dedicated binary in /opt/yantrik/bin (RPC-live, embedded surface)"
        target = "/opt/yantrik/bin/" + binname
        pids = [pid for pid, args in procs if target in args]
        if pids:
            return True, f"{len(pids)} pid(s): {', '.join(pids)} running {target}", None
        return False, None, f"no process running {target}"

    out_apps = []
    reply_lines = []
    for app in apps:
        d = describe(app)
        running, evidence, note = running_for(app)
        n_actions = len(d["actions"])
        safe_no_arg = sum(
            1 for a in d["actions"]
            if a["grade"] in SAFE_TIER and not a["required_args"]
        )
        entry = {
            "id": app,
            "protocol": d["protocol"],
            "already_running": running,
            "running_evidence": evidence,
            "running_note": note,
            "header": d["header"],
            "revision": d["revision"],
            "action_count": n_actions,
            "safe_no_arg_count": safe_no_arg,
            "actions": d["actions"],
        }
        if d["methods"]:
            entry["methods"] = d["methods"]
        if d["describe_error"]:
            entry["describe_error"] = d["describe_error"]
        out_apps.append(entry)
        reply_lines.append(f"{app}: {n_actions} actions, {safe_no_arg} safe no-arg")

    # Known full app set (BINARY keys) minus what `yos ls` reports up right now:
    # running processes whose control surface is currently down (no socket).
    full_known = list(BINARY.keys())
    up_set = set(apps)
    down_entries = []
    for a in full_known:
        if a in up_set:
            continue
        binname = BINARY.get(a)
        target = "/opt/yantrik/bin/" + binname if binname else None
        pids = [pid for pid, args in procs if target and target in args]
        down_entries.append({
            "id": a,
            "surface_up": False,
            "process_running": bool(pids),
            "running_pids": pids,
            "note": "not currently publishing a control surface (`yos describe` -> 'no socket'); "
                    "out of scope. process_running field shows whether a process is up.",
        })

    doc = {
        "generated_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "source": {
            "surface_list": YOS + " ls",
            "describe": YOS + " describe <app> --full",
        },
        "scope_note": "scope = surfaces reported up by `yos ls` at snapshot time. "
                      "See surfaces_not_up for known apps not currently publishing a surface.",
        "surfaces_not_up": down_entries,
        "definitions": {
            "already_running": "a dedicated OS process for this app id is present in the "
                               "process table at snapshot time (matched by /opt/yantrik/bin/<binary>). "
                               "Snapshot taken before any surface was acted on.",
            "safe_no_arg": "grade in {safe, standard} AND zero required arguments "
                           "(i.e. fire-and-forget low-risk actions).",
            "settles_on_return": "true = 'settles on return' (completes synchronously); "
                                 "false = 'settles later' (async: launch dispatched / build started).",
        },
        "apps": out_apps,
    }
    with open(OUT, "w") as f:
        json.dump(doc, f, indent=2)
    with open(SUMMARY, "w") as f:
        f.write("\n".join(reply_lines) + "\n")

    # compact stdout report (well under 40 lines)
    print(f"wrote {OUT}  ({len(out_apps)} surfaces)")
    print("-" * 40)
    for ln in reply_lines:
        print(ln)
    return 0


if __name__ == "__main__":
    sys.exit(main())
