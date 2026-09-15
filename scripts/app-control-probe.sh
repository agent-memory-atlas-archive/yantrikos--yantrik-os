#!/bin/bash
# Prove that our apps can account for themselves.
#
# Starts every app that publishes a control surface, then talks to their sockets from an unrelated
# process — the same position the companion is in. A pass means the mind can read what a Yantrik
# app is showing and steer it without a screenshot, a vision model, or a synthetic click.
#
# Deliberately different shapes: one open document (notes), a folder with a list and a selection
# (email), a grid of dates (calendar), a machine that changes under you every two seconds
# (system-monitor), a reading of somewhere else (weather), a shell that runs what you type
# (terminal), and work that outlives the call that started it (download-manager). One trait
# carries all of them.
set -u

export DISPLAY=:0 SLINT_BACKEND=winit-software
unset XDG_RUNTIME_DIR          # so the sockets land in /tmp/yantrik-<uid>, which we can glob
TARGET=${TARGET:-/home/yantrik/target-yantrik/fast}
cd /home/yantrik/yantrik-run || exit 1

APPS="notes email calendar system-monitor weather containers terminal download-manager"

# Matched by path, not by name: pgrep/pkill compare against a 15-character process name, so
# `yantrik-system-monitor` matches nothing at all — silently, which once made this script report
# two running apps as dead and leave them running afterwards.
for app in $APPS; do
  pkill -f "$TARGET/yantrik-$app" 2>/dev/null
done
pkill -f "$TARGET/yantrik-container-manager" 2>/dev/null
sleep 1
rm -f /tmp/yantrik-*/app-*.sock

for app in $APPS; do
  # Email demo mode: this box has no mail account, and a surface with nothing behind it proves
  # nothing. Every other app here has real data or a real service.
  if [ "$app" = "email" ]; then
    YANTRIK_EMAIL_DEMO=1 setsid nohup "$TARGET/yantrik-email" > "control_$app.log" 2>&1 &
  elif [ "$app" = "containers" ]; then
    # The one app whose id is not its binary name: the window is Container Manager, the surface
    # it publishes is `containers`, which is what a caller would think to ask for.
    setsid nohup "$TARGET/yantrik-container-manager" > "control_$app.log" 2>&1 &
  else
    setsid nohup "$TARGET/yantrik-$app" > "control_$app.log" 2>&1 &
  fi
done
sleep 9

# Something real to download, served from this machine. A probe that reached the internet would
# fail for reasons that have nothing to do with our code, and pass only where there is a route out.
SERVE_DIR=$(mktemp -d)
head -c 2097152 /dev/urandom > "$SERVE_DIR/probe-payload.bin"
PROBE_SHA=$(sha256sum "$SERVE_DIR/probe-payload.bin" | cut -d' ' -f1)
# Take the port back first. A run killed before its cleanup leaves a server holding 8731 and
# serving the PREVIOUS run's random payload; the next run then binds nothing, downloads those
# stale bytes, and reports a checksum mismatch that looks exactly like a bug in the app. That
# happened while this was being written, which is why the kill and the trap are both here.
pkill -f "http.server 8731" 2>/dev/null
( cd "$SERVE_DIR" && exec python3 -m http.server 8731 --bind 127.0.0.1 ) >/dev/null 2>&1 &
SERVER_PID=$!
trap 'kill "$SERVER_PID" 2>/dev/null; rm -rf "$SERVE_DIR" /tmp/yantrik-dl-probe-*' EXIT INT TERM
export PROBE_SHA PROBE_URL="http://127.0.0.1:8731/probe-payload.bin"

alive=""
for app in $APPS; do
  bin=$app
  [ "$app" = "containers" ] && bin="container-manager"
  alive="$alive $app:$(pgrep -cf "$TARGET/yantrik-$bin")"
done
echo "running:$alive"

python3 - <<'PY'
import glob, hashlib, json, os, socket, sys, tempfile, time

APPS = ["notes", "email", "calendar", "system-monitor", "weather", "containers", "terminal",
        "download-manager"]
