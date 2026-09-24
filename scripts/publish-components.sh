#!/bin/bash
# Publish built component binaries to bin.yantrikos.com
# Usage: ./scripts/publish-components.sh [channel] [--skip-build]
#
# Builds all components via WSL, packages them as tarballs,
# and uploads to the binary registry.

set -euo pipefail

CHANNEL="${1:-nightly}"
SKIP_BUILD=false
if [ "${2:-}" = "--skip-build" ] || [ "${1:-}" = "--skip-build" ]; then
    SKIP_BUILD=true
    [ "${1:-}" = "--skip-build" ] && CHANNEL="nightly"
fi

REGISTRY_HOST="192.168.4.28"
REGISTRY_ROOT="/var/www/bin"
WSL_TARGET="/home/yantrik/target-yantrik"
WSL_SRC="/home/yantrik/src/yantrik-os"
WIN_SRC="/mnt/c/Users/sync/codes/yantrik-os"
TARGET_TRIPLE="x86_64-unknown-linux-gnu"
SSH_KEY="$HOME/.ssh/id_deploy"
PROXMOX_HOST="192.168.4.152"
CT_ID="124"

# Colors
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
NC='\033[0m'

step() { echo -e "${GREEN}==> $1${NC}"; }
warn() { echo -e "${YELLOW}    $1${NC}"; }
fail() { echo -e "${RED}!!! $1${NC}"; exit 1; }

# ── What this build does not publish ──
#
# The shelf is SHELVED in crates/yantrik-ui/src/wire/dock.rs; deploy/yantrik-os/shelved-bins.sh
# reads it. The component tables below used to name the shelved apps, so the component registry
# served Music and ySheets to every machine that pulled from it while the release tarball, built
# from the same tree on the same day by build-release.sh, deliberately left them out. Two
# answers to "what is this OS made of", from one repository.
PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SHELVED_BINS="$("$PROJECT_ROOT/deploy/yantrik-os/shelved-bins.sh" | paste -sd' ' -)" \
    || fail "cannot determine which apps are shelved"
is_shelved() {
    local b
    for b in $SHELVED_BINS; do [ "$1" = "$b" ] && return 0; done
    return 1
}

# Component definitions: name -> binary name in target/release/
declare -A CORE_COMPONENTS=(
    ["yantrik-ui"]="yantrik-ui"
    ["yantrik"]="yantrik"
)

# ── Which services this publishes ──
#
# The services table was written down here too, and it went stale the same way
# deploy.sh's did: a11y and perception were missing from the table, from the build's
# -p flags and from the manifest's binary_names, so a channel published with this
# script shipped a shell that registers both (start_services in
# crates/yantrik-ui/src/main.rs) with no binaries behind them — `yos describe`
# promised services nothing on the machine could answer.
# deploy/yantrik-os/service-bins.sh reads the services' own yantrik.toml manifests, so
# the three shapes this script needs are derived from one asking, shelf dropped as
# before.
SERVICE_BINS="$("$PROJECT_ROOT/deploy/yantrik-os/service-bins.sh")" \
    || fail "cannot determine which services this tree builds"
declare -A SERVICE_COMPONENTS=()
SERVICE_PACKAGES=""
MANIFEST_SERVICES=""
for b in $SERVICE_BINS; do
    if is_shelved "$b"; then continue; fi
    SERVICE_COMPONENTS["$b"]="$b"
    SERVICE_PACKAGES="$SERVICE_PACKAGES -p $b"
    MANIFEST_SERVICES="$MANIFEST_SERVICES'$b': '$b', "
done

# ── Which apps this publishes ──
#
# The apps table used to be written down here, and it went stale the same way deploy.sh's
# did: Arcade merged and answered on its control surface, but the registry never served it
# and the channel manifest never listed it, because nothing checked this copy against the
# workspace either. deploy/yantrik-os/app-bins.sh reads the apps/ members of Cargo.toml, so
# the three shapes this script needs — the component table, the cargo `-p` flags and the
# manifest's python dict — are all derived from one asking, shelf dropped as before.
APP_BINS="$("$PROJECT_ROOT/deploy/yantrik-os/app-bins.sh")" \
    || fail "cannot determine which apps this tree builds"
