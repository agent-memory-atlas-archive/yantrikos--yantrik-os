#!/usr/bin/env bash
# ═══════════════════════════════════════════════════════════════════════════════════════
# deploy-release.sh — DEPRECATED. Use deploy/yantrik-os/build-release.sh --publish instead.
# ═══════════════════════════════════════════════════════════════════════════════════════
#
# This script published a release by uploading exactly two binaries — yantrik-ui and yantrik —
# and a checksum file computed over twenty. A machine that installed from it got a desktop with
# none of the services or apps behind it: the two-binary OS that shipped for months and could
# do nothing. The real publisher discovers every binary the workspace builds, packs them into
# one verified tar.zst, writes the manifest the updater reads, and moves the -latest pointer
# atomically. There is no reason to keep a second, wrong path to the same shelf.
#
# It forwards to the real one so old habits and old docs still do the right thing. It does not
# reimplement anything, on purpose: a publisher that recomputes the artifact can disagree with
# the one that built it, and that difference is exactly how a machine ends up installing
# something other than what was tested.

set -euo pipefail

CHANNEL="${1:-}"
shift || true

BUILD_ARGS=()
for arg in "$@"; do
  case "$arg" in
    --skip-build) BUILD_ARGS+=(--no-build) ;;   # same intent, the new flag's name
    *) BUILD_ARGS+=("$arg") ;;
  esac
done

if [ -z "$CHANNEL" ]; then
  echo "usage: $0 <nightly|beta|stable> [--skip-build]" >&2
  echo "note:  deploy-release.sh is deprecated — this forwards to build-release.sh --publish" >&2
  exit 2
fi

printf '\033[33mdeploy-release.sh is deprecated; forwarding to build-release.sh --publish %s\033[0m\n' "$CHANNEL" >&2

HERE="$(cd "$(dirname "$0")" && pwd)"
exec "$HERE/deploy/yantrik-os/build-release.sh" --publish "$CHANNEL" "${BUILD_ARGS[@]}"
