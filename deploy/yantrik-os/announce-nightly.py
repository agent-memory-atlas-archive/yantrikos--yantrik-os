#!/usr/bin/env python3
"""Tell the community Discord that a nightly has published: version, link, size, checksum, what changed.

    announce-nightly.py <channel> <iso path> <version> [<previous git hash>] [--dry-run]

The webhook comes from $DISCORD_NIGHTLY_WEBHOOK and is never printed. With no webhook, or with
--dry-run, the message is printed instead of sent. It never fails the build: a nightly that
published and could not say so is still a published nightly, and the step that calls this is
marked continue-on-error for the same reason.

"What changed" is the non-merge commit subjects since the build the channel held before this one.
They are written as sentences about what a person hit, so they read as release notes without
being rewritten into release notes.
"""
import hashlib
import json
import os
import subprocess
import sys
import urllib.request

LIMIT = 15          # a message is for reading; the full list is on GitHub
SKY = 0x5EB8FF


def changes(previous):
    if not previous:
        return []
    try:
        out = subprocess.run(["git", "log", "--no-merges", "--format=%s", f"{previous}..HEAD"],
                             capture_output=True, text=True, check=True).stdout
    except (subprocess.CalledProcessError, FileNotFoundError):
        return []
    return [line.strip() for line in out.splitlines() if line.strip()]


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def message(channel, iso, version, previous):
    name = os.path.basename(iso)
    size = os.path.getsize(iso)
    lines = changes(previous)
    shown = [f"• {s}" for s in lines[:LIMIT]]
    if len(lines) > LIMIT:
        shown.append(f"• …and {len(lines) - LIMIT} more")
    body = "**In this build**\n" + "\n".join(shown) if shown else "Rebuilt from the same source as the last nightly."
    return {"embeds": [{
        "title": f"{version}",
        "url": f"https://iso.yantrikos.com/{channel}/{name}",
        "color": SKY,
        "description": body[:3900],
        "fields": [
            {"name": "Size", "value": f"{size / 1e9:.2f} GB", "inline": True},
            {"name": "Channel", "value": channel, "inline": True},
            {"name": "sha256", "value": f"`{sha256(iso)}`", "inline": False},
        ],
        "footer": {"text": "Built, boot-tested and published by CI"},
    }]}


def main(argv):
    dry = "--dry-run" in argv
    args = [a for a in argv if a != "--dry-run"]
    if len(args) < 3:
        print(__doc__)
        return 0
    channel, iso, version = args[:3]
    previous = args[3] if len(args) > 3 else ""
    payload = message(channel, iso, version, previous)
    hook = os.environ.get("DISCORD_NIGHTLY_WEBHOOK", "").strip()
    if dry or not hook:
        print(json.dumps(payload, indent=2))
        if not hook and not dry:
            print("DISCORD_NIGHTLY_WEBHOOK is not set; nothing was sent.")
        return 0
    req = urllib.request.Request(hook + "?wait=true", data=json.dumps(payload).encode(), method="POST",
                                 headers={"Content-Type": "application/json", "User-Agent": "yantrik-nightly (1.0)"})
    try:
        urllib.request.urlopen(req, timeout=30).read()
        print(f"Told Discord about {version}.")
    except Exception as e:  # never the webhook URL: only what went wrong
        print(f"Discord did not take the message: {type(e).__name__}: {getattr(e, 'code', '')}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
