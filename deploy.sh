#!/bin/bash
# Build and deploy Yantrik OS within WSL Ubuntu
# Usage: ./deploy.sh [--skip-build] [--debug|--fast]
#
# Prerequisites:
#   - WSL2 with Ubuntu and Rust toolchain
#   - sccache installed (cargo install sccache)

set -euo pipefail

REMOTE_BIN="/opt/yantrik/bin"
WSL_TARGET="/home/yantrik/target-yantrik"
WSL_SRC="/home/yantrik/src/yantrik-os"
WIN_SRC="/mnt/c/Users/sync/codes/yantrik-os"

# Colors
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
NC='\033[0m'

step() { echo -e "${GREEN}==> $1${NC}"; }
warn() { echo -e "${YELLOW}    $1${NC}"; }
fail() { echo -e "${RED}!!! $1${NC}"; exit 1; }

# ── What this build does not ship ──
#
# The shelf is SHELVED in crates/yantrik-ui/src/wire/dock.rs and deploy/yantrik-os/shelved-bins.sh
# reads it. Before that existed this script carried its own copy of the app list and nobody
# updated it, so `./deploy.sh` compiled Music and ySheets and copied them into /opt/yantrik/bin
# on a machine whose launcher refuses to open either — the tile reappears, the click does
# nothing, and the release tarball and the dev machine disagree about what the OS is.
SHELVED_BINS="$("$(cd "$(dirname "$0")" && pwd)/deploy/yantrik-os/shelved-bins.sh" | paste -sd' ' -)" \
    || fail "cannot determine which apps are shelved"
# Drops shelved names from a list of binaries, and `-p name` pairs from a list of cargo
# arguments. Whitespace (including the backslash-newlines the lists below are written with)
# is flattened to single spaces first, so one rule handles both shapes.
drop_shelved() {
    local s b
    s="$(printf '%s' "$1" | tr '\n\\\t' '   ' | tr -s ' ')"
    for b in $SHELVED_BINS; do
        # Padded both ends so the first and last entries match the same rule as the middle.
        s="$(printf ' %s ' "$s" | sed "s/ -p $b / /g; s/ $b / /g")"
    done
    printf '%s' "$s" | tr -s ' ' | sed 's/^ *//; s/ *$//'
}

# ── Which apps this tree builds and deploys ──
#
# The app list used to be written down here twice — once in the packages to build and again in
# the binaries to copy — and nothing checked either copy against the workspace. Arcade merged,
# registered in every shell table, answered on its control surface, and `./deploy.sh` still
# reported success without it: no package built it, and the copy loop skips a binary that is
# not there. deploy/yantrik-os/app-bins.sh reads the apps/ members of Cargo.toml instead, so
# an app added tomorrow is built and copied by this script with no edit to it.
APP_BINS="$("$(cd "$(dirname "$0")" && pwd)/deploy/yantrik-os/app-bins.sh")" \
    || fail "cannot determine which apps this tree builds"
APPS="$(drop_shelved "$APP_BINS")"
# The same set as cargo wants it: one `-p name` per app, shelf already dropped.
APP_PACKAGES=""
for app in $APPS; do APP_PACKAGES="$APP_PACKAGES -p $app"; done

# ── Which services this tree builds and deploys ──
#
# The service list was written down here too — once in the packages to build, twice
# because BUILD_ALL=1 chose its own longer-sounding list, and a third time in the copy
# loop below — and the copies disagreed: BUILD_ALL, the branch meant to build more,
# built two services fewer than the default. A deploy that ran it left the shell
# registering services whose binaries were never built, and nothing said so, because
# the copy loop skips a binary that is not there. deploy/yantrik-os/service-bins.sh
# reads the services' own yantrik.toml manifests — the same ones start_services scans —
# so a service added tomorrow is built and copied with no edit to this script, and
# BUILD_ALL has no shorter list left to choose.
SERVICE_BINS="$("$(cd "$(dirname "$0")" && pwd)/deploy/yantrik-os/service-bins.sh")" \
    || fail "cannot determine which services this tree builds"
