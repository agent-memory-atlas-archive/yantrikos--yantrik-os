#!/usr/bin/env bash
# ═══════════════════════════════════════════════════════════════════════════════
# app-bins.sh — the binaries the workspace's apps build, asked of the manifests
# ═══════════════════════════════════════════════════════════════════════════════
#
# Prints one binary name per line. Exits non-zero if it cannot read the list, because a
# deploy that silently gets no apps reports success and leaves the machine without them —
# the copy loops that consume this skip a binary that is not there.
#
# An app is a workspace member under apps/, and its binaries are the [[bin]] names in its
# Cargo.toml (or the package name, when it lets cargo derive the target from src/main.rs).
# That is the rule deploy.sh, its BUILD_ALL list and scripts/publish-components.sh were
# each expressing by hand: Arcade merged, registered in every shell table, answered on its
# control surface — and `./deploy.sh` never built it and never copied it, because no list
# anybody checked said to. Nobody writes the list down now; a new app under apps/ is built,
# deployed and published by existing code.
#
# The shelf still wins: callers drop what shelved-bins.sh names, the same way they always
# did, so a shelved app's crate stays a member (and keeps compiling) without being shipped.
#
#   deploy/yantrik-os/app-bins.sh                  # one name per line
#   APP_BINS="$(deploy/yantrik-os/app-bins.sh)"
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
WORKSPACE="$PROJECT_ROOT/Cargo.toml"

[ -f "$WORKSPACE" ] || {
  echo "app-bins.sh: cannot find $WORKSPACE — refusing to guess what the apps are" >&2
  exit 1
}

# The members = [ … ] array, keeping only the entries under apps/. The array is the list
# cargo itself builds from; a directory under apps/ that is not a member is not an app.
MEMBERS="$(sed -n '/^members = \[/,/^\]/p' "$WORKSPACE" \
  | sed -n 's|^[[:space:]]*"\(apps/[^"]\+\)".*|\1|p')"

[ -n "$MEMBERS" ] || {
  echo "app-bins.sh: read no apps/ members from $WORKSPACE." >&2
  echo "  Either the workspace has no apps — in which case delete this guard, not the" >&2
  echo "  callers — or the members array changed shape and every packaging script is now" >&2
  echo "  shipping none of them. Failing rather than returning nothing." >&2
  exit 1
}

BINS=""
for member in $MEMBERS; do
  MANIFEST="$PROJECT_ROOT/$member/Cargo.toml"
  [ -f "$MANIFEST" ] || {
    echo "app-bins.sh: $WORKSPACE lists member $member but $MANIFEST is not there" >&2
    exit 1
  }
  # Every [[bin]] target's name. A section runs from its header to the next header; only a
  # line at column zero is a header, so nothing indented is ever read as one.
  names="$(awk '
    /^\[\[bin\]\]/ { inbin = 1; next }
    /^\[/          { inbin = 0 }
    inbin && /^name[ \t]*=[ \t]*"/ {
      sub(/^name[ \t]*=[ \t]*"/, ""); sub(/".*$/, ""); print
    }
  ' "$MANIFEST")"
  if [ -z "$names" ]; then
    # No explicit [[bin]]: cargo names the binary after the package, so read [package]'s
    # name instead.
    names="$(awk '
      /^\[package\]/ { inpkg = 1; next }
      /^\[/          { inpkg = 0 }
      inpkg && /^name[ \t]*=[ \t]*"/ {
        sub(/^name[ \t]*=[ \t]*"/, ""); sub(/".*$/, ""); print; exit
      }
    ' "$MANIFEST")"
  fi
  [ -n "$names" ] || {
    echo "app-bins.sh: read no binary name from $MANIFEST" >&2
    exit 1
  }
  BINS="$BINS$names
"
done

[ -n "$BINS" ] || {
  echo "app-bins.sh: derived no binaries from the apps/ members of $WORKSPACE" >&2
  exit 1
}

printf '%s' "$BINS"
