#!/usr/bin/env bash
# ═══════════════════════════════════════════════════════════════════════════════════════
# proxmox-deploy.sh — stand up a Yantrik OS VM on Proxmox, from one definition
# ═══════════════════════════════════════════════════════════════════════════════════════
#
# One cloud-init, one command. The cloud-init that gets used is the one in this repo:
# the script pushes it to the node every run, so the node cannot drift away from git.
# It already had — the copy on node1 was missing qemu-guest-agent, which is precisely
# the package that lets the hypervisor tell you the address of the machine it just made.
#
#   ./proxmox-deploy.sh                        # next free VMID, sensible defaults
#   ./proxmox-deploy.sh --vmid 510 --name yantrik-desk
#   ./proxmox-deploy.sh --vmid 510 --replace   # destroy and rebuild that VMID
#
# The VM is a stock Debian 13 genericcloud image that turns itself into Yantrik OS on
# first boot by fetching the published release. That is a provisioning path, not a
# distribution — build-image.sh exists for the other approach, where the image already
# IS the OS. Both consume the same release tarball and the same cloud-init.
#
# Two settings here are not cosmetic:
#   vga virtio   — a Wayland compositor needs a real GPU device. With Proxmox's default
#                  the machine boots fine, reports a healthy session, and shows a black
#                  screen forever, because there is no /dev/dri for labwc to find.
#   agent 1      — without the guest agent nothing can report the VM's address back, and
#                  an instance you have to go hunting for is not a quick deploy.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CLOUD_INIT="$SCRIPT_DIR/cloud-init/user-data.yaml"

NODE="${YANTRIK_PVE_NODE:-192.168.4.151}"
SSH_KEY="${SSH_KEY:-$HOME/.ssh/id_deploy}"
VMID=""
NAME="yantrik-os"
MEMORY=8192
CORES=4
DISK="24G"
BRIDGE="vmbr0"
STORAGE="local-lvm"
BASE_IMAGE="/var/lib/vz/template/iso/debian-13-genericcloud-amd64.qcow2"
SNIPPET_NAME="yantrik-user-data.yaml"
REPLACE=0

while [ $# -gt 0 ]; do
  case "$1" in
    --vmid)    VMID="$2"; shift 2 ;;
    --name)    NAME="$2"; shift 2 ;;
    --node)    NODE="$2"; shift 2 ;;
    --memory)  MEMORY="$2"; shift 2 ;;
    --cores)   CORES="$2"; shift 2 ;;
    --disk)    DISK="$2"; shift 2 ;;
    --storage) STORAGE="$2"; shift 2 ;;
    --replace) REPLACE=1; shift ;;
    -h|--help) sed -n '2,26p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

SSH_OPTS="-i $SSH_KEY -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o ConnectTimeout=10"
pve() { ssh $SSH_OPTS "root@$NODE" "$@"; }

say()  { printf '\n\033[1m%s\033[0m\n' "$*"; }
ok()   { printf '   \033[32m✓\033[0m %s\n' "$*"; }
fail() { printf '\033[31mFAIL: %s\033[0m\n' "$*" >&2; exit 1; }

[ -f "$CLOUD_INIT" ] || fail "no cloud-init at $CLOUD_INIT"

say "Checking $NODE"
pve true 2>/dev/null || fail "cannot ssh root@$NODE with $SSH_KEY"
ok "$(pve hostname) · $(pve 'pveversion | head -1')"
pve "test -f '$BASE_IMAGE'" || fail "base image missing on the node: $BASE_IMAGE"
ok "base image present"

# ── One definition. Push it every run so the node cannot drift from git. ──
say "Publishing the cloud-init from this repo"
# `local` ships the snippets directory but is not always declared to hold that content
# type, and qm refuses a cicustom pointing at a storage that does not claim it.
pve "mkdir -p /var/lib/vz/snippets"
CONTENT="$(pve "grep -A5 '^dir: local\$' /etc/pve/storage.cfg | awk '/content/ {print \$2}'" || true)"
case "$CONTENT" in
  *snippets*) ok "storage 'local' already allows snippets" ;;
  *) pve "pvesm set local --content '${CONTENT:-iso,vztmpl,backup},snippets'" \
       && ok "storage 'local' now allows snippets" ;;