declare -A APP_COMPONENTS=()
APP_PACKAGES=""
MANIFEST_APPS=""
for b in $APP_BINS; do
    if is_shelved "$b"; then continue; fi
    APP_COMPONENTS["$b"]="$b"
    APP_PACKAGES="$APP_PACKAGES -p $b"
    MANIFEST_APPS="$MANIFEST_APPS'$b': '$b', "
done

# Get version from workspace Cargo.toml
get_version() {
    local component="$1"
    # Try component-specific version first, fall back to workspace
    local ver
    ver=$(wsl.exe -d Ubuntu -- bash -lc "cd $WSL_SRC && cargo metadata --format-version=1 --no-deps 2>/dev/null | python3 -c \"import sys,json; pkgs=json.load(sys.stdin)['packages']; matches=[p for p in pkgs if p['name']=='$component']; print(matches[0]['version'] if matches else '0.1.0')\" 2>/dev/null") || ver="0.1.0"
    echo "$ver" | tr -d '\r\n'
}

# Build all components
if [ "$SKIP_BUILD" = false ]; then
    step "Syncing source to native FS..."
    wsl.exe -d Ubuntu -- bash -lc \
        "rsync -a --delete $WIN_SRC/ $WSL_SRC/ \
            --exclude target --exclude .git/objects --exclude .claude/worktrees \
            --exclude '*.gguf' --exclude training/"

    step "Building all components (release)..."
    wsl.exe -d Ubuntu -- bash -lc \
        "cd $WSL_SRC && \
         export RUSTC_WRAPPER=sccache 2>/dev/null || true && \
         RUSTFLAGS=\"-A warnings\" CARGO_TARGET_DIR=$WSL_TARGET \
         cargo build --release \
            -p yantrik-ui -p yantrik \
            $SERVICE_PACKAGES \
            $APP_PACKAGES \
         2>&1" || fail "Build failed!"
fi

# Package and upload each component
step "Packaging and uploading components..."
STAGING="/tmp/yantrik-publish-$$"

publish_component() {
    local name="$1"
    local binary="$2"
    local version
    version=$(get_version "$name")

    wsl.exe -d Ubuntu -- bash -lc "
        STAGING=$STAGING/$name
        mkdir -p \$STAGING
        BIN=$WSL_TARGET/release/$binary
        if [ ! -f \$BIN ]; then
            echo 'SKIP: $name (binary not found)'
            exit 0
        fi

        # Copy binary
        cp \$BIN \$STAGING/

        # Create metadata
        GIT_SHA=\$(cd $WSL_SRC && git rev-parse --short HEAD 2>/dev/null || echo 'unknown')
        RUSTC_VER=\$(rustc --version 2>/dev/null | awk '{print \$2}' || echo 'unknown')
        SIZE=\$(stat -c%s \$BIN)
        SHA256=\$(sha256sum \$BIN | awk '{print \$1}')

        cat > \$STAGING/metadata.json << METAEOF
{
  \"name\": \"$name\",
  \"version\": \"$version\",
  \"target\": \"$TARGET_TRIPLE\",
  \"git_sha\": \"\$GIT_SHA\",
  \"rustc\": \"\$RUSTC_VER\",
  \"size\": \$SIZE,
  \"sha256\": \"\$SHA256\",
  \"built\": \"\$(date -u +%Y-%m-%dT%H:%M:%SZ)\"
}
METAEOF

        # Create tarball
        cd $STAGING/..
        tar -cf - $name/ | zstd -3 -o /tmp/$name-$version-$TARGET_TRIPLE.tar.zst
        echo 'OK: $name v$version (\$SHA256)'
    " 2>&1

    # Upload via proxmox pct push
    local tarball="/tmp/$name-$version-$TARGET_TRIPLE.tar.zst"
    wsl.exe -d Ubuntu -- bash -lc "
        # Upload tarball to releases server via SCP through proxmox
        # Since we can't SSH directly, we use a temp file approach
        cat /tmp/$name-$version-$TARGET_TRIPLE.tar.zst
    " > "/tmp/yantrik-$name.tar.zst" 2>/dev/null

    # Upload to registry via proxmox
    ssh -i "$SSH_KEY" -o StrictHostKeyChecking=no root@"$PROXMOX_HOST" \
        "pct exec $CT_ID -- mkdir -p $REGISTRY_ROOT/components/$name/$version/" 2>/dev/null

    cat "/tmp/yantrik-$name.tar.zst" | ssh -i "$SSH_KEY" -o StrictHostKeyChecking=no root@"$PROXMOX_HOST" \
        "pct exec $CT_ID -- tee $REGISTRY_ROOT/components/$name/$version/$TARGET_TRIPLE.tar.zst > /dev/null" 2>/dev/null

    rm -f "/tmp/yantrik-$name.tar.zst"
}

