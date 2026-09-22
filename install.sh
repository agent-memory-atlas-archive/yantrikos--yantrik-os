#!/bin/sh
# ═══════════════════════════════════════════════════════════════════════════════════════
# Yantrik OS is an ISO. This script does not install it — it tells you where the image is.
# ═══════════════════════════════════════════════════════════════════════════════════════
#
# What used to be here installed a set of binaries onto an already-running Alpine Linux
# system, from a domain that no longer resolves. Yantrik OS has been a Debian 13 live image
# built by CI since well before that script was last touched, so the documented way to
# install this operating system installed a different one. That script is kept, unused, at
# deploy/yantrik-os/legacy/install-alpine-2026-03.sh.
#
# This replaces it with the smallest honest thing:
#
#   curl -fsSL https://get.yantrikos.com/install.sh | sh
#
# prints where the current image is, how big it is and what its sha256 is, and stops. It
# writes nothing, installs nothing and asks for no privilege. An installer that runs the
# moment it is piped into a shell is asking a person to trust bytes they have not read; the
# download is behind --download, and even then the only thing it touches is one file in the
# directory you ran it from.
#
# Usage:
#   sh install.sh                 what the current image is, and what to do with it
#   sh install.sh --download      also fetch it here and check its sha256
#   sh install.sh --help
#
# Only the nightly channel has ever had a build published to it. There is no stable channel
# to point this at — https://iso.yantrikos.com/stable/latest.json is a 404 — so the channel
# is not a flag. When stable starts receiving images, this becomes a choice; until then
# offering one would only produce a confident error message.

set -eu

CHANNEL="nightly"
BASE="https://iso.yantrikos.com/$CHANNEL"

# Colour only when stdout is a terminal. Piped into a file or a log, escape codes are noise.
if [ -t 1 ]; then
    B=$(printf '\033[1m'); DIM=$(printf '\033[2m'); N=$(printf '\033[0m')
else
    B=''; DIM=''; N=''
fi

say()  { printf '%s\n' "$*"; }
fail() { printf '%s\n' "$*" >&2; exit 1; }

# ── Fetching, with whichever of the two is present ──────────────────────────────────────
#
# curl on most machines, wget on a Debian netinst that has not had curl added yet. Neither
# is a dependency worth installing on somebody's behalf, so this says which one to install
# rather than reaching for a package manager.
if command -v curl >/dev/null 2>&1; then
    fetch_stdout() { curl -fsSL --connect-timeout 15 --retry 2 -- "$1"; }
    fetch_file()   { curl -fL --connect-timeout 15 --retry 2 --progress-bar -o "$2" -- "$1"; }
elif command -v wget >/dev/null 2>&1; then
    fetch_stdout() { wget -qO- --timeout=15 --tries=3 -- "$1"; }
    fetch_file()   { wget --timeout=15 --tries=3 -O "$2" -- "$1"; }
else
    fail "This needs curl or wget, and neither is installed."
fi

# sha256sum on Linux, shasum -a 256 on macOS. Checked before anything is downloaded: finding
# out that 1.3 GB cannot be verified after it has arrived is the wrong order.
if command -v sha256sum >/dev/null 2>&1; then
    sha256_of() { sha256sum -- "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null 2>&1; then
    sha256_of() { shasum -a 256 -- "$1" | cut -d' ' -f1; }
else
    sha256_of() { return 1; }
fi

# One field out of latest.json.
#
# Parsed with sed rather than a JSON library because this has to run on a machine with
# nothing installed, and the file is written by deploy/yantrik-os/server/yantrik-publish —
# it is flat, it has no nesting and no string in it contains a quote. A field that cannot be
# read comes back empty and the caller says so, rather than this inventing a value.
json_field() { # <json> <key>
    printf '%s' "$1" | tr ',' '\n' | sed -n "s/.*\"$2\"[[:space:]]*:[[:space:]]*\"\{0,1\}\([^\",}]*\)\"\{0,1\}.*/\1/p" | head -1
}

human_bytes() { # <bytes> -> "1.31 GiB"
    awk -v b="$1" 'BEGIN {
        if (b+0 <= 0) { print "unknown"; exit }
        split("B KiB MiB GiB TiB", u, " "); i = 1
        while (b >= 1024 && i < 5) { b /= 1024; i++ }
        printf (i == 1 ? "%d %s\n" : "%.2f %s\n"), b, u[i]
    }'
}

DOWNLOAD=0
for a in "$@"; do
    case "$a" in
        --download) DOWNLOAD=1 ;;
        -h|--help)
            # Written out rather than sed'd out of "$0": piped into a shell there is no "$0"
            # to read, and a --help that prints nothing in the one situation this script is
            # most often run in is not a --help.
            cat <<'USAGE'

Yantrik OS ships as a live ISO. This prints where the current image is; it installs nothing.

  sh install.sh                 what the current image is, and what to do with it
  sh install.sh --download      also fetch it into this directory and check its sha256
  sh install.sh --help

Images:  https://iso.yantrikos.com/nightly/     (nightly is the only published channel)
Source:  https://github.com/yantrikos/yantrik-os/blob/main/install.sh
Guide:   https://github.com/yantrikos/yantrik-os/blob/main/docs/getting-started.md

