#!/bin/sh
# Bring the harness plugins an install copied into this person's home up to this build's: refresh-plugins.sh
#
# Run as the person, never as root: by `yantrik-update` after an update is applied and on every
# `yantrik-update reconcile` (reconcile_harness_plugins), and by hand from a terminal.
#
# Hermes is the only harness that loads a copy: its gateway reads ~/.hermes/plugins/, which
# hermes.sh writes once, at install, so without this every Hermes fix shipped later reached fresh
# installs only (VM 541, 14 Oct). Pi is handed its extension in /opt/yantrik/share by path
# (yantrik_pi.py, `_default_extension`); DeepSeek and OpenClaw run their scripts there; OpenClaw is
# given the image's own yantrik-os and yantrikdb-mcp by path; the Mind installs nothing in a home.
# So there is nothing of theirs to refresh, and a harness that starts copying one belongs here.
#
# Only a plugin the install wrote and that is still there is refreshed, only when this build's
# differs by content (hermes-plugins.sh, hermes_tree_hash), and by the same functions hermes.sh
# writes it with. Hermes's gateway is restarted once, and only when something changed and it is
# running. Each change is printed and kept in ~/.local/state/yantrik/harnesses/hermes/refreshed.
# Nothing else under ~/.hermes is touched: config.yaml, .env, sessions, memories, other plugins.
set -eu

# A machine with no Hermes plugin of ours: nothing to do, and nothing written, not even the
# ~/.local/bin common.sh makes.
[ -f "$HOME/.hermes/plugins/yantrik/plugin.yaml" ] || [ -d "$HOME/.hermes/plugins/yantrikdb" ] || exit 0

. "$(dirname "$0")/common.sh"
. "$(dirname "$0")/hermes-plugins.sh"

plugins="$HOME/.hermes/plugins"
shipped="$(dirname "$HARNESS_LIB")/hermes"
changed=""

# ── The desktop plugin ──
dest="$plugins/yantrik"
if [ -f "$dest/plugin.yaml" ] && [ -f "$shipped/plugin.yaml" ]; then
    was=$(hermes_tree_hash "$dest")
    now=$(hermes_tree_hash "$shipped")
    if [ "$was" != "$now" ]; then
        hermes_desktop_plugin_build "$shipped" "$plugins/.yantrik-new" \
            && hermes_swap_dir "$plugins/.yantrik-new" "$dest" \
            || { rm -rf "$plugins/.yantrik-new"; fail "could not refresh Hermes's desktop plugin; the earlier one is in place"; }
        changed="$changed yantrik"
        say "refreshed Hermes's desktop plugin in ~/.hermes/plugins/yantrik (was $(echo "$was" | cut -c1-12), now $(echo "$now" | cut -c1-12))"
    fi
fi

# ── The YantrikDB memory plugin ──
# Built from the image's wheels beside the installed one and compared there, since what an install
# writes is what pip makes of the wheels, not a file that ships; kept only when it differs. A build
# that fails leaves the earlier plugin as it was, and the desktop plugin above still counts.
dest="$plugins/yantrikdb"
if [ -d "$dest" ]; then
    if [ ! -d "$MEMORY_WHEELS" ]; then
        say "left Hermes's YantrikDB plugin as it was: this build has no wheels at $MEMORY_WHEELS"
    elif ! hermes_python=$(python3 "$HARNESS_LIB/hermes_runtime.py" </dev/null); then
        say "left Hermes's YantrikDB plugin as it was: cannot find the Python Hermes runs on"
    elif ! ( hermes_yantrikdb_plugin_build "$plugins/.yantrikdb-new" "$hermes_python" ); then
        rm -rf "$plugins/.yantrikdb-new"
        say "left Hermes's YantrikDB plugin as it was: this build's would not build (the line above says why)"
    else
        was=$(hermes_tree_hash "$dest")
        now=$(hermes_tree_hash "$plugins/.yantrikdb-new")
        if [ "$was" = "$now" ]; then
            rm -rf "$plugins/.yantrikdb-new"
        else
            hermes_swap_dir "$plugins/.yantrikdb-new" "$dest" \
                || { rm -rf "$plugins/.yantrikdb-new"; fail "could not refresh Hermes's YantrikDB plugin; the earlier one is in place"; }
            changed="$changed yantrikdb"
            say "refreshed Hermes's YantrikDB plugin in ~/.hermes/plugins/yantrikdb (was $(echo "$was" | cut -c1-12), now $(echo "$now" | cut -c1-12))"
        fi
    fi
fi

[ -n "$changed" ] || exit 0

state=$(harness_state hermes)
mkdir -p "$state" && printf '%s%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$changed" >> "$state/refreshed" \
    || say "could not note the refresh in $state/refreshed"

# The gateway is what loads the plugins, so a change reaches Hermes only once it restarts. One the
# person stopped is left stopped: it reads the new plugins when it next starts.
if systemctl --user is-active --quiet hermes-gateway 2>/dev/null; then
    systemctl --user restart hermes-gateway || fail "refreshed Hermes's plugins, but its gateway would not restart; run: systemctl --user restart hermes-gateway"
    say "restarted Hermes's gateway so it loads them"
else
    say "Hermes's gateway is not running; it loads them when it next starts"
fi
