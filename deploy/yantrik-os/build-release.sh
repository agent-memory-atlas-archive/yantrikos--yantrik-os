#!/usr/bin/env bash
# ═══════════════════════════════════════════════════════════════════════════════════════
# build-release.sh — package a built workspace into the tarball everything else installs
# ═══════════════════════════════════════════════════════════════════════════════════════
#
# One artifact, three consumers: the ISO unpacks it, cloud-init fetches it, and
# deploy-to-vm.sh could use it instead of rsyncing a directory. Before this existed each
# of those knew its own list of binaries, and the ISO's list was five months out of date —
# it shipped a two-binary OS with none of the agent surface, and booted a desktop that
# looked right and could do nothing.
#
# So the list is DISCOVERED, never written down. Anything executable in the release
# directory is part of the OS; a new service is packaged because it exists, not because
# someone remembered to add it here.
#
#   ./build-release.sh [--out DIR] [--models] [--no-build]
#
# --models includes the embedder and whisper weights (~235 MB). Without it the tarball is
# just code, which is what a machine that already has the models wants.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/target-yantrik}/release"
OUT_DIR="$PROJECT_ROOT/dist"
WITH_MODELS=0
DO_BUILD=1

while [ $# -gt 0 ]; do
  case "$1" in
    --out) OUT_DIR="$2"; shift 2 ;;
    --models) WITH_MODELS=1; shift ;;
    --no-build) DO_BUILD=0; shift ;;
    --publish) PUBLISH_CHANNEL="$2"; shift 2 ;;
    -h|--help) sed -n '2,22p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

say() { printf '\n\033[1m%s\033[0m\n' "$*"; }
fail() { printf '\033[31mFAIL: %s\033[0m\n' "$*" >&2; exit 1; }

VERSION="$(git -C "$PROJECT_ROOT" describe --tags --always --dirty 2>/dev/null)"
[ -n "$VERSION" ] || fail "cannot determine a version — refusing to build an unidentifiable release"
STAMP="$(date -u +%Y%m%d)"
GITREV="$(git -C "$PROJECT_ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)"
NAME="yantrik-os-${VERSION}-${STAMP}-${GITREV}-linux-amd64"

if [ "$DO_BUILD" = 1 ]; then
  say "Building the workspace"
  # One rustc in this workspace peaks near 14 GB of RSS (Slint macro expansion), so this
  # is the step that decides what machine can build a release at all.
  ( cd "$PROJECT_ROOT" && RUSTFLAGS="-A warnings" cargo build --release --workspace ) \
    || fail "cargo build failed"
fi

[ -d "$TARGET_DIR" ] || fail "no release directory at $TARGET_DIR (set CARGO_TARGET_DIR)"

say "Discovering what the OS is made of"
mapfile -t BINS < <(
  find "$TARGET_DIR" -maxdepth 1 -type f -executable \
    ! -name "*.so" ! -name "*.d" ! -name "*.rlib" ! -name "build-script*" ! -name "test-*" ! -name "*-test" ! -name "bench-*" \
    -printf '%f\n' | sort
)
[ "${#BINS[@]}" -gt 0 ] || fail "no binaries found in $TARGET_DIR"
printf '   %s\n' "${BINS[@]}" | paste -sd' ' - | fold -sw 76 | sed 's/^/   /'
echo "   ${#BINS[@]} binaries"

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
ROOT="$STAGE/$NAME"
mkdir -p "$ROOT/bin" "$ROOT/config" "$ROOT/models"

say "Staging"
for b in "${BINS[@]}"; do cp "$TARGET_DIR/$b" "$ROOT/bin/$b"; done

# The agent surface is not compiled, so binary discovery cannot find it. Without these the
# machine boots a desktop that no agent can see or drive — the exact failure the old ISO had.
for f in yos yos-mcp; do
  [ -f "$SCRIPT_DIR/$f" ] || fail "missing $SCRIPT_DIR/$f — the agent surface is not optional"
  cp "$SCRIPT_DIR/$f" "$ROOT/bin/$f"
  chmod +x "$ROOT/bin/$f"
done
echo "   + yos, yos-mcp"

# The updater ships in the image so a machine can update itself. It is a script, not a
# compiled binary, so binary discovery does not find it either — and a machine that cannot
# pull the next build is a machine that gets hand-patched over ssh forever.
if [ -f "$SCRIPT_DIR/yantrik-update" ]; then
  cp "$SCRIPT_DIR/yantrik-update" "$ROOT/bin/yantrik-update"
  chmod +x "$ROOT/bin/yantrik-update"
  echo "   + yantrik-update"
else
  echo "   (no yantrik-update script found — image will not self-update)"
fi

cp "$PROJECT_ROOT/config/yantrik-ollama.yaml" "$ROOT/config.yaml" 2>/dev/null \
  || echo "   (no config shipped — the machine will need one)"

if [ "$WITH_MODELS" = 1 ]; then
  say "Including models"
  for m in embedder whisper; do
    if [ -d "/opt/yantrik/models/$m" ]; then
      cp -r "/opt/yantrik/models/$m" "$ROOT/models/$m"
      echo "   $m ($(du -sh "$ROOT/models/$m" | cut -f1))"
    else
      echo "   $m not present locally — skipped"
    fi
  done
fi

