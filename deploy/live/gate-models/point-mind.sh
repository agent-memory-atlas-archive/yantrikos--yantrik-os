#!/bin/sh
# Point the live instance's Mind (VM 561) at the gate's cloud routes (setup-models.sh). Run on
# node2 as root, from this directory. VM defaults to 561. Restarts the Mind service, not the
# desktop. Undo: the Mind's settings are kept beside themselves as yantrik-mind.env.before-cloud.
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
# NVIDIA NIM is given to the Mind too (mind-env.sh) but leads nothing until YM_PRIMARY_BRAIN says
# nim:nvidia/nemotron-3-super-120b-a12b.
#
# Each provider's key, in the Mind's settings, is the instance key the AIG lane already carries:
# the gate checks it and puts the real key on.
set -eu
cd "$(dirname "$0")"
VM=${VM:-561}
. ../guest.sh

guest 'runuser -u yantrik-mind -- sh -s' < mind-env.sh
# The gate is a private address, and the OS's egress proxy refuses those to a mind unless a lan
# rule allows it. Root names the gate in /etc/yantrik/mind-person.env, a file the updater seeds
# from, and reconciles so the rule is there now. The address comes from here, never from the
# Mind's settings, which are the mind's to write: a mind must not choose its own lan rule.
# Without it every model call failed with "CONNECT proxy failed: 403" once 561 took the egress
# proxy (15 Oct 2026). The URL only; the instance key stays in the Mind's settings.
GATE_URL=${GATE_URL:-http://10.99.0.1:8443}
# It goes into a root shell in the VM, so it is held to an address and a port, nothing else.
case "$GATE_URL" in *[!0-9a-z.:/]*) echo "GATE_URL must be http://<ip>:<port>" >&2; exit 1 ;; esac
case "$GATE_URL" in http://[0-9]*.[0-9]*.[0-9]*.[0-9]*:[0-9]*) ;; *) echo "GATE_URL must be http://<ip>:<port>" >&2; exit 1 ;; esac
guest "set -e
install -d -m 755 /etc/yantrik
umask 022
printf '# The live gate, where the Mind of this instance reaches its models (deploy/live/gate-models).\nYM_LOCAL_OLLAMA_URL=%s\n' '$GATE_URL' > /etc/yantrik/mind-person.env.new
mv /etc/yantrik/mind-person.env.new /etc/yantrik/mind-person.env
/opt/yantrik/bin/yantrik-update reconcile 2>&1 | grep -E 'seeded|not seeded' || true" < /dev/null
guest 'set -e
systemctl restart yantrik-mind
sleep 8
systemctl is-active yantrik-mind
journalctl -u yantrik-mind --since "-20s" --no-pager -o cat | grep -iE "brain|chain" | head -8' < /dev/null
