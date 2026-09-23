#!/bin/bash
# Does every control surface on this machine keep the protocol? A thin wrapper over `yos check`.
#
#   scripts/app-control-probe.sh                 every surface that answers
#   scripts/app-control-probe.sh notes calendar  just these
#   scripts/app-control-probe.sh --json          for a machine to read
#
# This script used to start eight apps from /home/yantrik paths, glob /tmp/yantrik-*, and check
# the contract every surface owes with inline Python of its own — a fourth copy of the socket
# chain and a second, partial reading of the refusal rules. The contract is written down now
# (docs/surface-protocol.md) and `yos check` is the one program that reads it: the describe against
# the schema, the grades, the parameter types, no secret among the arguments, the revision, and
# every refusal the dispatch owes, word for word — without ever running an action.
#
# What the old script also did — drive each app through a real behaviour and look at the result
# (a download landing with its checksum, a terminal `cd` persisting) — is per-app behaviour, and
# that lives in tests/conformance, one probe per app, run on a live machine.
set -u

YOS=${YOS:-}
if [ -z "$YOS" ]; then
  here=$(cd "$(dirname "$0")" && pwd)
  if [ -x "$here/../deploy/yantrik-os/yos" ]; then
    YOS="$here/../deploy/yantrik-os/yos"
  else
    YOS=/opt/yantrik/bin/yos
  fi
fi

json=()
names=()
for arg in "$@"; do
  case "$arg" in
    --json) json=(--json) ;;
    *) names+=("$arg") ;;
  esac
done

if [ ${#names[@]} -eq 0 ]; then
  exec python3 "$YOS" check --all "${json[@]}"
fi
exec python3 "$YOS" check "${names[@]}" "${json[@]}"