# A manifest, so a running machine can say what it is. "Which build is this?" was
# unanswerable on the VM all day; a version string in a file costs nothing and settles it.
cat > "$ROOT/BUILD" <<EOF
name=$NAME
version=$VERSION
git=$GITREV
built=$(date -u +%Y-%m-%dT%H:%M:%SZ)
binaries=${#BINS[@]}
models=$([ "$WITH_MODELS" = 1 ] && echo included || echo excluded)
EOF

mkdir -p "$OUT_DIR"
TARBALL="$OUT_DIR/${NAME}.tar.zst"
say "Packing"
if command -v zstd >/dev/null 2>&1; then
  tar --zstd -cf "$TARBALL" -C "$STAGE" "$NAME"
else
  TARBALL="$OUT_DIR/${NAME}.tar.gz"
  tar -czf "$TARBALL" -C "$STAGE" "$NAME"
  echo "   zstd not installed — wrote gzip instead"
fi

# A checksum beside the artifact, because "did the download finish" and "is this the build
# I think it is" are the two questions every install path ends up asking.
( cd "$OUT_DIR" && sha256sum "$(basename "$TARBALL")" > "$(basename "$TARBALL").sha256" )

# Stable names, so cloud-init and the ISO can point at one URL forever.
case "$TARBALL" in *.tar.zst) LEXT=tar.zst ;; *.tar.gz) LEXT=tar.gz ;; *) LEXT="${TARBALL##*.}" ;; esac
ln -sf "$(basename "$TARBALL")" "$OUT_DIR/yantrik-os-linux-amd64.$LEXT" 2>/dev/null || true

say "Built"
echo "   $TARBALL"
echo "   $(du -h "$TARBALL" | cut -f1)  ·  $(cut -d= -f2 <<<"$(grep binaries "$ROOT/BUILD")") binaries  ·  $GITREV"

# ── Publishing ─────────────────────────────────────────────────────────────────────────
#
# Only runs with --publish CHANNEL. Kept in this script rather than a separate one because
# the artifact and its checksum are produced here, and a publisher that recomputes either
# can disagree with what was built — which is the kind of difference nobody notices until
# a machine installs something other than what was tested.
if [ -n "${PUBLISH_CHANNEL:-}" ]; then
  RELEASES_IP="${RELEASES_IP:-192.168.4.28}"
  SSH_KEY="${SSH_KEY:-$HOME/.ssh/id_deploy}"
  SSH_OPTS="-o StrictHostKeyChecking=no -i $SSH_KEY"
  REMOTE="/var/www/releases/$PUBLISH_CHANNEL"

  say "Publishing to $PUBLISH_CHANNEL"
  BASE="$(basename "$TARBALL")"
  # `${BASE##*.}` strips only the LAST extension, so a .tar.zst published as .zst — a name
  # nothing asks for. The verification below passed anyway, because it checked the name it
  # had just written rather than the one a consumer uses.
  case "$BASE" in
    *.tar.zst) EXT="tar.zst" ;;
    *.tar.gz)  EXT="tar.gz" ;;
    *)         EXT="${BASE##*.}" ;;
  esac

  ssh $SSH_OPTS "root@$RELEASES_IP" "mkdir -p $REMOTE" || fail "cannot reach $RELEASES_IP"

  # Upload under the dated name first, then move the -latest pointer. A reader that catches
  # the window sees the old build rather than a half-written one.
  scp $SSH_OPTS "$TARBALL" "$TARBALL.sha256" "root@$RELEASES_IP:$REMOTE/" \
    || fail "upload failed"
  ssh $SSH_OPTS "root@$RELEASES_IP" \
    "cd $REMOTE && ln -sf '$BASE' 'yantrik-os-latest-linux-amd64.$EXT' && ln -sf '$BASE.sha256' 'yantrik-os-latest-linux-amd64.$EXT.sha256'" \
    || fail "could not update the latest pointer"

  # The manifest is what a machine reads to answer "is there something newer than me".
  # Updated in place so the other channels keep whatever they were pointing at.
  ssh $SSH_OPTS "root@$RELEASES_IP" "python3 - <<'PYEOF'
import json, os
path = '/var/www/releases/manifest.json'
m = {'channels': {}}
if os.path.exists(path):
    try:
        m = json.load(open(path))
    except Exception:
        pass   # a corrupt manifest should not stop a good build being published
m.setdefault('channels', {})['$PUBLISH_CHANNEL'] = {
    'version': '$VERSION',
    'date': '$(date -u +%Y-%m-%d)',
    'url': '/$PUBLISH_CHANNEL',
    'notes': 'Build $GITREV ($(date -u +%Y-%m-%d))',
    'git': '$GITREV',
    'artifact': '$BASE',
    'sha256': '$(cut -d" " -f1 < "$TARBALL.sha256")',
    'binaries': ${#BINS[@]},
}
json.dump(m, open(path, 'w'), indent=2)
print('  manifest: $PUBLISH_CHANNEL -> $VERSION')
PYEOF" || fail "manifest update failed"

  # Verify by fetching, not by trusting the upload. The checksum is the whole point of
  # publishing one: a 200 with the wrong bytes reads exactly like a 200 with the right ones.
  say "Verifying what is actually being served"
  # The URL cloud-init is configured with, read from the file rather than reconstructed —
  # a publisher and an installer that each derive the name separately can drift apart.
  URL="$(grep -o 'http[^"]*yantrik-os-latest[^"]*' "$SCRIPT_DIR/cloud-init/user-data.yaml" 2>/dev/null | head -1)"
  URL="${URL:-http://releases.yantrikos.com/$PUBLISH_CHANNEL/yantrik-os-latest-linux-amd64.$EXT}"
  GOT="$(curl -sfL "$URL" | sha256sum | cut -d' ' -f1)" || fail "cannot fetch $URL"
  WANT="$(cut -d' ' -f1 < "$TARBALL.sha256")"
  if [ "$GOT" = "$WANT" ]; then
    echo "   $URL"
    echo "   sha256 matches the artifact that was built"
  else
    fail "served bytes do not match what was built (got $GOT, want $WANT)"
  fi
fi
