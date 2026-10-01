#!/bin/sh
# Give the live instance's Mind two cloud providers, Ollama Cloud and NanoGPT, through the gate
# (VM 560), beside the AIG route it already has. Run on node2 as root, from this directory, with
# the two keys on stdin as KEY=value lines; they go straight into the gate (root, 600) and are
# never written on node2 or printed:
#   grep -E '^(OLLAMA_CLOUD_KEY|NANOGPT_KEY)=' keys.env | ssh root@node2 'cd …/gate-models && sh setup-models.sh'
# VM defaults to 560. Needs gate-setup.sh's model proxy in place. Run it again to change a key.
#
# Then the instance's Mind is pointed at these routes (see README.md): its own settings name the
# gate as each provider's address and carry the instance key where a provider's key would be.
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

# One root-only file per provider, holding the one header nginx adds for it.
auth() { printf 'proxy_set_header Authorization "Bearer %s";\n' "$1"; }
auth "$ollama" | guest 'set -e; umask 077; cat > /etc/live-gate/ollama-cloud.auth.new; mv /etc/live-gate/ollama-cloud.auth.new /etc/live-gate/ollama-cloud.auth'
auth "$nano" | guest 'set -e; umask 077; cat > /etc/live-gate/nanogpt.auth.new; mv /etc/live-gate/nanogpt.auth.new /etc/live-gate/nanogpt.auth'

# The routes, with the instance key filled in on the gate itself, and the server block made to
# include them. A configuration nginx refuses is rolled back, so the AIG route keeps working.
guest 'set -e
test -s /etc/live-gate/instance-key
umask 077
install -d -m 700 /etc/nginx/live-routes
key=$(cat /etc/live-gate/instance-key)
case "$key" in *[!0-9a-f]*|"") echo "the instance key is not hex" >&2; exit 1;; esac
sed "s/@INSTANCE_KEY@/$key/" > /etc/nginx/live-routes/cloud.conf.new
cp -p /etc/nginx/conf.d/live-model.conf /etc/live-gate/live-model.conf.before-cloud
cat > /etc/nginx/conf.d/live-cloud-zones.conf <<EOF
# live-gate (deploy/live/gate-models): request rates for the cloud routes, per instance address.
limit_req_zone \$binary_remote_addr zone=live_ollama_cloud:1m rate=30r/m;
limit_req_zone \$binary_remote_addr zone=live_nanogpt:1m rate=10r/m;
EOF
grep -q "include /etc/nginx/live-routes/" /etc/nginx/conf.d/live-model.conf \
  || sed -i "s|^    location / { return 404; }|    include /etc/nginx/live-routes/*.conf;\n    location / { return 404; }|" /etc/nginx/conf.d/live-model.conf
grep -q "include /etc/nginx/live-routes/" /etc/nginx/conf.d/live-model.conf
mv /etc/nginx/live-routes/cloud.conf.new /etc/nginx/live-routes/cloud.conf
if ! nginx -t 2>/dev/null; then
  cp -p /etc/live-gate/live-model.conf.before-cloud /etc/nginx/conf.d/live-model.conf
  rm -f /etc/nginx/live-routes/cloud.conf /etc/nginx/conf.d/live-cloud-zones.conf
  nginx -t 2>&1 | tail -2
  echo "nginx refused the cloud routes; put back as it was" >&2
  exit 1
fi
systemctl reload nginx
echo "cloud routes live: /ollama-cloud/v1/chat/completions, /nanogpt/api/v1/chat/completions"' < routes.conf