# Not ~/Downloads: a probe must not leave two megabytes where a person keeps their own files.
DOWNLOAD_DIR = tempfile.mkdtemp(prefix="yantrik-dl-probe-")
fails = []

def socket_for(app):
    paths = glob.glob(f"/tmp/yantrik-*/app-{app}.sock")
    return paths[0] if paths else None

def call(path, method, params, timeout=10):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    s.connect(path)
    s.sendall((json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}) + "\n").encode())
    buf = b""
    while b"\n" not in buf:
        chunk = s.recv(65536)
        if not chunk:
            break
        buf += chunk
    s.close()
    return json.loads(buf.decode().splitlines()[0])

def result(r, label):
    """Unwrap the JSON-RPC envelope, then app.act's own wrapper.

    `app.act` answers {accepted, action_id, app, result, revision, settled, state, summary} and
    `app.describe` answers {app, summary, state, revision, actions}; only the first nests the
    handler's own return value. The test this replaces matched a bare {"result": ...} that the
    runtime has not sent for some time, so every `act` here was handing back the envelope and the
    lines reading e.g. `moved["showing"]` could only have raised KeyError.
    """
    if r.get("error") is not None:
        return None
    payload = r["result"]
    if isinstance(payload, dict) and "accepted" in payload and "result" in payload:
        return payload["result"]
    return payload

def error_of(r):
    return (r.get("error") or {}).get("message", "")

# ── The contract every surface owes, whatever the app ────────────────
views = {}
for app in APPS:
    path = socket_for(app)
    if not path:
        fails.append(f"{app}: published no control socket")
        continue

    view = result(call(path, "app.describe", {}), "describe")
    if view is None:
        fails.append(f"{app}: app.describe returned an error")
        continue
    views[app] = (path, view)

    print(f"\n── {app} ──")
    print(f"summary : {view['summary']}")
    actions = view.get("actions", [])
    print(f"actions : {[a['name'] for a in actions]}")
    print(f"state   : {len(json.dumps(view['state']))} bytes, keys={sorted(view['state'])[:6]}...")

    if view.get("app") != app:
        fails.append(f"{app}: describes itself as {view.get('app')!r}")
    if not view["summary"].strip():
        fails.append(f"{app}: published an empty summary")
    if not isinstance(view.get("state"), dict) or not view["state"]:
        fails.append(f"{app}: published no state")
    if not actions:
        fails.append(f"{app}: published no actions")

    # Every action declares its risk, and every argument is typed and described.
    for a in actions:
        if a.get("permission") not in ("safe", "standard", "sensitive", "dangerous"):
            fails.append(f"{app}.{a['name']}: risk is {a.get('permission')!r}")
        for name, spec in a["parameters"]["properties"].items():
            if spec.get("type") not in ("string", "number", "integer", "boolean"):
                fails.append(f"{app}.{a['name']}({name}): type is {spec.get('type')!r}")

    # A wrong call must correct itself — a model reads these errors.
    msg = error_of(call(path, "app.act", {"action": "definitely-not-an-action", "args": {}}))
    if not all(a["name"] in msg for a in actions):
        fails.append(f"{app}: an unknown action did not list the real ones — {msg!r}")

    needs_arg = next((a for a in actions if a["parameters"]["required"]), None)
    if needs_arg:
        want = needs_arg["parameters"]["required"][0]
        msg = error_of(call(path, "app.act", {"action": needs_arg["name"], "args": {}}))
        if want not in msg:
            fails.append(f"{app}: a missing `{want}` was not named — {msg!r}")
        else:
            print(f"errors  : {msg}")

# ── What each one specifically owes ──────────────────────────────────
print()

if "notes" in views:
    path, view = views["notes"]
    titles = [n["title"] for n in view["state"]["notes"]]
    if titles:
        result(call(path, "app.act", {"action": "open_note", "args": {"title": titles[0]}}), "open")
        after = result(call(path, "app.describe", {}), "describe")
        print(f"notes           : opened {after['state']['title']!r}")
        if after["state"]["title"] != titles[0]:
            fails.append(f"notes: opened {after['state']['title']!r}, asked for {titles[0]!r}")

