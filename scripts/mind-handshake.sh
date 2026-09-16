#!/usr/bin/env bash
# The body, and how to drive it.
#
# yantrik-os is the body; yantrik-mind is in the driving seat. This starts the OS — the shell, not
# a handful of loose app windows — and then reads it back the way a driver would, so the contract
# is demonstrated rather than described.
#
# The entry point is ONE socket: `app-shell`. The shell knows which screen is up, which windows
# are open, which services are alive, and it can launch an app. Everything else follows from
# there: `open_app` starts something, and that something publishes its own `app-<id>` socket.
# A driver does not need to know what is installed — it asks.
#
# Every call is one line of JSON in, one line out, over a Unix socket in $XDG_RUNTIME_DIR/yantrik.
# No client library, no GPU, no vision model. An app that knows which note is open simply says so.
#
# Run from the repo root inside WSL:
#     bash scripts/mind-handshake.sh          # boot the OS and walk the surface
#     bash scripts/mind-handshake.sh --stop   # shut it down
#
# perception-service is separate and optional here: it needs CAP_SYS_ADMIN at startup, so
# demanding sudo from a read-only handshake would be rude. Start it with the sibling script and
# this one will find it.

set -u

TARGET=${TARGET:-/home/yantrik/target-yantrik/debug}
SOCK_DIR=${XDG_RUNTIME_DIR:-/tmp}/yantrik
SHELL_BIN="$TARGET/yantrik-ui"

