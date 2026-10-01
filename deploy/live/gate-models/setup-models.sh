#!/bin/sh
# Give the live instance's Mind two cloud providers, Ollama Cloud and NanoGPT, through the gate
# (VM 560), beside the AIG route it already has. Run on node2 as root, from this directory, with
# the two keys on stdin as KEY=value lines; they go straight into the gate (root, 600) and are
# never written on node2 or printed:
#   grep -E '^(OLLAMA_CLOUD_KEY|NANOGPT_KEY)=' keys.env | ssh root@node2 'cd …/gate-models && sh setup-models.sh'
# VM defaults to 560. Needs gate-setup.sh's model proxy in place, and must be run again after
# gate-setup.sh, which rewrites live-model.conf without this directory's include. Run it again
# to change a key; a run nginx refuses puts back everything it touched.
#
# Then the instance's Mind is pointed at these routes (point-mind.sh; see README.md).
set -eu
cd "$(dirname "$0")"
VM=${VM:-560}
. ../guest.sh

keys=$(cat)
value() { printf '%s\n' "$keys" | sed -n "s/^$1=\([A-Za-z0-9._-]\{20,200\}\)\$/\1/p" | head -n 1; }
ollama=$(value OLLAMA_CLOUD_KEY)
nano=$(value NANOGPT_KEY)
# The keys are written into nginx's configuration, so anything outside that character set (a
# quote, a semicolon, a newline) is refused here rather than allowed to become a directive.
[ -n "$ollama" ] && [ -n "$nano" ] \
    || { echo "stdin needs OLLAMA_CLOUD_KEY= and NANOGPT_KEY= lines, each 20-200 of [A-Za-z0-9._-]" >&2; exit 1; }

# Everything this run may change, kept first, so a refused configuration goes back to exactly
# what was there, the previous working routes and keys included.
guest 'set -e
test -s /etc/live-gate/instance-key
test -f /etc/nginx/conf.d/live-model.conf
# A gate whose configuration nginx already refuses is not this script to fix, and its refusal
# would otherwise be reported as the new routes being refused. (2026-10-01: the LAN router
# answered NXDOMAIN for aig.mycluster.cyou, so the AIG route itself would not load.)
if ! nginx -t 2>/dev/null; then nginx -t 2>&1 | tail -2; echo "the gate is refused as it stands; nothing changed" >&2; exit 1; fi
DEBIAN_FRONTEND=noninteractive apt-get -qq install -y libnginx-mod-http-js >/dev/null
umask 077
rm -rf /etc/live-gate/before-cloud
install -d -m 700 /etc/live-gate/before-cloud /etc/nginx/live-routes /etc/nginx/njs
for f in /etc/nginx/conf.d/live-model.conf /etc/nginx/conf.d/live-cloud.conf /etc/nginx/live-routes/cloud.conf \
         /etc/nginx/njs/live_models.js /etc/live-gate/ollama-cloud.auth /etc/live-gate/nanogpt.auth; do
  if [ -e "$f" ]; then cp -p "$f" "/etc/live-gate/before-cloud/$(echo "$f" | tr / _)"; fi
done
echo kept' < /dev/null

# One root-only file per provider, holding the one header nginx adds for it. printf is the
# shell's own, so a key is never on a command line.
auth() { printf 'proxy_set_header Authorization "Bearer %s";\n' "$1"; }
auth "$ollama" | guest 'set -e; umask 077; cat > /etc/live-gate/ollama-cloud.auth.new; mv /etc/live-gate/ollama-cloud.auth.new /etc/live-gate/ollama-cloud.auth'
auth "$nano" | guest 'set -e; umask 077; cat > /etc/live-gate/nanogpt.auth.new; mv /etc/live-gate/nanogpt.auth.new /etc/live-gate/nanogpt.auth'
guest 'set -e; umask 077; cat > /etc/nginx/njs/live_models.js.new; mv /etc/nginx/njs/live_models.js.new /etc/nginx/njs/live_models.js' < live_models.js

# The routes, with the instance key filled in on the gate itself (by awk reading the key file,
# so it is on no command line), and the server block made to include them.
guest 'set -e
umask 077
case "$(cat /etc/live-gate/instance-key)" in *[!0-9a-f]*|"") echo "the instance key is not hex" >&2; exit 1;; esac
awk "BEGIN { getline k < \"/etc/live-gate/instance-key\" } { gsub(/@INSTANCE_KEY@/, k); print }" \
  > /etc/nginx/live-routes/cloud.conf.new
mv /etc/nginx/live-routes/cloud.conf.new /etc/nginx/live-routes/cloud.conf
# Keyed on the gate address, not the caller address: the instance could add addresses of its own on
# the segment, and each would get a limit of its own. There is one instance; there is one limit.
cat > /etc/nginx/conf.d/live-cloud.conf <<EOF
# live-gate (deploy/live/gate-models): rates, daily counts and the filter of the cloud routes.
limit_req_zone \$server_addr zone=live_ollama_cloud:1m rate=30r/m;
limit_req_zone \$server_addr zone=live_nanogpt:1m rate=10r/m;
js_shared_dict_zone zone=live_budget:64k type=number timeout=3d state=/var/lib/nginx/live_budget.json;
js_import live from /etc/nginx/njs/live_models.js;
EOF
# The zones file of the first version named the same zones; left beside this one, nginx refuses both.
rm -f /etc/nginx/conf.d/live-cloud-zones.conf
grep -q "include /etc/nginx/live-routes/" /etc/nginx/conf.d/live-model.conf \
  || sed -i "s|^    location / { return 404; }|    include /etc/nginx/live-routes/*.conf;\n    location / { return 404; }|" /etc/nginx/conf.d/live-model.conf
chmod 600 /etc/nginx/conf.d/live-model.conf
if ! grep -q "include /etc/nginx/live-routes/" /etc/nginx/conf.d/live-model.conf || ! nginx -t 2>/dev/null; then
  nginx -t 2>&1 | tail -3 || true
  for f in /etc/nginx/conf.d/live-model.conf /etc/nginx/conf.d/live-cloud.conf /etc/nginx/live-routes/cloud.conf \
           /etc/nginx/njs/live_models.js /etc/live-gate/ollama-cloud.auth /etc/live-gate/nanogpt.auth; do
    kept="/etc/live-gate/before-cloud/$(echo "$f" | tr / _)"
    if [ -e "$kept" ]; then cp -p "$kept" "$f"; else rm -f "$f"; fi
  done
  nginx -t 2>/dev/null && systemctl reload nginx
  echo "nginx refused the cloud routes; everything is as it was" >&2
  exit 1
fi
systemctl reload nginx
echo "cloud routes live: /ollama-cloud/v1/chat/completions, /nanogpt/api/v1/chat/completions"' < routes.conf
