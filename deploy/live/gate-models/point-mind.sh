#!/bin/sh
# Point the live instance's Mind (VM 561) at the gate's cloud routes (setup-models.sh). Run on
# node2 as root, from this directory. VM defaults to 561. Restarts the Mind service, not the
# desktop.
#
# The brain it ends up with, in order:
#   Ollama Cloud, deepseek-v4.1-flash  first: 8 of 9 on the desktop task battery (2026-09-21),
#                                      at about 3 s a call;
#   NanoGPT, its default model         when Ollama Cloud refuses or fails;
#   AIG through the gate               the survival fallback, and still the private lane's only
#                                      model, because a private turn never goes to a cloud.
# AIG's own model (bonsai2-27b) leads no longer: on this machine it could not fill a tool's named
# parameters, and repeated the same bare string to the desktop seven times over two runs.
#
# Each provider's key, in the Mind's settings, is the instance key the AIG lane already carries:
# the gate checks it and puts the real key on. The settings file stays the Mind account's, 600.
set -eu
VM=${VM:-561}
. ../guest.sh

guest 'set -e
f=/var/lib/yantrik-mind/.config/yantrik-mind.env
key=$(sed -n "s/^YM_LOCAL_OLLAMA_KEY=\([0-9a-f]*\)\$/\1/p" "$f" | head -n 1)
url=$(sed -n "s/^YM_LOCAL_OLLAMA_URL=\(.*\)\$/\1/p" "$f" | head -n 1)
[ -n "$key" ] && [ -n "$url" ] || { echo "the Mind has no gate lane to build on" >&2; exit 1; }
gate=${url%/}
cp -p "$f" "$f.before-cloud"
# Drop any earlier copy of these lines, then write them once.
sed -i "/^# The cloud providers, through the gate/d; /^YM_LOCAL_ROLE=/d; /^YM_PRIMARY_BRAIN=/d; /^YM_PROVIDER_BASE_URL_OLLAMA_CLOUD=/d; /^YM_PROVIDER_BASE_URL_NANOGPT=/d; /^OLLAMA_CLOUD_KEY=/d; /^NANOGPT_KEY=/d" "$f"
cat >> "$f" <<EOF
# The cloud providers, through the gate (deploy/live/gate-models/point-mind.sh).
YM_LOCAL_ROLE=fallback
YM_PRIMARY_BRAIN=ollama-cloud:deepseek-v4.1-flash
YM_PROVIDER_BASE_URL_OLLAMA_CLOUD=$gate/ollama-cloud/v1
YM_PROVIDER_BASE_URL_NANOGPT=$gate/nanogpt/api/v1
OLLAMA_CLOUD_KEY=$key
NANOGPT_KEY=$key
EOF
chown yantrik-mind: "$f"; chmod 600 "$f"
systemctl restart yantrik-mind
sleep 8
systemctl is-active yantrik-mind
journalctl -u yantrik-mind --since "-20s" --no-pager -o cat | grep -iE "brain|chain" | head -8'