esac
scp $SSH_OPTS -q "$CLOUD_INIT" "root@$NODE:/var/lib/vz/snippets/$SNIPPET_NAME"
LOCAL_SUM="$(sha256sum "$CLOUD_INIT" | cut -d' ' -f1)"
NODE_SUM="$(pve "sha256sum /var/lib/vz/snippets/$SNIPPET_NAME | cut -d' ' -f1")"
[ "$LOCAL_SUM" = "$NODE_SUM" ] || fail "cloud-init did not land intact on the node"
ok "snippets/$SNIPPET_NAME matches this repo (${LOCAL_SUM:0:12})"

# ── VMID ──
if [ -z "$VMID" ]; then
  VMID="$(pve pvesh get /cluster/nextid)"
  ok "allocated VMID $VMID"
fi

if pve "qm status $VMID" >/dev/null 2>&1; then
  if [ "$REPLACE" = 1 ]; then
    say "Replacing existing VM $VMID"
    pve "qm stop $VMID --skiplock 1" >/dev/null 2>&1 || true
    pve "qm destroy $VMID --purge 1 --destroy-unreferenced-disks 1" >/dev/null \
      || fail "could not destroy $VMID"
    ok "old $VMID removed"
  else
    fail "VM $VMID already exists — pass --replace to rebuild it"
  fi
fi

# ── Create ──
say "Creating $NAME ($VMID) on $NODE"
pve "qm create $VMID \
      --name '$NAME' \
      --memory $MEMORY --cores $CORES --cpu host \
      --machine q35 --ostype l26 \
      --scsihw virtio-scsi-single \
      --scsi0 $STORAGE:0,import-from=$BASE_IMAGE \
      --net0 virtio,bridge=$BRIDGE \
      --vga virtio \
      --agent enabled=1 \
      --serial0 socket \
      --boot order=scsi0" \
  || fail "qm create failed"
ok "created from the Debian 13 genericcloud image"

pve "qm disk resize $VMID scsi0 $DISK" >/dev/null || fail "could not resize the disk to $DISK"
ok "disk $DISK"

pve "qm set $VMID \
      --ide2 $STORAGE:cloudinit \
      --ipconfig0 ip=dhcp \
      --cicustom 'user=local:snippets/$SNIPPET_NAME'" >/dev/null \
  || fail "could not attach cloud-init"
ok "cloud-init attached from local:snippets/$SNIPPET_NAME"

say "Starting"
pve "qm start $VMID" >/dev/null || fail "could not start $VMID"
ok "running"

# ── Wait for the machine to say where it is ──
say "Waiting for the guest agent"
IP=""
for _ in $(seq 1 60); do
  sleep 10
  IP="$(pve "qm guest cmd $VMID network-get-interfaces 2>/dev/null" \
        | grep -oE '\"ip-address\" ?: ?\"192\.168\.[0-9]+\.[0-9]+\"' \
        | grep -oE '192\.168\.[0-9]+\.[0-9]+' | head -1 || true)"
  [ -n "$IP" ] && break
done

if [ -n "$IP" ]; then
  ok "address $IP"
else
  printf '   \033[33m!\033[0m no address yet — cloud-init purges the cloud kernel and reboots once,\n'
  printf '     so the first boot is two boots. Check: qm guest cmd %s network-get-interfaces\n' "$VMID"
fi

say "Deployed"
cat <<EOF
   VM        $VMID ($NAME) on $NODE
   Console   https://$NODE:8006  →  $VMID  →  Console
   Address   ${IP:-pending}
   Progress  ssh root@$NODE "qm terminal $VMID"   (serial0)
             tail -f /var/log/yantrik-firstboot.log on the guest

   First boot installs the published release and reboots once to drop Debian's
   cloud kernel, which carries no virtio-gpu. Give it a few minutes.
EOF
