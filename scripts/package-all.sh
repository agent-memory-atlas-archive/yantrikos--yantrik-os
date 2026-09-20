#!/usr/bin/env bash
# Package all built binaries as component tarballs
# Run inside WSL: bash scripts/package-all.sh
set -eu

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# Where cargo actually writes, asked of cargo. This used to be /home/yantrik/target-yantrik —
# one developer's home directory — so on any other machine it packaged nothing and said "SKIP"
# twenty-five times in a row, which reads like a result.
TARGET_DIR="${TARGET_DIR:-$( \
  cd "$PROJECT_ROOT" && cargo metadata --format-version 1 --no-deps --offline 2>/dev/null \
    | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')/release}"
[ -d "$TARGET_DIR" ] || { echo "no release directory at $TARGET_DIR" >&2; exit 1; }

STAGING=/tmp/ypub-all
OUTPUT_DIR=/tmp/yantrik-components
rm -rf "$STAGING" "$OUTPUT_DIR"
mkdir -p "$STAGING" "$OUTPUT_DIR"

# What the OS is made of is discovered, never listed here — the rule build-release.sh already
# follows. The list that used to live on this line named twenty-five binaries: it had gone
# stale in both directions at once, missing a11y-service and perception-service (so no machine
# fed from these components had them) while still naming the two shelved apps (so every machine
# fed from them got Music and ySheets back, which the shipped launcher refuses to open).
SHELVED_BINS="$("$PROJECT_ROOT/deploy/yantrik-os/shelved-bins.sh" | paste -sd' ' -)"
BINARIES="$(
  find "$TARGET_DIR" -maxdepth 1 -type f -executable \
    ! -name ".*" ! -name "*.so" ! -name "*.d" ! -name "*.rlib" ! -name "build-script*" \
    ! -name "test-*" ! -name "*-test" ! -name "bench-*" \
    -printf '%f\n' | sort | grep -vxF "$(printf '%s\n' $SHELVED_BINS)" | paste -sd' ' -
)"
[ -n "$BINARIES" ] || { echo "no binaries found in $TARGET_DIR" >&2; exit 1; }
for b in $SHELVED_BINS; do
    [ -f "$TARGET_DIR/$b" ] && echo "SHELVED (not packaged): $b"
done

VERSION="$(git -C "$PROJECT_ROOT" describe --tags --always --dirty 2>/dev/null || echo 0.0.0-unknown)"
TARGET="x86_64-unknown-linux-gnu"
GIT_SHA=$(git -C "$PROJECT_ROOT" rev-parse --short HEAD 2>/dev/null || echo "unknown")

COMPONENTS_JSON="{"
FIRST=true

for name in $BINARIES; do
    BIN="$TARGET_DIR/$name"
    if [ ! -f "$BIN" ]; then
        echo "SKIP: $name"
        continue
    fi

    # Stage the binary
    mkdir -p "$STAGING/$name"
    cp "$BIN" "$STAGING/$name/"

    # Compute checksums
    SHA=$(sha256sum "$BIN" | cut -d' ' -f1)
    SIZE=$(stat -c%s "$BIN")

    # Write metadata
    cat > "$STAGING/$name/metadata.json" << EOF
{"name":"$name","version":"$VERSION","target":"$TARGET","git_sha":"$GIT_SHA","sha256":"$SHA","size":$SIZE,"built":"$(date -u +%Y-%m-%dT%H:%M:%SZ)"}
EOF

    # Create tarball
    cd "$STAGING"
    tar cf - "$name/" | zstd -3 -o "$OUTPUT_DIR/$name.tar.zst" 2>/dev/null
    cd /

    SIZE_MB=$(echo "scale=1; $SIZE/1048576" | bc 2>/dev/null || echo "?")
    echo "OK: $name (${SIZE_MB}MB)"

    # Build manifest entry
    URL="http://bin.yantrikos.com/components/$name/$VERSION/$TARGET.tar.zst"
    if [ "$FIRST" = true ]; then
        FIRST=false
    else
        COMPONENTS_JSON="$COMPONENTS_JSON,"
    fi
    COMPONENTS_JSON="$COMPONENTS_JSON\"$name\":{\"version\":\"$VERSION\",\"target\":\"$TARGET\",\"url\":\"$URL\",\"sha256\":\"$SHA\",\"size\":$SIZE}"
done

COMPONENTS_JSON="$COMPONENTS_JSON}"

# Write channel manifest
cat > "$OUTPUT_DIR/nightly.json" << EOF
{"channel":"nightly","date":"$(date -u +%Y-%m-%d)","git_sha":"$GIT_SHA","target":"$TARGET","components":$COMPONENTS_JSON}
EOF

echo "---"
echo "Packaged $(ls "$OUTPUT_DIR"/*.tar.zst | wc -l) components"
du -sh "$OUTPUT_DIR"
echo "Manifest: $OUTPUT_DIR/nightly.json"

# Cleanup staging
rm -rf "$STAGING"
