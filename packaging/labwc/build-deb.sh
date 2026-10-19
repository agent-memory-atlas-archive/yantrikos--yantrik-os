#!/bin/bash
# Build Yantrik's labwc: Debian's labwc 0.8.3-1 plus debian-patches/, as 0.8.3-1yantrik1.
# Run as root in a Debian 13 (trixie) system with deb-src enabled (the labwc-build container on
# node2, or a debian:trixie CI job). Prints the path of the .deb it built.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
UPSTREAM=0.8.3-1
OURS=0.8.3-1yantrik1
WORK="${1:-/tmp/labwc-yantrik}"
export DEBIAN_FRONTEND=noninteractive LC_ALL=C.UTF-8
apt-get build-dep -y -qq labwc >/dev/null
rm -rf "$WORK" && mkdir -p "$WORK" && cd "$WORK"
apt-get source -qq "labwc=$UPSTREAM" >/dev/null 2>&1
cd labwc-0.8.3
# Debian ships labwc with no patches of its own, so the directory may not exist yet.
mkdir -p debian/patches
for p in "$HERE"/debian-patches/*.patch; do
  cp "$p" debian/patches/
  basename "$p" >> debian/patches/series
done
# Apply our patches in the unpacked tree (dpkg-source applied Debian's own on unpack).
while read -r p; do
  case "$p" in yantrik-*) patch -p1 --forward --quiet < "debian/patches/$p" ;; esac
done < debian/patches/series
# The entry is written with a fixed date, not dch's "now": dpkg takes SOURCE_DATE_EPOCH from the
# changelog, so two builds of the same patch are the same bytes and the .deb can be pinned by hash
# (labwc.pin), which is how a build checks it shipped the package that was reviewed.
STAMP="Mon, 19 Oct 2026 12:00:00 +0000"
{
  printf 'labwc (%s) trixie; urgency=medium\n\n' "$OURS"
  printf '  * Add an external window switcher renderer (zlabwc_window_switcher_renderer_v1)\n'
  printf '    for Yantrik OS Alt+Tab (yantrikos/yantrik-os#872).\n\n'
  printf ' -- Yantrik OS <dev@yantrikos.com>  %s\n\n' "$STAMP"
  cat debian/changelog
} > debian/changelog.new
mv debian/changelog.new debian/changelog
export SOURCE_DATE_EPOCH="$(date -d "$STAMP" +%s)"
DEB_BUILD_OPTIONS="nocheck parallel=$(nproc)" dpkg-buildpackage -us -uc -b >"$WORK/build.log" 2>&1 \
  || { tail -30 "$WORK/build.log"; exit 1; }
ls "$WORK/labwc_${OURS}_amd64.deb"
