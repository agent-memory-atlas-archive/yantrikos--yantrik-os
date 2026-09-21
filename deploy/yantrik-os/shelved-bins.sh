#!/usr/bin/env bash
# ═══════════════════════════════════════════════════════════════════════════════════════
# shelved-bins.sh — the binaries this build does not ship, asked of the one place that decides
# ═══════════════════════════════════════════════════════════════════════════════════════
#
# Prints one binary name per line. Exits non-zero if it cannot read the list, because a
# packaging script that silently gets an empty shelf ships the shelved apps — which is the
# failure this file exists to stop.
#
# The shelf is SHELVED in crates/yantrik-ui/src/wire/dock.rs: each entry carries the app's
# names, the binary it would run, why it is not in this build, and what would bring it back.
# tests/app-lints/shelved.toml is the lint's view of the same decision and a test in dock.rs
# fails if the two stop agreeing; design/shelved-2026-09-20.md is the account.
#
# Five scripts used to carry their own copy of this list — build-release.sh, deploy.sh,
# install.sh, scripts/package-all.sh and scripts/publish-components.sh — and four of them
# still named the shelved apps, so `./deploy.sh` put Music and ySheets back on a machine that
# the shipped build refuses to open. A list written down in five places is a list that is
# wrong in four of them. Nobody writes it down now.
#
#   deploy/yantrik-os/shelved-bins.sh            # one name per line
#   SHELVED_BINS="$(deploy/yantrik-os/shelved-bins.sh | paste -sd' ' -)"
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
DOCK="$PROJECT_ROOT/crates/yantrik-ui/src/wire/dock.rs"

[ -f "$DOCK" ] || {
  echo "shelved-bins.sh: cannot find $DOCK — refusing to guess what is shelved" >&2
  exit 1
}

# Only the VALUES in the SHELVED table, never the struct's field declaration: `pub binary:`
# is a type, `binary: "yantrik-music-player"` is a decision. The leading whitespace and the
# absence of `pub` is what tells them apart.
BINS="$(sed -n 's/^[[:space:]]\+binary:[[:space:]]*"\([^"]\+\)".*/\1/p' "$DOCK")"

[ -n "$BINS" ] || {
  echo "shelved-bins.sh: read no shelved binaries from $DOCK." >&2
  echo "  Either the shelf is empty — in which case delete this guard, not the callers —" >&2
  echo "  or the SHELVED table changed shape and every packaging script is now shipping" >&2
  echo "  apps the launcher refuses to open. Failing rather than returning nothing." >&2
  exit 1
}

printf '%s\n' "$BINS"