if [ "${1:-}" = "--stop" ]; then
  # Kill by the pid each app wrote, not by name. `pkill -x yantrik-system-monitor` matches
  # nothing at all: Linux truncates comm to 15 characters, so the process is really called
  # `yantrik-system-` and the exact match silently fails — which is how four apps survived a
  # cleanup that reported success. And never `pkill -f` on a pattern that also matches this
  # script's own command line; that self-match has killed this shell twice.
  pkill -x yantrik-ui 2>/dev/null
  for f in "$SOCK_DIR"/*.pid; do
    [ -e "$f" ] || continue
    pid=$(cat "$f" 2>/dev/null) || continue
    case "$pid" in '' | *[!0-9]*) continue ;; esac
    kill "$pid" 2>/dev/null && echo "  stopped $(basename "$f" .pid) (pid $pid)"
  done
  echo "stopped."
  exit 0
fi

command -v python3 >/dev/null || { echo "FAIL: needs python3 to speak JSON-RPC"; exit 1; }
[ -x "$SHELL_BIN" ] || { echo "FAIL: $SHELL_BIN is not built"; exit 1; }
[ -n "${WAYLAND_DISPLAY:-}" ] || echo "note: no WAYLAND_DISPLAY; the shell will not open a window"

# ── Boot the OS ──
pkill -x yantrik-ui 2>/dev/null
sleep 1
# `setsid` and a detached stdin, not a bare `&`. Backgrounding alone leaves the shell in this
# script's process group, so when the invoking session ends — `wsl.exe -- bash -lc …` returning,
# an ssh disconnect — the OS goes with it. The whole point of this handshake is to leave something
# running that another workspace can reach afterwards, so it has to outlive whoever started it.
setsid "$SHELL_BIN" > /tmp/yantrik-shell.log 2>&1 < /dev/null &
sleep 8

if [ ! -S "$SOCK_DIR/app-shell.sock" ]; then
  echo "FAIL: the shell did not publish a control surface"
  tail -20 /tmp/yantrik-shell.log
  exit 1
fi

SOCK_DIR="$SOCK_DIR" python3 - <<'PY'
import json, os, socket, sys, time

sock_dir = os.environ["SOCK_DIR"]


# perception-service is started with sudo, so the transport picks /run/yantrik rather than the
# user runtime dir. Both are searched: a driver should not have to know which uid started what.
SOCK_DIRS = [sock_dir, "/run/yantrik"]


def socket_path(service):
    for d in SOCK_DIRS:
        p = os.path.join(d, f"{service}.sock")
        if os.path.exists(p):
            return p
    return os.path.join(sock_dir, f"{service}.sock")


def call(service, method, params, timeout=15):
    """One JSON-RPC round trip. This is the entire client — there is no library to install."""
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    s.connect(socket_path(service))
    s.sendall(
        (json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}) + "\n").encode()
    )
    buf = b""
    while not buf.endswith(b"\n"):
        chunk = s.recv(65536)
        if not chunk:
            break
        buf += chunk
    s.close()
    reply = json.loads(buf)
    if "error" in reply:
        raise RuntimeError(reply["error"].get("message", "unknown"))
    return reply["result"]


def show_actions(view, indent="     "):
    for a in view.get("actions", []):
        args = ", ".join(a["parameters"].get("properties", {}).keys())
        print(
            f"{indent}act      : {a['name']}({args})"
            f"  [{a.get('permission', '?')}, settles {a.get('settles', '?')}]"
        )


print("════════ 1. the one socket you need: app-shell ════════")
print("The shell is the body's own account of itself. Start here; everything else is reachable")
print("from it, so a driver never has to know in advance what is installed.\n")

shell = call("app-shell", "app.describe", {})
print(f"  summary  : {shell['summary']}")
print(f"  revision : {shell.get('revision')}")
state = shell.get("state", {})
for key in ("screen", "windows", "services", "do_not_disturb", "status"):
    if key in state:
        rendered = json.dumps(state[key])
        print(f"  {key:9}: {rendered[:150]}{'…' if len(rendered) > 150 else ''}")
show_actions(shell, "  ")
print()

print("════════ 2. navigating: the shell opens an app ════════")
print("`open_app` is how a driver gets anywhere. It launches, or focuses if already running —")
print("so it is safe to call twice, which matters when you reconnect and cannot remember.\n")

before = {f for f in os.listdir(sock_dir) if f.startswith("app-")}
try:
    out = call("app-shell", "app.act", {"action": "open_app", "args": {"name": "notes"}})
    print("  app-shell.open_app(notes) →")
    for key in ("accepted", "action_id", "settled"):
        print(f"     {key:9}: {out.get(key)}")
    print(f"     summary  : {out.get('summary')}")
except Exception as e:
    print(f"  open_app refused: {e}")

# The app needs a moment to bind its own socket. This wait is the honest shape of the thing:
# `open_app` settles when the launch is dispatched, not when the window is up.
for _ in range(20):
    time.sleep(0.5)
    if {f for f in os.listdir(sock_dir) if f.startswith("app-")} - before:
        break

new = sorted({f for f in os.listdir(sock_dir) if f.startswith("app-")} - before)
print(f"\n  new sockets after the launch: {[n[:-5] for n in new] or 'none yet'}")
print("  `settled: false` is the whole point: the launcher spawned a process, and the window")
print("  arrives seconds later or not at all. Until this action declared itself deferred it")
print("  answered `settled: true` while the shell still reported zero windows open — a driver")
print("  reading that would have reported a launch it had only requested.\n")

print("════════ 3. reading an app: exact state, no screenshot ════════")
apps = sorted(
    f[len("app-"):-len(".sock")]
    for f in os.listdir(sock_dir)
    if f.startswith("app-") and f.endswith(".sock") and f != "app-shell.sock"
)
live = {}
for app in apps:
    try:
        live[app] = call(f"app-{app}", "app.describe", {})
    except Exception:
        continue  # a socket file outlives a crashed process; a stale one is not news

for app, view in live.items():
    print(f"  ── {app} ──")
    print(f"     summary  : {view['summary']}")
    print(f"     revision : {view.get('revision')}")
    s = json.dumps(view.get("state", {}))
    print(f"     state    : {s[:150]}{'…' if len(s) > 150 else ''}")
    show_actions(view)
    print()

if not live:
    print("  (nothing else is open yet)\n")

print("════════ 4. the guard: compare and act in one turn ════════")
print("A driver that reads state, decides, then acts has a gap in between in which the person at")
print("the keyboard can type, close the document or switch windows. `expect_revision` closes it —")
print("the comparison runs INSIDE the app's UI-thread closure, its own serialization domain, so")
print("nothing can run between the check and the handler.\n")

try:
    call("app-shell", "app.act", {
        "action": "show_screen",
        "args": {"screen": "desktop"},
        "expect_revision": "0000000000000000",
    })
    print("  !! a stale guard was ACCEPTED — that is a bug; the guard is not working")
except Exception as e:
    print("  acting on revision 0000000000000000 (deliberately wrong):")
    print(f"     refused: {e}\n")
    print("  The refusal names both revisions, so your next read is not blind. Compare a revision")
    print("  yourself and then call `act` and you have rebuilt the race this removes: the revision")
    print("  from a `describe` is a hint about whether to bother; `expect_revision` is the guard,")
    print("  and only the guard is atomic.")
print()

print("════════ 5. the feed: kernel facts, pushed not polled ════════")
try:
    page = call("perception", "perception.since", {"seq": 0})
    obs = page["observations"]
    print(f"  {len(obs)} observations, next_seq {page['next_seq']}, missed {page['missed']}")
    for o in obs[-6:]:
        print(f"     [{o['salience']:.2f}] {o['summary'][:105]}")
    print("\n  Each carries its own salience, so something expensive is spent only where something")
    print("  cheap pointed. `missed` is never silent: a reader that fell off the ring is told the")
    print("  size of its blind spot rather than handed a shorter list that looks complete.")
except Exception as e:
    print(f"  not running ({type(e).__name__}) — start it with scripts/start-perception.sh")
print()

print("════════ how to drive this ════════")
print("""
  One socket to start from, everything else discovered:

      app-shell.sock   app.describe {}                       what is on screen, open, alive
                       app.act {"action":"open_app",
                                "args":{"app":"notes"}}      launch or focus
      app-<id>.sock    app.describe {}                       exact state + revision + actions
                       app.act {"action":…,"args":{…},
                                "expect_revision":"…"}       guarded action
      perception.sock  perception.since {"seq":N,
                                         "wait_ms":30000}    parks on a condvar; costs nothing idle
      a11y.sock        a11y.windows {}                       windows we did NOT write, over AT-SPI

  `perception.since` with wait_ms does not return empty — it waits. A driver that wants to be
  woken is woken, and pays nothing while nothing happens. You should never have to poll to find
  out something happened; if you do, the boundary has leaked.

  a11y.* is the same shape for foreign windows — GTK, Qt, Chromium — read over AT-SPI rather than
  from pixels. The rule the whole surface rests on: semantic for ours, visual for theirs.
""")
PY

echo "The OS is still running. \`bash scripts/mind-handshake.sh --stop\` shuts it down."