USAGE
            exit 0 ;;
        *) fail "Unknown option '$a'. This takes --download or --help, and nothing else." ;;
    esac
done

say ""
say "${B}Yantrik OS${N} ships as a live ISO — a Debian 13 image you boot and try before you"
say "install anything. There is no package to add to a system you already have."
say ""

say "${DIM}Asking $BASE/latest.json …${N}"
LATEST=$(fetch_stdout "$BASE/latest.json") \
    || fail "Could not read $BASE/latest.json. Check the network, then https://iso.yantrikos.com/$CHANNEL/ in a browser."

FILE=$(json_field "$LATEST" file)
SHA=$(json_field "$LATEST" sha256)
BYTES=$(json_field "$LATEST" bytes)
VERSION=$(json_field "$LATEST" version)
DATE=$(json_field "$LATEST" date)

[ -n "$FILE" ] || fail "latest.json named no file. Nothing here is going to guess a URL; look at $BASE/ instead."
[ -n "$SHA" ]  || fail "latest.json carries no sha256 for $FILE. Refusing to point anyone at an image that cannot be verified."

URL="$BASE/$FILE"

say ""
say "  ${B}version${N}   ${VERSION:-unknown}${DATE:+  (built $DATE)}"
say "  ${B}file${N}      $FILE"
say "  ${B}size${N}      $(human_bytes "${BYTES:-0}")${BYTES:+  ($BYTES bytes)}"
say "  ${B}sha256${N}    $SHA"
say "  ${B}url${N}       $URL"
say ""
say "This is the ${B}nightly${N} channel, and it is the only one that has ever had a build"
say "published to it. It is early software: things break, and the audits of what does not"
say "work are public in the repository under design/."
say ""

if [ "$DOWNLOAD" = 0 ]; then
    say "To fetch it here and check it:"
    say ""
    say "  ${B}sh install.sh --download${N}"
    say ""
    say "or download it yourself and verify before you boot it:"
    say ""
    say "  curl -fL -O $URL"
    say "  curl -fL -O $URL.sha256"
    say "  sha256sum -c $FILE.sha256"
    say ""
    exit 0
fi

# ── --download ──────────────────────────────────────────────────────────────────────────
#
# Into the current directory, under the image's own name. Not /tmp, which some distributions
# clear and some mount small; not a path this script invents somewhere in $HOME. The person
# ran it where they want the file.
if ! sha256_of /dev/null >/dev/null 2>&1; then
    fail "Neither sha256sum nor shasum is installed, so the download could not be checked. Not downloading 1.3 GB that nothing here can verify."
fi

if [ -e "$FILE" ]; then
    say "$FILE is already here. Checking it rather than downloading it again."
else
    say "Downloading $FILE ($(human_bytes "${BYTES:-0}")) into $(pwd)"
    # To a partial name, renamed only once the bytes are all here. An interrupted download
    # left under the final name is the thing that later gets written to a USB stick.
    fetch_file "$URL" "$FILE.part" || { rm -f "$FILE.part"; fail "Download failed. Nothing was kept."; }
    mv -f "$FILE.part" "$FILE"
fi

say "Verifying sha256 …"
GOT=$(sha256_of "$FILE") || fail "Could not compute a sha256 of $FILE."
if [ "$GOT" != "$SHA" ]; then
    say ""
    say "  expected  $SHA"
    say "  got       $GOT"
    say ""
    fail "The file does not match what latest.json says it should be. Do not boot it. Delete $FILE and try again; if it fails twice, say so on https://github.com/yantrikos/yantrik-os/issues."
fi
say "  sha256 ok  $GOT"
say ""

say "${B}Writing it to a USB stick${N}"
say ""
say "  Linux, macOS   ${B}dd${N}, as root, with the device — not a partition — as the target:"
say "                   sudo dd if=$FILE of=/dev/sdX bs=4M status=progress conv=fsync"
say "                 Get /dev/sdX wrong and you overwrite the wrong disk. On Linux"
say "                 'lsblk' names them; on macOS 'diskutil list', where it is /dev/rdiskN"
say "                 and the stick must be unmounted first ('diskutil unmountDisk')."
say ""
say "  Windows        ${B}Rufus${N} (rufus.ie) or ${B}balenaEtcher${N} (etcher.balena.io). Both take"
say "                 the .iso as it is; do not unpack it."
say ""
say "  macOS, no dd   ${B}balenaEtcher${N} does the same job with the device picker."
say ""
say "  Or skip the stick entirely and attach the .iso to a VM — 4 CPUs, 8 GB of RAM and no"
say "  GPU is what this is developed on; the automated boot test runs it under QEMU with"
say "  4 GB. See docs/hardware-requirements.md."
say ""
say "${B}Booting it${N}"
say ""
say "  The image boots to a live desktop without touching any disk. When you want it on one,"
say "  the installer is ${B}inside${N} the live session: pick \"Install Yantrik OS\" in the boot"
say "  menu, or from a terminal in the running desktop run"
say ""
say "                   ${B}sudo /opt/yantrik/bin/yantrik-install${N}"
say ""
say "  which asks for a disk and erases it. (The full path because sudo's secure_path does"
say "  not carry /opt/yantrik/bin, though your own PATH does.)"
say ""
say "  docs/getting-started.md walks through the whole of it, including how to point the"
say "  desktop at a model."
say ""
