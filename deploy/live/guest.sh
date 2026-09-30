# Shared by the live setup scripts, which run on node2 as root and reach the VMs only through the
# Proxmox guest agent (the live segment has no path in from the LAN). Source it, set VM, then:
#   guest 'SCRIPT' [< stdin]
# runs SCRIPT with sh in the VM as root, passes stdin through (the agent takes at most 1 MiB),
# prints what it said, and fails when it exits non-zero.
guest() {
    _out=$(qm guest exec "$VM" --timeout 120 --pass-stdin 1 -- sh -c "$1")
    printf '%s' "$_out" | python3 -c 'import json,sys; d=json.load(sys.stdin); sys.stdout.write(d.get("out-data","")); sys.stderr.write(d.get("err-data",""))'
    [ "$(printf '%s' "$_out" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("exitcode", 1))')" = 0 ]
}
