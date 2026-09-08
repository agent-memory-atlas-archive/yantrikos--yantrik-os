#!/usr/bin/env bash
# Bring up perception-service for the mind handshake, then make one atomic save so the feed has
# something real in it. Written to a file rather than passed through `wsl.exe -- bash -lc` because
# every quoting layer between PowerShell and sudo is one more place for an argument to vanish.
set -u

TARGET=/home/yantrik/target-yantrik/debug
LAB=/tmp/mind-lab
# A distinct name: `pkill -f perception-service` matches the shell command that contains the
# string, including its own, which is how the last attempt killed itself with SIGTERM.
BIN="$TARGET/perception-svc-run"

sudo pkill -x perception-svc-run 2>/dev/null
sleep 1
rm -rf "$LAB"
mkdir -p "$LAB/watched"
cp "$TARGET/perception-service" "$BIN"

cat > "$LAB/scope.yaml" <<'YAML'
watch:
  - /tmp/mind-lab/watched
enforce: true
YAML

sudo -b env YANTRIK_PERCEPTION_CONFIG="$LAB/scope.yaml" RUST_LOG=info "$BIN" \
  > "$LAB/svc.log" 2>&1
sleep 4

echo "=== startup ==="
grep -E "landlock|apabilit|fanotify|listening|watching" "$LAB/svc.log" | head -6

# An atomic save: write a scratch file, pause so the daemon resolves it under its scratch name,
# then rename it over the document. This is what every serious editor does, and it is the case a
# single FAN_CLOSE_WRITE group cannot see.
python3 - <<'PY'
import os, time
d = "/tmp/mind-lab/watched"
scratch = os.path.join(d, ".report.odt.swpx")
with open(scratch, "w") as f:
    f.write("the document a person would name")
time.sleep(0.4)
os.rename(scratch, os.path.join(d, "report.odt"))
PY
sleep 2
echo "started."
