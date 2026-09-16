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
# Where cargo ACTUALLY puts things, asked of cargo rather than assumed.
#
# This used to read "${CARGO_TARGET_DIR:-$HOME/target-yantrik}/release". On a machine with
# CARGO_TARGET_DIR unset -- which is the normal case, and was the case on the build box --
# cargo writes to $PROJECT_ROOT/target/release while this script packaged $HOME/target-yantrik.
# It built one directory and shipped another, reported "29 binaries" and a green publish, and
# put a release on the server whose app binaries were five hours old. Every UI change in it was
# missing, and nothing anywhere said so.
#
# `cargo metadata` is the authoritative answer: it accounts for the environment variable, for
# build.target-dir in any .cargo/config.toml, and for the default. An explicit TARGET_DIR still
# wins, for the case where someone is packaging binaries built elsewhere on purpose.
resolve_target_dir() {
  if [ -n "${TARGET_DIR:-}" ]; then printf '%s' "$TARGET_DIR"; return; fi
  local d
  d="$(cd "$PROJECT_ROOT" && cargo metadata --format-version 1 --no-deps 2>/dev/null \
       | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"
  [ -n "$d" ] || d="${CARGO_TARGET_DIR:-$PROJECT_ROOT/target}"
  printf '%s/release' "$d"
}
TARGET_DIR="$(resolve_target_dir)"
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
  # Package the directory the build writes to. Not "a directory that usually is it".
  #
  # A first version of this check compared file timestamps against a marker made before the
  # build, and failed when nothing was newer. That is wrong: an up-to-date incremental build
  # legitimately rewrites nothing, and the check turned a correct no-op build into an error.
  # The invariant worth enforcing is not "files changed", it is "the directory being packaged
  # is the directory cargo writes to".
  CARGO_DIR="$(cd "$PROJECT_ROOT" && cargo metadata --format-version 1 --no-deps 2>/dev/null \
               | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')/release"
  if [ -n "$CARGO_DIR" ] && [ "$CARGO_DIR" != "/release" ] && [ "$CARGO_DIR" != "$TARGET_DIR" ]; then
    fail "this build writes to $CARGO_DIR but the packaging step reads $TARGET_DIR.
   Those must be the same directory or the release ships binaries the build never touched —
   which is exactly how a green publish came to contain app binaries five hours old.
   Use --no-build if you mean to package binaries that were built elsewhere."
  fi

  say "Building the workspace"
  # One rustc in this workspace peaks near 14 GB of RSS (Slint macro expansion), so this
  # is the step that decides what machine can build a release at all.
  ( cd "$PROJECT_ROOT" && RUSTFLAGS="-A warnings" cargo build --release --workspace ) \
    || fail "cargo build failed"
fi

[ -d "$TARGET_DIR" ] || fail "no release directory at $TARGET_DIR (set CARGO_TARGET_DIR)"

say "Discovering what the OS is made of"
echo "   from $TARGET_DIR"
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

# The session script. It decides what environment every program on this desktop inherits,
# and it used to live only inside cloud-init's write_files -- written once at provision
# time and unfixable thereafter. Shipping it here is what makes the session updatable.
if [ -f "$SCRIPT_DIR/yantrik-session" ]; then
  cp "$SCRIPT_DIR/yantrik-session" "$ROOT/bin/yantrik-session"
  chmod +x "$ROOT/bin/yantrik-session"
  echo "   + yantrik-session"
else
  fail "missing $SCRIPT_DIR/yantrik-session -- a release without a session does not boot"
fi

cp "$PROJECT_ROOT/config/yantrik-ollama.yaml" "$ROOT/config.yaml" 2>/dev/null \
  || echo "   (no config shipped — the machine will need one)"

# The desktop's own chrome. Without these the compositor runs on stock defaults and draws a
# light-grey title bar, in a font this OS does not use, around every one of its dark apps — the
# most-seen pixels on the machine, and the last ones anybody thought to own. The session script
# installs them on start; they ship here because a release that cannot dress its own windows is
# not a release of this OS.
mkdir -p "$ROOT/share/labwc" "$ROOT/share/fonts"
cp "$PROJECT_ROOT/config/labwc/rc.xml" "$ROOT/share/labwc/rc.xml"
cp "$PROJECT_ROOT/config/labwc/themerc" "$ROOT/share/labwc/themerc"
# The titlebar buttons, which labwc loads from the theme directory in place of its built-in
# six-by-six bitmaps. Rendered by scripts/render-window-buttons.py. Not optional decoration: the
# built-ins lit 64 pixels between the three of them on this palette, which is how a machine
# comes to have no visible way to close a window.
cp "$PROJECT_ROOT/config/labwc/"*.png "$ROOT/share/labwc/" 2>/dev/null \
  || fail "no titlebar button icons in config/labwc — run scripts/render-window-buttons.py"
