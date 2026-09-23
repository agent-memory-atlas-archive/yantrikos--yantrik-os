#!/usr/bin/env bash
# ═══════════════════════════════════════════════════════════════════════════════════════
# shipped-desktop-files.sh — the .desktop entries a build installs, one path per line
# ═══════════════════════════════════════════════════════════════════════════════════════
#
# Every entry in apps/desktop-files except a shelved app's. The shelf is asked of
# shelved-bins.sh (which reads SHELVED in crates/yantrik-ui/src/wire/dock.rs), and an entry is
# a shelved app's when its Exec line runs a shelved binary. That rule lived inline in
# build-release.sh; deploy-to-vm.sh needs the same answer, and a rule written down in two places
# is a rule that drifts in one of them — so both ask this.
#
# The entries are what the shell finds this OS's own apps by now: their X-Yantrik-Surface,
# X-Yantrik-Purpose and X-Yantrik-Aliases keys are the only record of which surface each app
# publishes, what it is for and what else it is called (docs/app-control.md, "Findable while
# closed"). A machine that gets the binaries without them opens our apps only by their display
# name and lists none of them while closed.
#
# Each skipped entry is named on stderr. Exits non-zero if the shelf cannot be read or nothing
# is left to ship, because either would mean installing the wrong set without noticing.
#
#   deploy/yantrik-os/shipped-desktop-files.sh           # one path per line
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

SHELVED_BINS="$("$SCRIPT_DIR/shelved-bins.sh")"

shipped=0
for f in "$PROJECT_ROOT"/apps/desktop-files/*.desktop; do
  [ -f "$f" ] || continue
  shelf=0
  for b in $SHELVED_BINS; do
    if grep -q "^Exec=.*$b" "$f"; then shelf=1; fi
  done
  if [ "$shelf" = 1 ]; then
    echo "   (shelved, not installed: $(basename "$f"))" >&2
    continue
  fi
  printf '%s\n' "$f"
  shipped=$((shipped + 1))
done

[ "$shipped" -gt 0 ] || {
  echo "shipped-desktop-files.sh: no .desktop files to ship from $PROJECT_ROOT/apps/desktop-files" >&2
  exit 1
}