if "email" in views:
    path, view = views["email"]
    subjects = [m["subject"] for m in view["state"].get("messages", [])]
    if subjects:
        result(call(path, "app.act", {"action": "open_message", "args": {"which": subjects[0]}}), "open")
        after = result(call(path, "app.describe", {}), "describe")
        body = (after["state"].get("open_message") or {}).get("body", "")
        print(f"email           : {len(body)} chars of body readable without a screenshot")
        if not body:
            fails.append("email: the open message reported no body")
    # Drafting is offered; sending is not, and that is the point.
    if any(a["name"] == "send" for a in view["actions"]):
        fails.append("email: sending must not be on this surface")

if "calendar" in views:
    path, view = views["calendar"]
    before = view["state"]["month"]
    moved = result(call(path, "app.act", {"action": "show_month", "args": {"direction": "next"}}), "month")
    print(f"calendar        : {before} → {moved['showing'] if moved else '?'}")
    if not moved or moved["showing"] == before:
        fails.append("calendar: show_month did not move the month")
    result(call(path, "app.act", {"action": "go_to_today", "args": {}}), "today")

if "system-monitor" in views:
    path, view = views["system-monitor"]
    st = view["state"]
    top = st.get("top_processes", [])
    print(f"system-monitor  : {st.get('health')}, {len(top)} processes named, {len(st.get('disks', []))} disks")
    if not top:
        fails.append("system-monitor: named no processes")
    elif not all(p.get("pid") and p.get("name") for p in top):
        fails.append("system-monitor: a process had no pid or name")
    # kill_process must be declared dangerous, not ride in as an ordinary view change.
    kill = next((a for a in view["actions"] if a["name"] == "kill_process"), None)
    if not kill or kill.get("permission") != "dangerous":
        fails.append("system-monitor: kill_process is not declared dangerous")
    else:
        print("                : kill_process declared dangerous")

if "weather" in views:
    path, view = views["weather"]
    st = view["state"]
    print(f"weather         : {st.get('location')}, {st.get('temperature')}, units={st.get('units')}")
    set_units = result(call(path, "app.act", {"action": "set_units", "args": {"units": "fahrenheit"}}), "units")
    again = result(call(path, "app.act", {"action": "set_units", "args": {"units": "fahrenheit"}}), "units")
    print(f"                : set_units → {set_units}, asked again → {again}")
    # Both halves matter: it must do what was asked, and asking twice must not undo it.
    if not set_units or set_units.get("units") != "fahrenheit":
        fails.append(f"weather: set_units did not reach fahrenheit — {set_units}")
    if set_units != again:
        fails.append(f"weather: set_units is not idempotent — {set_units} then {again}")
    result(call(path, "app.act", {"action": "set_units", "args": {"units": "celsius"}}), "units")

if "containers" in views:
    path, view = views["containers"]
    st = view["state"]
    print(f"containers      : {st.get('running')} running of {st.get('total')} on {st.get('runtime')}")
    # Removing a container destroys its writable layer; stopping one only interrupts it.
    risks = {a["name"]: a["permission"] for a in view["actions"]}
    if risks.get("remove") != "dangerous" or risks.get("stop") != "sensitive":
        fails.append(f"containers: risks are wrong — {risks}")
    else:
        print(f"                : start={risks['start']}, stop={risks['stop']}, remove={risks['remove']}")

if "terminal" in views:
    path, view = views["terminal"]
    st = view["state"]
    print(f"terminal        : in {st.get('directory')}, last={st.get('last_command')}")
    if not st.get("directory"):
        fails.append("terminal: reported no working directory")
    # This assertion used to be the opposite — that the terminal must publish no way to run
    # commands — and it outlived the decision it encoded. `run` exists deliberately: it runs in the
    # window the person is looking at, which the companion's headless run_command cannot do.
    run = next((a for a in view["actions"] if a["name"] == "run"), None)
    if not run:
        fails.append("terminal: publishes no `run`")
    elif run["permission"] != "sensitive":
        fails.append(f"terminal: run is declared {run['permission']!r}, not sensitive")
    else:
        ran = result(call(path, "app.act", {"action": "run", "args": {"command": "echo yantrik-probe"}}), "run")
        print(f"                : run -> exit {ran.get('exit_code')}, output {ran.get('output','').strip()!r}")
        if ran.get("exit_code") != 0 or "yantrik-probe" not in ran.get("output", ""):
            fails.append(f"terminal: run did not come back with the command's output — {ran}")
        # cd has to persist, or every command after it lands somewhere the caller did not choose.
        result(call(path, "app.act", {"action": "run", "args": {"command": "cd /tmp"}}), "cd")
        moved = result(call(path, "app.describe", {}), "describe")["state"].get("directory")
        if moved != "/tmp":
            fails.append(f"terminal: cd did not persist — still {moved!r}")

