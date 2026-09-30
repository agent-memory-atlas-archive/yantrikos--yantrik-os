# Shared by every harness installer here. Sourced, not run.
#
# The Install button on Settings > Harnesses runs one of these as the person, with no terminal
# and no root, and streams what it prints into the row. So every line printed is written for
# that row, and everything lands in the person's home:
#
#   ~/.local/bin    the one place a harness's command is put. The desktop looks here as well as
#                   on PATH (crates/yantrik-ui/src/harness_catalogue.rs, `user_bin_dir`), and
#                   each harness's unit puts it on PATH, so nothing depends on a login shell
#                   having read ~/.profile first.
#   ~/.local/node   Node, when the machine has none new enough. The image ships none.

USER_BIN="$HOME/.local/bin"
mkdir -p "$USER_BIN"
case ":$PATH:" in *":$USER_BIN:"*) ;; *) PATH="$USER_BIN:$PATH" ;; esac
export PATH

say() { printf '%s\n' "$*"; }
fail() { printf 'install failed: %s\n' "$*" >&2; exit 1; }

# Node, pinned. Updated by hand with the checksums nodejs.org publishes in SHASUMS256.txt for
# this release: a download that does not match is refused, not unpacked. The .tar.gz, not the
# smaller .tar.xz, because the image has no xz.
NODE_VERSION=24.21.0
NODE_SHA256_X64=6e1db87ef58b8819e5d5402eff1536491b18edd8eb7bee5ef7897876e88dc5ff
NODE_SHA256_ARM64=724282c3b43aec998aa9527380465b45d229e021b58035f5f4f63095eabfe5d5

# node_at_least MAJOR.MINOR — the node on PATH is at least that version.
node_at_least() {
    command -v node >/dev/null 2>&1 || return 1
    node -e 'const h = process.versions.node.split(".").map(Number);
             const w = process.argv[1].split(".").map(Number);
             process.exit(h[0] > w[0] || (h[0] === w[0] && h[1] >= (w[1] || 0)) ? 0 : 1)' "$1" 2>/dev/null
}

# ensure_node MAJOR.MINOR — a node at least that new on PATH, installing the pinned one if not.
ensure_node() {
    want=$1
    if node_at_least "$want"; then
        say "Node $(node --version) is here"
        return 0
    fi
    case "$(uname -m)" in
        x86_64|amd64) arch=x64; sum=$NODE_SHA256_X64 ;;
        aarch64|arm64) arch=arm64; sum=$NODE_SHA256_ARM64 ;;
        *) fail "no Node build for $(uname -m)" ;;
    esac
    name="node-v$NODE_VERSION-linux-$arch"
    dest="$HOME/.local/$name"
    if [ ! -x "$dest/bin/node" ]; then
        say "fetching Node $NODE_VERSION"
        tmp=$(mktemp -d) || fail "no temporary directory"
        curl -fsSL --retry 3 -o "$tmp/node.tar.gz" "https://nodejs.org/dist/v$NODE_VERSION/$name.tar.gz" \
            || { rm -rf "$tmp"; fail "could not download Node $NODE_VERSION"; }
        got=$(sha256sum "$tmp/node.tar.gz" | cut -d' ' -f1)
        [ "$got" = "$sum" ] || { rm -rf "$tmp"; fail "the Node download did not match its checksum"; }
        tar -xzf "$tmp/node.tar.gz" -C "$HOME/.local" || { rm -rf "$tmp"; fail "could not unpack Node"; }
        rm -rf "$tmp"
    fi
    ln -sfn "$dest" "$HOME/.local/node"
    for tool in node npm npx; do
        ln -sf "$HOME/.local/node/bin/$tool" "$USER_BIN/$tool"
    done
    node_at_least "$want" || fail "Node $NODE_VERSION is unpacked but does not run, or is older than $want"
    say "Node $(node --version) installed in ~/.local/node"
}
