#!/bin/sh
# Install the Yantrik Live publisher on the live instance, from node2 as root, through the guest
# agent (the instance's segment has no path in from the LAN). The relay's secrets.env comes in on
# stdin and goes straight into the instance; it is never written on node2 or printed:
#   ssh ubuntu@yantrikos.com sudo cat /etc/mediamtx/secrets.env | ssh root@node2 sh setup-publisher.sh
# Run from this directory. VM defaults to 561.
set -eu
cd "$(dirname "$0")"
VM=${VM:-561}

guest() {  # guest SCRIPT [< stdin]: run SCRIPT in the VM as root; fail on a non-zero exit
    out=$(qm guest exec "$VM" --timeout 120 --pass-stdin 1 -- sh -c "$1")
    code=$(printf '%s' "$out" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("exitcode", 1))')
    printf '%s' "$out" | python3 -c 'import json,sys; d=json.load(sys.stdin); sys.stdout.write(d.get("out-data","")); sys.stderr.write(d.get("err-data",""))'
    [ "$code" = 0 ]
}

secrets=$(cat)
printf '%s\n' "$secrets" | grep -qE '^MTX_AUTHINTERNALUSERS_0_PASS=[A-Za-z0-9]+$' \
    && printf '%s\n' "$secrets" | grep -qE '^MTX_PATHS_LIVE_SRTPUBLISHPASSPHRASE=[A-Za-z0-9]{10,79}$' \
    || { echo "stdin is not the relay's secrets.env" >&2; exit 1; }

printf '%s\n' "$secrets" | guest 'set -e; d=/home/yantrik/.config/yantrik-live; runuser -u yantrik -- sh -c "umask 077; mkdir -p $d; cat > $d/publish.env.new; mv $d/publish.env.new $d/publish.env"; stat -c "%U %a %n" $d/publish.env'
guest 'set -e; install -d -m 755 /usr/local/lib/yantrik-live; cat > /usr/local/lib/yantrik-live/publish.new; chmod 755 /usr/local/lib/yantrik-live/publish.new; mv /usr/local/lib/yantrik-live/publish.new /usr/local/lib/yantrik-live/publish' < publish
guest 'set -e; cat > /etc/systemd/system/yantrik-live-publish.service; chmod 644 /etc/systemd/system/yantrik-live-publish.service; systemctl daemon-reload; systemctl enable yantrik-live-publish.service 2>&1; systemctl restart yantrik-live-publish.service; sleep 8; systemctl is-active yantrik-live-publish.service' < yantrik-live-publish.service
echo "publisher running on VM $VM"