SERVICES="$(drop_shelved "$SERVICE_BINS")"
# The same set as cargo wants it: one `-p name` per service, shelf already dropped.
SERVICE_PACKAGES=""
for svc in $SERVICES; do SERVICE_PACKAGES="$SERVICE_PACKAGES -p $svc"; done

# Determine build profile
PROFILE="release"
PROFILE_FLAG="--release"
if [ "${1:-}" = "--debug" ] || [ "${2:-}" = "--debug" ]; then
    PROFILE="debug"
    PROFILE_FLAG=""
fi
# --fast: the iteration profile from Cargo.toml — workspace crates unoptimised + incremental,
# dependencies at opt-level 3, so it draws at release speed but rebuilds in a fraction of the
# time. Use it while iterating on the UI; ship with the default release build.
if [ "${1:-}" = "--fast" ] || [ "${2:-}" = "--fast" ]; then
    PROFILE="fast"
    PROFILE_FLAG="--profile fast"
fi

# Step 1: Build via WSL2
if [ "${1:-}" != "--skip-build" ]; then
    # The engine is a git dependency pinned in Cargo.toml now, so there is no sibling repo to
    # sync: this step used to copy ../yantrikdb across first, because cargo could not load the
    # manifest without it.
    step "Syncing source to native FS..."
    wsl.exe -d Ubuntu -- bash -lc \
        "rsync -a --checksum --delete $WIN_SRC/ $WSL_SRC/ \
            --exclude target --exclude .git/objects --exclude .claude/worktrees \
            --exclude '*.gguf' --exclude training/"

    # Determine packages to build. The apps come from APP_PACKAGES and the services from
    # SERVICE_PACKAGES, both derived above. BUILD_ALL=1 used to switch to a hand-written
    # "everything" list of services that was in fact shorter than the default list beside
    # it; with one list asked of the tree, every build builds the same full set and the
    # flag has nothing left to choose.
    PACKAGES="-p yantrik-ui -p yantrik $SERVICE_PACKAGES $APP_PACKAGES"
    step "Building all packages ($PROFILE) via WSL2..."

    wsl.exe -d Ubuntu -- bash -lc \
        "cd $WSL_SRC && \
         export RUSTC_WRAPPER=sccache && \
         CARGO_TARGET_DIR=$WSL_TARGET \
         cargo build $PROFILE_FLAG $PACKAGES 2>&1"

    # Verify binaries exist
    wsl.exe -d Ubuntu -- bash -lc \
        "test -f $WSL_TARGET/$PROFILE/yantrik-ui && \
         test -f $WSL_TARGET/$PROFILE/yantrik" \
        || fail "Build claimed success but binaries not found!"

    step "Build succeeded."
else
    step "Skipping build (--skip-build)"
fi

# Step 2: Deploy binaries within WSL
step "Deploying binaries..."
wsl.exe -d Ubuntu -- bash -lc "
    sudo mkdir -p $REMOTE_BIN &&
    sudo cp $WSL_TARGET/$PROFILE/yantrik-ui $REMOTE_BIN/yantrik-ui &&
    sudo cp $WSL_TARGET/$PROFILE/yantrik    $REMOTE_BIN/yantrik &&
    sudo chmod +x $REMOTE_BIN/yantrik-ui $REMOTE_BIN/yantrik
" || fail "Failed to deploy binaries."

# Step 2a: Deploy service binaries — the SERVICES list derived above, shelf dropped
step "Deploying services..."
wsl.exe -d Ubuntu -- bash -lc "
    for svc in $SERVICES; do
        if [ -f $WSL_TARGET/$PROFILE/\$svc ]; then
            sudo cp $WSL_TARGET/$PROFILE/\$svc $REMOTE_BIN/\$svc &&
            sudo chmod +x $REMOTE_BIN/\$svc
        fi
    done
