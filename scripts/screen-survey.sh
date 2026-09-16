#!/usr/bin/env bash
# ═══════════════════════════════════════════════════════════════════════════════════════
# screen-survey.sh — photograph every screen this OS can show, in one run
# ═══════════════════════════════════════════════════════════════════════════════════════
#
# Design review needs the actual pixels. Every defect worth finding in this project has been
# found by looking at a running machine and none of them were visible in the source: a context
# menu that opened in the middle of the window, a desktop that listed itself as an open window,
# a category with no way to reach it, a launcher that resized as you filtered it. Reading a
# .slint file tells you what someone intended.
#
# So this drives a real machine through every screen and brings back a picture of each one.
#
#   ./screen-survey.sh                       # shell screens + settings sections + apps
#   ./screen-survey.sh --only shell          # just the shell's own screens
#   ./screen-survey.sh --out /tmp/survey     # where the pictures land
#
# It photographs with `grim` INSIDE the guest, and falls back to the hypervisor's screendump only
# when the guest cannot be reached.
#
# That order is not a preference, it is a correction. `qm monitor screendump` returns the QEMU
# display surface, which under virtio-gpu is updated by DAMAGE — so a screen change that repaints
# only part of the output comes back as a composite of the new frame and the old one. Surveying
# with it produced pictures of the Notifications header drawn over the Files browser, and the
# Settings body under the Memory title, and neither was real: grim showed both screens rendering
# perfectly. An hour was nearly spent fixing a bug that belonged to the camera.
#
# The hypervisor path is still the right tool for a machine that has no key, no agent and no
# route — it just must not be the default when the guest can answer.
set -uo pipefail

VM_HOST="${VM_HOST:-yantrik@192.168.4.65}"
PVE_HOST="${PVE_HOST:-root@192.168.4.151}"
VMID="${VMID:-520}"
SSH_KEY="${SSH_KEY:-$HOME/.ssh/id_deploy}"
OUT="${OUT:-$(pwd)/survey}"
ONLY="all"

while [ $# -gt 0 ]; do
  case "$1" in
    --out)  OUT="$2"; shift 2 ;;
    --only) ONLY="$2"; shift 2 ;;
    --vmid) VMID="$2"; shift 2 ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

SSH_OPTS="-o StrictHostKeyChecking=no -o BatchMode=yes -o ConnectTimeout=8 -i $SSH_KEY"
mkdir -p "$OUT"

on_vm()  { ssh $SSH_OPTS "$VM_HOST" "PATH=/opt/yantrik/bin:\$PATH $*" 2>/dev/null; }
on_node() { ssh $SSH_OPTS "$PVE_HOST" "$*" 2>/dev/null; }

# One picture. Named for what it is, so the filenames are the review agenda.
shot() {
  local name="$1"

  # The guest's own compositor output, which is the truth about what is on screen.
  if ssh $SSH_OPTS "$VM_HOST"       "XDG_RUNTIME_DIR=/run/user/1000 WAYLAND_DISPLAY=wayland-0 grim -o Virtual-1 /tmp/survey.png"       >/dev/null 2>&1; then
    if scp $SSH_OPTS "$VM_HOST:/tmp/survey.png" "$OUT/$name.png" >/dev/null 2>&1        && [ -s "$OUT/$name.png" ]; then
      echo "   $name"
      return
    fi
  fi

  # Only if the guest could not answer.
  echo "   $name — guest unreachable, falling back to the hypervisor (may be a stale frame)"
  on_node "rm -f /tmp/survey.ppm; echo 'screendump /tmp/survey.ppm' | qm monitor $VMID >/dev/null 2>&1"
  sleep 2
  scp $SSH_OPTS "$PVE_HOST:/tmp/survey.ppm" "$OUT/$name.ppm" >/dev/null 2>&1
  if [ -s "$OUT/$name.ppm" ]; then
    # PPM is what QEMU produces and nothing reads; convert if we can, keep it either way.
    # Whichever python on this box has Pillow. The .ppm is kept when neither does, because a
    # picture in a format you convert later still beats no picture at all.
    for py in python3 python; do
      if "$py" -c "from PIL import Image; Image.open('$OUT/$name.ppm').save('$OUT/$name.png')" 2>/dev/null; then
        rm -f "$OUT/$name.ppm"
        break
      fi
    done
    echo "   $name"
  else
    echo "   $name — FAILED"
  fi
}

settle() { sleep "${1:-3}"; }

# ── The shell's own screens ─────────────────────────────────────────────────────────────
if [ "$ONLY" = "all" ] || [ "$ONLY" = "shell" ]; then
  echo "Shell screens"
  for s in desktop files settings notifications memory system bond personality permissions terminal; do
    on_vm "yos act shell show_screen screen=$s" >/dev/null
    settle 3
    shot "shell-$s"
  done

  echo "Settings sections"
  for sec in appearance ai desktop network accounts privacy system skills harnesses; do
    on_vm "yos act shell show_screen screen=settings section=$sec" >/dev/null
    settle 3
    shot "settings-$sec"
  done

  on_vm "yos act shell show_screen screen=desktop" >/dev/null
fi

# ── The apps ────────────────────────────────────────────────────────────────────────────
# One at a time, so the window on top is the one being judged — the same reason the app-control
# probe starts them one at a time.
if [ "$ONLY" = "all" ] || [ "$ONLY" = "apps" ]; then
  echo "Apps"
  APPS="notes email calendar weather system-monitor container-manager download-manager terminal
        image-viewer music-player network-manager text-editor document-editor spreadsheet
        presentation snippet-manager"
  for app in $APPS; do
    # Verify it was ACCEPTED before photographing. open_app refuses an id it cannot launch, and
    # a survey that ignores that photographs whatever was already on screen and files it under
    # the app's name — which is how a picture of the desktop ended up labelled "text-editor".
    reply="$(on_vm "yos act shell open_app name=$app" 2>&1)"
    case "$reply" in
      *refused*)
        echo "   app-$app — NOT LAUNCHABLE: ${reply#*refused: }"
        continue
        ;;
    esac
    settle 7
    # And that something actually appeared. `open_app` defers, so acceptance is not arrival.
    if ! on_vm "yos describe shell" | grep -qi "$app\|$(echo "$app" | tr '-' ' ')"; then
      # Do not photograph. Whatever is on screen belongs to the previous app, and filing it
      # under this one is worse than having no picture — it is a picture that lies.
      echo "   app-$app — accepted but no window appeared, not photographed"
      continue
    fi
    shot "app-$app"
    ssh $SSH_OPTS "$VM_HOST" "pkill -f '[b]in/yantrik-$app'" >/dev/null 2>&1
    settle 2
  done
fi

echo
echo "$(ls -1 "$OUT"/*.png 2>/dev/null | wc -l) pictures in $OUT"
