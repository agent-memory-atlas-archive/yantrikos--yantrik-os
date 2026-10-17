#!/bin/sh
# Install the Director on the live instance (VM 561), from node2 as root, from this directory.
# VM defaults to 561. Re-run to update the script or the missions; finished missions are kept
# in the account's ~/director/done.json and are not started again.
set -eu
cd "$(dirname "$0")"
VM=${VM:-561}
# ENABLE=0 installs without starting the timer, e.g. while the Mind is still in Ask mode, where
# its first command would only raise a card that nobody is there to answer.
ENABLE=${ENABLE:-1}
. ../guest.sh

# The showcase's frame page is the Director's to rewrite, as the desktop's account.
guest 'set -e; install -d -m 755 /opt/yantrik-live/director; install -d -o yantrik -g yantrik -m 700 /home/yantrik/director; install -d -o yantrik -g yantrik -m 755 /opt/yantrik-live/showcase' < /dev/null
for f in director.py missions.json check-page showcase-browser; do
  guest "set -e; cat > /opt/yantrik-live/director/$f.new; chmod 644 /opt/yantrik-live/director/$f.new; case $f in check-page|showcase-browser) chmod 755 /opt/yantrik-live/director/$f.new ;; esac; mv /opt/yantrik-live/director/$f.new /opt/yantrik-live/director/$f" < "$f"
done
for u in yantrik-live-director.service yantrik-live-director.timer yantrik-live-showcase.service; do
  guest "set -e; cat > /etc/systemd/system/$u; chmod 644 /etc/systemd/system/$u" < "$u"
done
guest 'set -e; python3 -c "import json; json.load(open(\"/opt/yantrik-live/director/missions.json\"))"; systemctl daemon-reload' < /dev/null

# The Mind's desktop bridge without the private-read taint (yos-mcp-sealed says why), named in the
# Mind's MCP settings. 561 only: it is the machine with no personal data on it, and running this
# against a test VM to try the Director must not turn the taint off there (security re-review of
# #822). SEAL=1 asks for it elsewhere, on purpose.
if [ "$VM" = 561 ] || [ "${SEAL:-0}" = 1 ]; then
  # Root writes the wrapper; the settings are the Mind's file, so they are written as its account,
  # never by root through a path that account could have linked elsewhere.
  guest 'set -e; cat > /opt/yantrik-live/yos-mcp-sealed.new; chmod 755 /opt/yantrik-live/yos-mcp-sealed.new; mv /opt/yantrik-live/yos-mcp-sealed.new /opt/yantrik-live/yos-mcp-sealed' < yos-mcp-sealed
  guest 'set -e
runuser -u yantrik-mind -- sh -c "umask 077; cd /var/lib/yantrik-mind/.config/yantrik-mind && { [ -e mcp.json.before-sealed ] || cp mcp.json mcp.json.before-sealed; } && printf %s \"{\\\"mcpServers\\\": {\\\"yantrik-os\\\": {\\\"command\\\": \\\"/opt/yantrik-live/yos-mcp-sealed\\\"}}}\" > mcp.json.new && mv mcp.json.new mcp.json"' < /dev/null
  # With the taint off, the Mind's commands run unasked as the desktop's account, and that account
  # may use the egress proxy's control socket: one command could widen the Mind's policy for good,
  # past the session, the restart and the next "Allow". Here only root may use the socket. An
  # empty EGRESS_OWNER_UID parses to no owner, and this drop-in sorts after the updater's
  # owner.conf, which the updater rewrites and leaves this one alone. Nobody uses Settings ->
  # Network on 561, so nothing is lost.
  guest 'set -e
install -d -m 755 /etc/systemd/system/yantrik-egress.service.d
printf "%s\n" "# deploy/live/director/setup-director.sh: on the live instance only root changes the egress policy." "[Service]" "Environment=EGRESS_OWNER_UID=" > /etc/systemd/system/yantrik-egress.service.d/zz-live-sealed.conf
chmod 644 /etc/systemd/system/yantrik-egress.service.d/zz-live-sealed.conf
systemctl daemon-reload
systemctl restart yantrik-egress
systemctl is-active yantrik-egress
echo "the Mind bridge is sealed and the egress socket is root'"'"'s; restart yantrik-mind for the bridge to take effect"' < /dev/null
fi
if [ "$ENABLE" = 1 ]; then
  guest 'systemctl enable --now yantrik-live-director.timer >/dev/null 2>&1; systemctl is-active yantrik-live-director.timer' < /dev/null
  # Restarted, not only started, so a re-run puts the new showcase code on screen.
  guest 'systemctl enable yantrik-live-showcase.service >/dev/null 2>&1; systemctl restart yantrik-live-showcase.service; systemctl is-active yantrik-live-showcase.service' < /dev/null
else
  echo "timer and showcase left off (ENABLE=0)"
fi
echo "director installed on VM $VM; start a mission now with: systemctl start --no-block yantrik-live-director"
