# The public server's half of publishing

Two names on one machine (`15.204.233.63`, user `ubuntu`), both plain nginx directories:

| Name | Root | What is in it |
|---|---|---|
| `iso.yantrikos.com` | `/var/www/iso.yantrikos.com/{nightly,beta,stable}` | ISOs, a `.sha256` beside each, `yantrik-os-latest.iso` → the newest |
| `releases.yantrikos.com` | `/var/www/releases.yantrikos.com/{nightly,beta,stable}` + `manifest.json` | what `yantrik-update` reads: the bundle, its `.sha256`, `yantrik-os-latest-linux-amd64.tar.zst`, and the manifest that names each channel's current build and hash |

`releases.yantrikos.com` also exists *inside* the author's network, at a private address, fed by
`build-release.sh --publish`. Same name, split DNS, different machine: what is published there
does not appear here. The pipeline publishes here.

## The rule about space

**Each channel keeps its newest three builds.** The disk is shared with eighteen other sites
and an ISO is 1.4 GB. The rule is enforced by `yantrik-publish` on the server, at the moment
a build arrives — not by the client, and not by a cron job that can stop running. The build
`latest` points at is never deleted.

## The key

The pipeline holds one secret, `ISO_PUBLISH_KEY`. On the server that key's line in
`~ubuntu/.ssh/authorized_keys` is:

    command="/usr/local/bin/yantrik-publish",restrict ssh-ed25519 AAAA… yantrik-os-ci-publish-<date>

`restrict` turns off port forwarding, agent forwarding, X11 and the pty; `command=` means that
whatever the client asks to run, this runs instead, and receives the request as text in
`SSH_ORIGINAL_COMMAND`. It parses that text and never executes it. So the key can put an ISO or
a release bundle into a channel, and nothing else: it cannot read a file, cannot get a shell,
cannot touch the other sites. That is the point — a CI secret is the credential most likely to
end up in a log one day.

Verified when it was installed: `ssh -i key host bash -i`, `… cat /etc/passwd` and a bare
login are each answered by `yantrik-publish: unknown kind …` / `no command was given`, and an
upload whose hash does not match is deleted and changes nothing.

### Rotating it

    ssh-keygen -t ed25519 -N "" -C "yantrik-os-ci-publish-$(date +%Y%m%d)" -f ci_publish
    # on the server: replace the old yantrik-os-ci-publish line with
    #   command="/usr/local/bin/yantrik-publish",restrict <contents of ci_publish.pub>
    gh secret set ISO_PUBLISH_KEY -R yantrikos/yantrik-os < ci_publish
    rm ci_publish ci_publish.pub

## Installing or updating the receiver

    scp deploy/yantrik-os/server/yantrik-publish ubuntu@HOST:/tmp/
    ssh ubuntu@HOST 'sudo install -m 755 -o root -g root /tmp/yantrik-publish /usr/local/bin/'

Owned by root so that the account the key logs in as cannot rewrite what the key is allowed to
do. Test it first — it needs nothing but Python and a scratch directory:

    python3 deploy/yantrik-os/server/publish_selftest.py

## The nginx site

`releases.yantrikos.com.nginx` is the port-80 half; `certbot --nginx -d releases.yantrikos.com`
adds the rest. `manifest.json` is served `Cache-Control: no-store`: a machine told that
yesterday's build is current does not update, and the hash it verifies the download against
comes from that file.

## Publishing by hand

    YANTRIK_PUBLISH_KEY=~/.ssh/some_publish_key \
      deploy/yantrik-os/publish.sh iso nightly yantrik-os-<version>.iso

It needs a key whose forced command is `yantrik-publish`; an ordinary login key will open a
shell instead and the upload will be typed into it. `publish.sh` pins the server's host key and,
after the upload, fetches the checksum back over https and compares it with the file in hand.
