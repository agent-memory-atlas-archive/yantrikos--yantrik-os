#!/usr/bin/env bash
# Run the conformance suite on the test machine, from a workstation.
#
# The suite itself runs on the machine under test: it reads that machine's processes, its
# compositor and its files, none of which can be seen from here. So this pushes the suite
# across, runs it there, brings the report back, and exits with the code the runner gave.
#
#   tests/conformance/run-on-vm.sh                    every probe
#   tests/conformance/run-on-vm.sh --app calendar     one
#   tests/conformance/run-on-vm.sh --probes-dir selftest --timeout 20
#
# Anything after the script name is passed to `run.py` on the far side.
#
# Reaching the machine goes through two helper scripts rather than an inline ssh command,
# because the nesting here is three shells deep — Git Bash, wsl bash, remote bash — and
# each one eats a layer of quoting. `vmrun.sh` pipes a whole script file over stdin, so
# nothing in between ever parses it.
#
#   YANTRIK_VM_TOOLS   where vmrun.sh and vmpush.sh live
#   YANTRIK_VM         user@host, used only for the plain-ssh fallback
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
TOOLS="${YANTRIK_VM_TOOLS:-/c/Users/sync/tour-frames}"
VM="${YANTRIK_VM:-yantrik@192.168.4.44}"

STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
REMOTE_DIR="/tmp/yantrik-conformance-$STAMP"
REMOTE_TAR="/tmp/yantrik-conformance-$STAMP.tar.gz"
OUT_DIR="$REPO/target/conformance"
OUT_JSON="$OUT_DIR/conformance-$STAMP.json"

WORK="$(mktemp -d)"
SCRIPTS="$(mktemp -d)"
cleanup_local() { rm -rf "$WORK" "$SCRIPTS"; }
trap cleanup_local EXIT

# -- push -------------------------------------------------------------------
vm_run() {
  if [ -x "$TOOLS/vmrun.sh" ]; then
    (cd "$TOOLS" && ./vmrun.sh "$1")
  else
    ssh -o BatchMode=yes -o StrictHostKeyChecking=no "$VM" bash -s < "$1"
  fi
}

vm_push() {
  if [ -x "$TOOLS/vmpush.sh" ]; then
    (cd "$TOOLS" && ./vmpush.sh "$1" "$2")
  else
    scp -o BatchMode=yes -o StrictHostKeyChecking=no "$1" "$VM:$2"
  fi
}

mkdir -p "$WORK/conformance"
for item in lib.py run.py expected-fail.json README.md probes selftest; do
  [ -e "$HERE/$item" ] && cp -r "$HERE/$item" "$WORK/conformance/"
done
find "$WORK/conformance" -name '__pycache__' -type d -prune -exec rm -rf {} + 2>/dev/null

# The image viewer is checked against the repo's own wallpapers, which are known-good
# pictures of known sizes. They are staged into the bundle rather than committed under
# tests/, and the probe falls back to PNGs it writes itself when they are absent.
mkdir -p "$WORK/conformance/fixtures"
for picture in aurora first-light ocean; do
  source_png="$REPO/crates/yantrik-ui-slint/ui/wallpapers/$picture.png"
  [ -f "$source_png" ] && cp "$source_png" "$WORK/conformance/fixtures/"
done

tar czf "$WORK/conformance.tar.gz" -C "$WORK" conformance
echo "pushing $(du -h "$WORK/conformance.tar.gz" | cut -f1) to $VM:$REMOTE_TAR"
vm_push "$WORK/conformance.tar.gz" "$REMOTE_TAR" || { echo "could not reach the machine"; exit 2; }

# -- run --------------------------------------------------------------------
# The arguments are re-quoted one at a time, because the remote bash parses this text
# afresh and an unquoted `--app image viewer` would arrive as two arguments.
ARGS=""
for arg in "$@"; do ARGS="$ARGS $(printf '%q' "$arg")"; done

cat > "$SCRIPTS/run.sh" <<EOF
set -u
export XDG_RUNTIME_DIR=/run/user/1000
export WAYLAND_DISPLAY=wayland-0
export PATH=/opt/yantrik/bin:\$PATH
rm -rf "$REMOTE_DIR"
mkdir -p "$REMOTE_DIR"
tar xzf "$REMOTE_TAR" -C "$REMOTE_DIR" --strip-components=1
cd "$REMOTE_DIR"
python3 run.py --json "$REMOTE_DIR/report.json"$ARGS
exit \$?
EOF

vm_run "$SCRIPTS/run.sh"
RC=$?

# -- fetch ------------------------------------------------------------------
mkdir -p "$OUT_DIR"
# `--list` and a runner that died before writing anything leave no report. Saving an
# empty or invented one would put a file in target/ that says less than nothing.
cat > "$SCRIPTS/fetch.sh" <<EOF
[ -f "$REMOTE_DIR/report.json" ] || exit 9
cat "$REMOTE_DIR/report.json"
EOF
if vm_run "$SCRIPTS/fetch.sh" > "$OUT_JSON" && [ -s "$OUT_JSON" ]; then
  echo "report saved: $OUT_JSON"
else
  rm -f "$OUT_JSON"
  echo "no report came back from the machine (nothing was run, or the runner died first)"
fi

# -- leave the machine as it was found --------------------------------------
cat > "$SCRIPTS/tidy.sh" <<EOF
rm -rf "$REMOTE_DIR" "$REMOTE_TAR"
echo "scratch removed; what is left under /tmp:"
ls -d /tmp/yantrik-conformance-* 2>/dev/null || echo "  nothing"
EOF
vm_run "$SCRIPTS/tidy.sh"

exit $RC