# Publish all components
for name in "${!CORE_COMPONENTS[@]}"; do
    if is_shelved "${CORE_COMPONENTS[$name]}"; then warn "shelved, not published: $name"; continue; fi
    publish_component "$name" "${CORE_COMPONENTS[$name]}"
done
for name in "${!SERVICE_COMPONENTS[@]}"; do
    if is_shelved "${SERVICE_COMPONENTS[$name]}"; then warn "shelved, not published: $name"; continue; fi
    publish_component "$name" "${SERVICE_COMPONENTS[$name]}"
done
for name in "${!APP_COMPONENTS[@]}"; do
    if is_shelved "${APP_COMPONENTS[$name]}"; then warn "shelved, not published: $name"; continue; fi
    publish_component "$name" "${APP_COMPONENTS[$name]}"
done

# Generate channel manifest
step "Generating $CHANNEL channel manifest..."
wsl.exe -d Ubuntu -- bash -lc "
    cd $WSL_SRC
    GIT_SHA=\$(git rev-parse --short HEAD 2>/dev/null || echo 'unknown')

    python3 -c \"
import json, subprocess, os

components = {}
binary_names = {
    'yantrik-ui': 'yantrik-ui', 'yantrik': 'yantrik',
    $MANIFEST_SERVICES
    $MANIFEST_APPS
}

target = 'x86_64-unknown-linux-gnu'
target_dir = '$WSL_TARGET/release'

for name, binary in binary_names.items():
    bin_path = os.path.join(target_dir, binary)
    if os.path.exists(bin_path):
        import hashlib
        sha = hashlib.sha256(open(bin_path, 'rb').read()).hexdigest()
        size = os.path.getsize(bin_path)
        # Get version from cargo metadata
        try:
            meta = json.loads(subprocess.check_output(['cargo', 'metadata', '--format-version=1', '--no-deps'], cwd='$WSL_SRC', stderr=subprocess.DEVNULL))
            ver = next((p['version'] for p in meta['packages'] if p['name'] == name), '0.1.0')
        except:
            ver = '0.1.0'

        components[name] = {
            'version': ver,
            'target': target,
            'url': f'http://bin.yantrikos.com/components/{name}/{ver}/{target}.tar.zst',
            'sha256': sha,
            'size': size,
        }

manifest = {
    'channel': '$CHANNEL',
    'date': '$(date -u +%Y-%m-%d)',
    'git_sha': '\$GIT_SHA',
    'target': target,
    'components': components,
}
print(json.dumps(manifest, indent=2))
\"
" > /tmp/yantrik-channel-manifest.json 2>/dev/null

# Upload manifest
cat /tmp/yantrik-channel-manifest.json | ssh -i "$SSH_KEY" -o StrictHostKeyChecking=no root@"$PROXMOX_HOST" \
    "pct exec $CT_ID -- tee $REGISTRY_ROOT/channels/$CHANNEL.json > /dev/null" 2>/dev/null

step "Published to $CHANNEL channel on bin.yantrikos.com"

# Verify
step "Verifying..."
wsl.exe -d Ubuntu -- bash -lc "curl -s http://bin.yantrikos.com/channels/$CHANNEL.json | python3 -m json.tool | head -20" 2>&1

step "Done. Components published to bin.yantrikos.com/$CHANNEL"