# What starts with the desktop: the notification daemon and the polkit agent.
cp "$PROJECT_ROOT/config/labwc/autostart" "$ROOT/share/labwc/autostart"
# Barlow is embedded in each app binary, which the compositor cannot read a font out of, so the
# same files also ship loose for fontconfig.
cp "$PROJECT_ROOT/crates/yantrik-design-tokens/slint/fonts/"*.ttf "$ROOT/share/fonts/"
# The applications this OS ships, as .desktop entries.
#
# They existed in the repository and were installed nowhere, so the launcher -- the "all
# applications" view -- listed fourteen shell screens, Chromium, Vim and Print Settings, and
# not one of the sixteen apps this OS is made of. Photographed saying "Search 17 applications"
# with no Notes, no Mail, no Terminal in it.
#
# They go under $ROOT/share so the session can add /opt/yantrik/share to XDG_DATA_DIRS and the
# ordinary freedesktop scan finds them. No root, no writing into /usr, and the same mechanism
# every other application on the machine uses.
mkdir -p "$ROOT/share/applications"
cp "$PROJECT_ROOT"/apps/desktop-files/*.desktop "$ROOT/share/applications/" 2>/dev/null \
  || fail "no .desktop files to ship — the launcher would not list this OS's own apps"
echo "   + $(ls "$ROOT/share/applications" | wc -l) application entries"

echo "   + labwc theme and $(ls "$ROOT/share/fonts" | wc -l) fonts"

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
  # IdentitiesOnly=yes so ssh offers this key and only this key. Without it, ssh tries every
  # key the agent holds first and the server can refuse the connection before the right one is
  # reached — the exact failure that made a homelab host look unreachable earlier in this work.
  SSH_OPTS="-o StrictHostKeyChecking=no -o IdentitiesOnly=yes -o BatchMode=yes -i $SSH_KEY"
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
  #
  # Fetch the channel THIS RUN published to. That sounds obvious; it was not what happened.
  #
  # This used to read the URL out of cloud-init/user-data.yaml, on the reasoning that a
  # publisher and an installer which each derive the name separately will drift apart. The
  # reasoning is right and the implementation was wrong: that file names one specific channel
  # (nightly), so publishing to any other channel downloaded nightly's bundle and compared it
  # against the bundle we had just built somewhere else. A correct publish to `stable` failed
  # verification with a hash that, read against the manifest, turned out to be nightly's.
  #
  # So the URL comes from the channel, and cloud-init is used for what it can actually
  # settle — the host — with a warning rather than a failure if it points somewhere else.
  CI_URL="$(grep -o 'http[^"]*yantrik-os-latest[^"]*' "$SCRIPT_DIR/cloud-init/user-data.yaml" 2>/dev/null | head -1)"
  HOST_URL="$(printf '%s' "$CI_URL" | sed -n 's#^\(https\?://[^/]*\)/.*#\1#p')"
  URL="${HOST_URL:-http://releases.yantrikos.com}/$PUBLISH_CHANNEL/yantrik-os-latest-linux-amd64.$EXT"

  CI_CHANNEL="$(printf '%s' "$CI_URL" | sed -n 's#.*/\([^/]*\)/yantrik-os-latest.*#\1#p')"
  if [ -n "$CI_CHANNEL" ] && [ "$CI_CHANNEL" != "$PUBLISH_CHANNEL" ]; then
    printf '   note: a fresh install follows %s, and this build went to %s\n' \
      "$CI_CHANNEL" "$PUBLISH_CHANNEL"
  fi

  GOT="$(curl -sfL "$URL" | sha256sum | cut -d' ' -f1)" || fail "cannot fetch $URL"
  WANT="$(cut -d' ' -f1 < "$TARBALL.sha256")"
  if [ "$GOT" = "$WANT" ]; then
    echo "   $URL"
    echo "   sha256 matches the artifact that was built"
  else
    fail "served bytes do not match what was built (got $GOT, want $WANT)"
  fi

  # ── Retention ──
  #
  # A nightly channel with no retention is a disk filling at ~240 MB a build; five had
  # accumulated to 1.4 GB before anyone looked. Keep the newest RETAIN bundles and delete the
  # rest, AFTER the verification above — so a publish that turned out to serve the wrong bytes
  # has not already deleted the build that was working.
  #
  # Deliberately by modification time and never by name: the version string contains a date
  # that is the BUILD date, and a rebuild of an old commit would sort itself into the wrong
  # place. Whatever `-latest` points at is protected regardless of age.
  RETAIN="${RELEASE_RETAIN:-3}"
  say "Pruning $PUBLISH_CHANNEL to the newest $RETAIN"
  ssh $SSH_OPTS "root@$RELEASES_IP" "
    cd $REMOTE || exit 0
    KEEP=\$(readlink yantrik-os-latest-linux-amd64.$EXT 2>/dev/null)
    ls -1t *.tar.zst *.tar.gz 2>/dev/null | grep -v '^yantrik-os-latest' | tail -n +\$(($RETAIN + 1)) | while read -r old; do
      [ \"\$old\" = \"\$KEEP\" ] && continue
      rm -f -- \"\$old\" \"\$old.sha256\"
      echo \"   removed \$old\"
    done
    echo \"   \$(ls -1 *.tar.zst *.tar.gz 2>/dev/null | grep -v '^yantrik-os-latest' | wc -l) kept, \$(df -h . | awk 'NR==2{print \$4}') free\"
  " || echo "   (prune skipped)"
fi