if "download-manager" in views:
    path, view = views["download-manager"]
    risks = {a["name"]: a["permission"] for a in view["actions"]}
    settles = {a["name"]: a["settles"] for a in view["actions"]}
    print(f"download-manager: {view['state']['total']} transfers, saving to {view['state']['save_dir']}")

    # Cancelling interrupts work and deletes the partial file; it is not an ordinary view change.
    if risks.get("cancel") != "sensitive":
        fails.append(f"download-manager: cancel is {risks.get('cancel')!r}, not sensitive")
    # Everything that hands work to a thread must say so. A caller that read `add` as settled would
    # report a 4 GB image as downloaded the instant it started — and pause is the subtle one: the
    # flag it sets is read by the worker at its next chunk, not by the call that set it.
    for name in ("add", "resume", "retry", "verify", "pause", "cancel"):
        if settles.get(name) != "later":
            fails.append(f"download-manager: {name} claims it settles {settles.get(name)!r}")

    # A path is not a URL, and saying so is more use than whatever the transport would have said.
    refused = error_of(call(path, "app.act", {"action": "add", "args": {"url": "/etc/passwd"}}))
    print(f"                : refused a path — {refused}")
    if "http" not in refused:
        fails.append(f"download-manager: a non-URL was not explained — {refused!r}")

    # The whole point, end to end: fetch a real file and prove it arrived intact. The expected hash
    # is computed outside this process, so a bug that hashed the wrong bytes twice cannot pass.
    started = result(call(path, "app.act", {
        "action": "add",
        "args": {"url": os.environ["PROBE_URL"], "sha256": os.environ["PROBE_SHA"], "save_dir": DOWNLOAD_DIR},
    }), "add")
    landed, row = started["path"], None
    for _ in range(60):
        row = next((d for d in result(call(path, "app.describe", {}), "describe")["state"]["downloads"]
                    if d["id"] == started["id"]), None)
        if row and row["status"] in ("completed", "failed"):
            break
        time.sleep(0.5)
    print(f"                : {row['status']} {row['size']}, checksum {row.get('checksum')}")
    if row["status"] != "completed":
        fails.append(f"download-manager: the transfer ended {row['status']} — {row.get('error')}")
    elif row.get("checksum") != "pass":
        fails.append(f"download-manager: checksum came back {row.get('checksum')!r}")
    elif not os.path.exists(landed) or os.path.getsize(landed) != 2097152:
        fails.append(f"download-manager: {landed} is not the file it said it wrote")
    else:
        print(f"                : {landed} is on disk, {os.path.getsize(landed)} bytes")

    # Reported success is not the same as the file being right, so hash it here too — this is the
    # check that would catch a surface confidently describing a truncated or wrong file.
    if os.path.exists(landed):
        digest = hashlib.sha256(open(landed, "rb").read()).hexdigest()
        if digest != os.environ["PROBE_SHA"]:
            fails.append("download-manager: the file on disk is not the file that was served")

print()
if fails:
    print("FAILED:")
    for f in fails:
        print("  -", f)
    sys.exit(1)
print(f"PASS: {len(views)} apps describe themselves and take instruction over the bus")
PY
STATUS=$?

kill "$SERVER_PID" 2>/dev/null
rm -rf "$SERVE_DIR" /tmp/yantrik-dl-probe-*
for app in $APPS; do
  pkill -f "$TARGET/yantrik-$app" 2>/dev/null
done
pkill -f "$TARGET/yantrik-container-manager" 2>/dev/null
exit $STATUS