" || warn "Failed to deploy some services (non-fatal)"

# Step 2a2: Deploy app binaries
step "Deploying apps..."
wsl.exe -d Ubuntu -- bash -lc "
    for app in $APPS; do
        if [ -f $WSL_TARGET/$PROFILE/\$app ]; then
            sudo cp $WSL_TARGET/$PROFILE/\$app $REMOTE_BIN/\$app &&
            sudo chmod +x $REMOTE_BIN/\$app
        fi
    done
" || warn "Failed to deploy some apps (non-fatal)"

# Step 2b: Deploy i18n translation files
I18N_SRC="$WSL_SRC/crates/yantrik-ui/i18n"
wsl.exe -d Ubuntu -- bash -lc "
    if [ -d '$I18N_SRC' ]; then
        sudo mkdir -p $REMOTE_BIN/i18n &&
        sudo cp $I18N_SRC/*.yaml $REMOTE_BIN/i18n/ 2>/dev/null
    fi
" || warn "Failed to deploy i18n files (non-fatal)"

# Step 2c: Deploy skill manifests
SKILLS_SRC="$WSL_SRC/skills"
wsl.exe -d Ubuntu -- bash -lc "
    if [ -d '$SKILLS_SRC' ]; then
        sudo mkdir -p /opt/yantrik/skills &&
        sudo cp $SKILLS_SRC/*.yaml /opt/yantrik/skills/ 2>/dev/null
    fi
" || warn "Failed to deploy skill manifests (non-fatal)"

# Step 2d: Deploy .desktop files
DESKTOP_SRC="$WSL_SRC/apps/desktop-files"
wsl.exe -d Ubuntu -- bash -lc "
    if [ -d '$DESKTOP_SRC' ]; then
        sudo mkdir -p /usr/share/applications &&
        sudo cp $DESKTOP_SRC/*.desktop /usr/share/applications/ 2>/dev/null;
        # An installed .desktop entry is all the launcher needs to list an app, so a shelved
        # app's entry puts the tile back with nothing behind the click.
        for b in $SHELVED_BINS; do sudo rm -f /usr/share/applications/\$b.desktop; done
    fi
" || warn "Failed to deploy .desktop files (non-fatal)"

# Step 3: Restart yantrik-ui
step "Restarting yantrik-ui..."
wsl.exe -d Ubuntu -- bash -lc \
    "pgrep -f '/opt/yantrik/bin/yantrik-ui' | xargs -r kill 2>/dev/null; sleep 2; pgrep -f '/opt/yantrik/bin/yantrik-ui' | xargs -r kill -9 2>/dev/null" \
    || true
sleep 1

# Start new process
wsl.exe -d Ubuntu -- bash -lc \
    "sudo -u yantrik bash -c '
        WAYLAND_DISPLAY=wayland-0 \
        XDG_RUNTIME_DIR=/run/user/1000 \
        DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus \
        # renderer chosen by yantrik-ui::render_backend — do not hardcode
        LD_PRELOAD=\"/lib/libgcompat.so.0 /usr/lib/libgcompat_shim.so\" \
        nohup /opt/yantrik/bin/yantrik-ui /opt/yantrik/config.yaml \
            >> /opt/yantrik/logs/yantrik-os.log 2>&1 &
    '" || fail "Failed to start yantrik-ui."

# Step 4: Verify
sleep 3
step "Verifying..."
PROCS=$(wsl.exe -d Ubuntu -- bash -lc "ps aux | grep yantrik-ui | grep -v grep | wc -l")
if [ "$PROCS" -eq 1 ]; then
    step "Deploy successful! yantrik-ui running. (PID verified)"
elif [ "$PROCS" -gt 1 ]; then
    warn "Warning: $PROCS instances running. Kill extras manually."
else
    fail "yantrik-ui not running after deploy. Check: wsl.exe -d Ubuntu -- tail -50 /opt/yantrik/logs/yantrik-os.log"
fi

step "Done."
