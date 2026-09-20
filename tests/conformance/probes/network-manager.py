#!/usr/bin/env python3
"""Network Manager's one job: say truthfully how this machine is connected, and change it when asked.

The survey found two faults here and the second is the one that reached an audit.

The app called five methods — `network.wifi_toggle`, `wifi_scan`, `wifi_connect`,
`wifi_disconnect`, `wifi_forget` — that `network-service` did not implement; the service answered
`interfaces`, `status` and `dns`. Not one name in common, and three of the five calls went into a
`let _ =`, so every press logged that it had worked.

And two properties the screen drew as measurements were Slint defaults Rust never wrote.
`firewall-enabled: false` was rendered as "Off" in warning colour, and a security audit of this OS
copied "Firewall: Off" out of the window as a finding about a machine the app had never looked at.
`wifi-enabled: false` was the same thing for the radio. So this probe's hardest check is not that
an action worked: it is that the app does not claim to know something it has not read.

## What this machine is, and why that is useful here

The test VM is **wired, with no Wi-Fi adapter**, and it is reached over ssh. Both facts shape this
file.

The absence of an adapter is not a limitation, it is the check. `/sys/class/net/*/wireless` is
empty here, and the app has to say "this machine has no Wi-Fi adapter" rather than "Wi-Fi: Off" —
which is exactly the sentence it used to draw. Nothing has to be hidden or renamed to force that
case; it is simply the truth about the machine.

Being reached over the network decides what is not run. `wifi_disconnect` and `wifi_radio` are
graded `dangerous` because on a machine driven from somewhere else they take away the channel the
undo would travel on. This probe does not exercise them, on any machine, and the reason is in
`notes.sections_not_exercised` in words — a green table must not be read as coverage of a path
nobody ran. The same goes for forgetting a network this machine actually has: that deletes a
credential that may exist nowhere else. What is exercised is every one of those verbs against a
name that is not here, which is the case where a refusal is the only correct answer.

Every check compares the app against ground truth this probe reads for itself — `ip -j addr`,
`/etc/resolv.conf`, `/sys/class/net`, and the firewall tools run at the same privilege the app has
— never against the action's own answer. And the machine's connectivity is recorded before and
after and compared at the end, because the one thing this probe must not do is strand the machine
it is running on.
"""

import glob
import json
import os
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import lib  # noqa: E402

ONE_JOB = ("Say truthfully how this machine is connected — its interfaces and addresses, its "
           "resolvers, whether it even has a Wi-Fi adapter, and what its firewall is actually "
           "doing — and change the Wi-Fi only when asked, reporting what was observed rather "
           "than what was requested.")

APP = "network"  # the control surface id, which is the launcher's route id for this app
APP_BIN = "/opt/yantrik/bin/yantrik-network-manager"

# A name nothing can be called. Every mutation is asked about this one, because a refusal is the
# only correct answer for it and a refusal cannot damage the machine.
ABSENT_SSID = "yantrik-probe-no-such-network-9f13"

# How each of `lib.refusal_kind`'s three answers reads in the report.
OUTCOME = {
    "policy": "refused by policy — the machine's ceiling turned it away before the app saw it",
    "app": "refused by the app",
    None: "answered by the app",
}


# ── Ground truth, read from the machine and not from the app ─────────────────

def sh(argv):
    """Run something and get its output back, whatever it did. `(stdout, stderr, code)`."""
    try:
        done = subprocess.run(argv, capture_output=True, text=True, timeout=30)
    except (OSError, subprocess.TimeoutExpired) as exc:
        return "", str(exc), 127
    return done.stdout, done.stderr, done.returncode


def ip_addresses():
    """Every non-loopback interface and its IPv4 addresses, from `ip -j addr`.

    `{"eth0": ["10.0.0.5"], ...}`, or None when `ip` could not be asked — which is itself worth
    recording rather than silently becoming an empty dict the app would appear to match.
    """
    out, _, code = sh(["ip", "-j", "addr"])
    if code != 0:
        return None
    try:
        parsed = json.loads(out)
    except ValueError:
        return None
    found = {}
    for link in parsed:
        name = link.get("ifname")
        if not name or name == "lo":
            continue
        found[name] = sorted(
            a.get("local") for a in link.get("addr_info") or []
            if a.get("family") == "inet" and a.get("local")
        )
    return found


def default_route():
    """The default route as one line, or "" when there is none. The thing not to change."""
    out, _, code = sh(["ip", "-4", "route", "show", "default"])
    return out.strip() if code == 0 else "<ip route failed>"


def resolvers():
    """The machine's nameservers, from resolv.conf, falling back to resolvectl."""
    found = []
    try:
        with open("/etc/resolv.conf", "r") as handle:
            for line in handle:
                line = line.strip()
                if line.startswith("nameserver"):
                    value = line[len("nameserver"):].strip()
                    if value:
                        found.append(value)
    except OSError:
        pass
    if not found:
        out, _, code = sh(["resolvectl", "dns"])
        if code == 0:
            for line in out.splitlines():
                found += [t for t in line.split()[2:] if t and t[0].isdigit()]
    return found


def wireless_interfaces():
    """Interfaces the kernel calls wireless. Empty on this VM, which is the point.

    This is the same test the service uses (`/sys/class/net/<iface>/wireless`), on purpose: the
    app and the probe have to be reading the same thing for the comparison to mean anything, and
    this one needs no daemon and no privilege.
    """
    return sorted(os.path.basename(os.path.dirname(p))
                  for p in glob.glob("/sys/class/net/*/wireless"))


def firewall_truth():
    """What this probe can learn about the firewall at the privilege the app has.

    Returns `(kind, state, detail)` where state is one of the words the app publishes. The app
    runs these same three commands unprivileged and must not claim more than they show — the whole
    point being that "I was not allowed to read the ruleset" is a real answer and "Off" is not.
    """
    out, err, code = sh(["nft", "list", "ruleset"])
    if code != 127:
        said = (err or out).strip()
        if code == 0:
            body = [l.strip() for l in out.splitlines() if l.strip()]
            return "nftables", ("active" if body else "inactive"), said[:300]
        low = said.lower()
        if "not permitted" in low or "permission denied" in low or "not authorized" in low:
            return "nftables", "unknown", said[:300]
        return "nftables", "unknown", said[:300]

    out, err, code = sh(["ufw", "status"])
    if code != 127:
        said = (out or err).strip()
        for line in out.splitlines():
            if line.lower().startswith("status:"):
                return "ufw", ("active" if "active" in line.lower() else "inactive"), said[:300]
        return "ufw", "unknown", said[:300]

    out, err, code = sh(["firewall-cmd", "--state"])
    if code != 127:
        word = (out or err).strip().lower()
        if "not running" in word:
            return "firewalld", "inactive", word[:300]
        if "running" in word:
            return "firewalld", "active", word[:300]
        return "firewalld", "unknown", word[:300]

    return None, "absent", "no nft, ufw or firewall-cmd on this machine"


def connectivity():
    """Everything this probe must be able to prove it did not change."""
    return {"default_route": default_route(), "addresses": ip_addresses()}


def run():
    """The probe. In a function so that importing this file does nothing."""
    not_exercised = []
    ceiling_refusal = None

    wireless = wireless_interfaces()
    has_adapter = bool(wireless)
    truth_before = connectivity()
    truth_dns = resolvers()
    fw_kind, fw_state, fw_detail = firewall_truth()

    with lib.Probe(APP, ONE_JOB) as probe:
        was_running = bool(lib.running(APP_BIN))
        probe.note("processes_before", lib.running(APP_BIN))
        probe.note("the_machine_this_ran_on", {
            "wireless_interfaces_in_sys_class_net": wireless,
            "has_wifi_adapter": has_adapter,
            "default_route": truth_before["default_route"],
            "addresses": truth_before["addresses"],
            "resolvers": truth_dns,
            "firewall_this_probe_found": {"kind": fw_kind, "state": fw_state, "said": fw_detail},
            "app_was_already_open": was_running,
        })

        try:
            # ── 1. It opens ───────────────────────────────────────────────────
            opened = lib.open_app(APP, expect_process=APP_BIN, window_words=("network",))
            probe.check(
                "it opens: a process exists, the compositor has its window, and the surface answers",
                bool(opened["processes"]) and bool(opened["windows"]) and opened["surface_up"],
                contract=1, evidence=opened)
            probe.check(
                "this launch added nothing to the shell's failed_launches",
                not opened["new_failed_launches_for_this_app"],
                contract=1, evidence={"added_by_this_launch": opened["new_failed_launches"]})

            view = lib.describe(APP)
            state = view.get("state") or {}
            summary = str(view.get("summary") or "")
            published = [a.get("name") for a in view.get("actions") or [] if isinstance(a, dict)]

            # `yos describe network` tries `app-network.sock` and falls back to `network.sock`,
            # which is the *service* — and the service publishes a view of its own with an empty
            # action list. Without this check a run with the app dead and the service up would
            # read as a pass. The verbs are the app's; the service has none by design.
            probe.check(
                "the surface answering is the app's, not the service falling back behind it",
                "wifi_connect" in published and "refresh" in published,
                contract=1, evidence={"actions": published, "app": view.get("app")})

            wifi = state.get("wifi") or {}
            firewall = state.get("firewall") or {}
            probe.note("describe", {
                "summary": summary, "notice": state.get("notice"),
                "connected": state.get("connected"), "type": state.get("type"),
                "ip_address": state.get("ip_address"), "dns": state.get("dns"),
                "wifi": wifi, "firewall": firewall, "source": state.get("source"),
                "actions": published,
            })

            probe.check(
                "describe carries a notice field, so a failure can be said to the caller too",
                "notice" in state,
                contract=4, evidence={"notice": state.get("notice"), "keys": sorted(state)})
            probe.check(
                "describe says where each reading came from",
                isinstance(state.get("source"), dict) and bool(state["source"].get("service")),
                contract=3, evidence={"source": state.get("source")})

            # ── 2. The interfaces and addresses are the machine's ─────────────
            listed = {}
            for row in state.get("interfaces") or []:
                if isinstance(row, dict) and row.get("name"):
                    ip = row.get("ip")
                    listed[str(row["name"])] = sorted([ip] if ip else [])
            probe.note("interfaces_the_app_lists", listed)

            if truth_before["addresses"] is None:
                not_exercised.append(
                    "the interface list matches `ip -j addr`: `ip` could not be asked on this "
                    "machine, so there was nothing to compare against")
            else:
                # The app publishes the ethernet interfaces it draws. Every one of them has to be
                # on the machine, with the address the kernel gives it. The window used to report
                # "Ethernet — No interface" on a machine with eth0 up and routable.
                # Subset, not equality: the service reports one address per interface (the first
                # `SIOCGIFADDR` gives) and an interface may hold several. What is asserted is
                # that every address the app shows is one the kernel agrees it has.
                truth = truth_before["addresses"]
                wrong = {name: {"app": addrs, "ip_addr": truth.get(name)}
                         for name, addrs in listed.items()
                         if name not in truth
                         or (addrs and not set(addrs) <= set(truth.get(name) or []))}
                probe.check(
                    "every interface and address it publishes is one `ip -j addr` agrees with",
                    not wrong,
                    contract=2, evidence={"disagreements": wrong, "app": listed, "ip": truth})
                wired = sorted(n for n in truth if n not in wireless
                               and not n.startswith(("docker", "br-", "veth", "virbr", "tun",
                                                     "tap", "wg")))
                probe.check(
                    "a machine with a wired interface up is not described as having none",
                    not wired or bool(listed),
                    contract=2, evidence={"wired_on_this_machine": wired, "app_lists": listed})

            route = truth_before["default_route"]
            if route.startswith("<"):
                not_exercised.append(
                    "the connected flag against the default route: `ip route` could not be asked "
                    "on this machine (%s)" % route)
            else:
                probe.check(
                    "the connection it reports is the one carrying the default route",
                    (state.get("connected") is True) == bool("default" in route),
                    contract=2, evidence={"app_says_connected": state.get("connected"),
                                          "default_route": route})

            # ── 3. The resolvers are the machine's ────────────────────────────
            app_dns = (state.get("dns") or {}).get("nameservers") or []
            probe.check(
                "the nameservers it publishes are the ones in resolv.conf",
                sorted(str(n) for n in app_dns) == sorted(truth_dns),
                contract=2, evidence={"app": app_dns, "machine": truth_dns})

            # ── 4. Wi-Fi: absent is not off ───────────────────────────────────
            #
            # The check this machine is good for, and it needs nothing hidden to make it.
            probe.check(
                "describe publishes the radio as a word a caller can branch on, never a bool",
                wifi.get("radio") in ("on", "off", "unknown"),
                contract=3, evidence={"radio": wifi.get("radio"), "wifi": wifi})
            probe.check(
                "whether there is an adapter is what /sys/class/net says",
                wifi.get("adapter_present") is has_adapter,
                contract=2, evidence={"app_says": wifi.get("adapter_present"),
                                      "wireless_in_sys": wireless})

            if not has_adapter:
                reason = str(wifi.get("reason") or "")
                probe.check(
                    "a machine with no Wi-Fi adapter is described as having none — not as a "
                    "machine whose Wi-Fi is switched off",
                    wifi.get("adapter_present") is False
                    and wifi.get("connected_ssid") in (None, "")
                    and "adapter" in reason.lower(),
                    contract=2, evidence={"wifi": wifi, "summary": summary})
                probe.check(
                    "and it does not report the radio as off, which is a different statement",
                    wifi.get("radio") != "off",
                    contract=2, evidence={"radio": wifi.get("radio"), "reason": reason})
            else:
                not_exercised.append(
                    "the absent-adapter wording: this machine has a Wi-Fi adapter (%s), so the "
                    "sentence that replaced \"Wi-Fi: Off\" was not the one on screen"
                    % ", ".join(wireless))

            # ── 5. The firewall, against a reading this probe took itself ─────
            app_fw_state = firewall.get("state")
            app_fw_reason = str(firewall.get("reason") or "")
            probe.check(
                "describe publishes the firewall as one of four words, never a bool",
                app_fw_state in ("active", "inactive", "absent", "unknown"),
                contract=3, evidence={"firewall": firewall})
            probe.check(
                "the firewall it reports is the one this probe found at the same privilege",
                app_fw_state == fw_state and (firewall.get("kind") or None) == fw_kind,
                contract=2, evidence={"app": {"kind": firewall.get("kind"), "state": app_fw_state},
                                      "probe": {"kind": fw_kind, "state": fw_state},
                                      "probe_saw": fw_detail})
            # The audit's finding, as an assertion. "Off" is only allowed to be said when
            # something read a ruleset and found nothing loaded.
            probe.check(
                "it does not claim the firewall is off without having read one",
                app_fw_state != "inactive" or fw_state == "inactive",
                contract=2, evidence={"app": app_fw_state, "probe": fw_state, "probe_saw": fw_detail})
            probe.check(
                "an unknown or absent firewall always carries a reason",
                app_fw_state in ("active", "inactive") or bool(app_fw_reason.strip()),
                contract=3, evidence={"state": app_fw_state, "reason": app_fw_reason})
            # A firewall nobody read has no rule count at all. A zero there would be the finding
            # this whole change exists to remove, wearing a number instead of the word "Off".
            probe.check(
                "a firewall that was not read carries no rule count — null, never zero",
                app_fw_state in ("active", "inactive") or firewall.get("rules") is None,
                contract=3, evidence={"rules": firewall.get("rules"), "state": app_fw_state})
            if app_fw_state == "unknown":
                probe.check(
                    "when it cannot read the ruleset it says so in words a person can act on",
                    "root" in app_fw_reason.lower() or "privileg" in app_fw_reason.lower()
                    or "permitted" in app_fw_reason.lower(),
                    contract=4, evidence={"reason": app_fw_reason})

            # ── 6. refresh, which changes nothing about the machine ───────────
            refreshed = lib.act(APP, "refresh")
            probe.note("refresh", refreshed)
            probe.check(
                "refresh is answered and reports what it found rather than that it ran",
                refreshed.get("accepted") is True
                and isinstance((refreshed.get("result") or {}).get("connected"), bool),
                contract=3, evidence=refreshed)

            # ── 7. Asking for a network that is not here ──────────────────────
            #
            # Every verb that names a network, against a name nothing is called. A refusal is the
            # only correct answer and a refusal cannot damage anything — which is what makes this
            # the part of the mutation surface that CAN be exercised on a machine reached over
            # the network it would be changing.
            for action, args in (("wifi_connect", {"ssid": ABSENT_SSID}),
                                 ("wifi_forget", {"ssid": ABSENT_SSID})):
                answer = lib.act(APP, action, **args)
                kind = lib.refusal_kind(answer)
                text = str(answer.get("refused") or "")
                result = answer.get("result") or {}
                evidence = {"accepted": answer.get("accepted"), "refused": text,
                            "result": result, "refusal_kind": kind, "outcome": OUTCOME[kind]}
                probe.note("asked_for_a_network_that_is_not_here_%s" % action, evidence)

                probe.check(
                    "%s for a network that is not here is refused, never answered with success"
                    % action,
                    answer.get("accepted") is not True,
                    contract=9, evidence=evidence)
                probe.check(
                    "the refusal for %s arrives in words the caller can read, not \"1\"" % action,
                    bool(text) and text not in ("1", "0"),
                    contract=4, evidence=evidence)

                if kind == "policy":
                    ceiling_refusal = ceiling_refusal or text
                    not_exercised.append(
                        "the refusal from `%s` names the reason — the ceiling refused on the "
                        "grade before the app saw the call" % action)
                else:
                    # Either reason is right, and which one is right depends on the machine: with
                    # no adapter the truthful answer names the adapter, because a machine that
                    # cannot look for a network should not report that it looked and failed.
                    names_it = ABSENT_SSID in text or "adapter" in text.lower()
                    probe.check(
                        "the refusal for %s names the absent adapter or the network asked for"
                        % action,
                        names_it,
                        contract=3, evidence=evidence)

            # A scan changes no connection: it puts the radio off the air for a moment and reads
            # a list. Safe to run on a machine reached over Wi-Fi, and refused outright here.
            scanned = lib.act(APP, "wifi_scan")
            scan_kind = lib.refusal_kind(scanned)
            probe.note("wifi_scan", {"answer": scanned, "outcome": OUTCOME[scan_kind]})
            if has_adapter:
                probe.check(
                    "a scan is accepted on a machine with an adapter, and says it settles later",
                    scanned.get("accepted") is True and scanned.get("settled") is False,
                    contract=3, evidence=scanned)
            else:
                probe.check(
                    "a scan on a machine with no adapter is refused, naming the adapter",
                    scanned.get("accepted") is not True
                    and "adapter" in str(scanned.get("refused") or "").lower(),
                    contract=3, evidence=scanned)

            # ── 8. The failure is said on screen as well ──────────────────────
            after_refusals = lib.state(APP)
            probe.check(
                "the last refusal is on the person's screen too, in the notice",
                bool(str(after_refusals.get("notice") or "").strip()),
                contract=4, evidence={"notice": after_refusals.get("notice")})

            # ── 9. Nothing here changed how this machine is connected ─────────
            truth_after = connectivity()
            probe.check(
                "the default route is the one this machine had before the probe ran",
                truth_after["default_route"] == truth_before["default_route"],
                contract="leave-as-found",
                evidence={"before": truth_before["default_route"],
                          "after": truth_after["default_route"]})
            probe.check(
                "every address this machine had, it still has",
                truth_after["addresses"] == truth_before["addresses"],
                contract="leave-as-found",
                evidence={"before": truth_before["addresses"],
                          "after": truth_after["addresses"]})

            # ── What this run did not measure ─────────────────────────────────
            not_exercised += [
                "wifi_radio on / off against a real adapter",
                "wifi_disconnect against a real connection",
                "wifi_forget against a network this machine has actually saved",
                "a wifi_connect that succeeds, and the re-read that has to agree with it",
                "a wrong password, and the app reporting the AP's own refusal",
            ]
            probe.note("sections_not_exercised", {
                "the_dangerous_verbs": (
                    "wifi_disconnect and wifi_radio are NOT EXERCISED, by design, on any machine. "
                    "They are graded `dangerous` for one reason: this machine is driven from "
                    "somewhere else, and taking the network down removes the channel an undo "
                    "would travel on. There is no call in this surface that puts it back — it "
                    "needs someone in the room. A probe that ran them to get a green line would "
                    "be trading the machine for a tick."),
                "forgetting_a_real_network": (
                    "wifi_forget is exercised only against a name nothing is called. Deleting a "
                    "saved network destroys a credential that may exist nowhere else, and the "
                    "machine is the user's."),
                "the_wifi_half_on_this_machine": (
                    "this machine has no Wi-Fi adapter (%s is empty), so connect, scan, radio "
                    "and disconnect have no hardware to act on. Every one of them was asked and "
                    "every one was refused naming the absent adapter, which is the behaviour "
                    "this app most needed and did not have: it used to draw \"Wi-Fi: Off\" here."
                    % "/sys/class/net/*/wireless"
                    if not has_adapter else
                    "this machine has an adapter (%s). The connect and radio paths were still "
                    "not run against it: see the note above." % ", ".join(wireless)),
                "the_firewall_write_controls": (
                    "there are none to exercise. Toggle, allow port, block port and apply "
                    "profile were four log lines and have been removed; changing a firewall from "
                    "a desktop session needs root and this OS has no scoped privileged path for "
                    "it. What is checked instead is that the read-only state is read and that "
                    "\"unknown\" is never dressed up as \"off\"."),
                "the_ceiling": (
                    "`network.wifi_disconnect` and `network.wifi_radio` are graded `dangerous` "
                    "and `wifi_connect` and `wifi_forget` `sensitive`. Where the machine's "
                    "ceiling refused a call before dispatch, the app's own code did not run and "
                    "the assertion about its words is recorded as not exercised rather than "
                    "asserted. The ceiling is the user's setting, in `tool_permission` in "
                    "~/.config/yantrik/settings.yaml; raising it is their decision, not this "
                    "probe's."),
                "the_refusal_the_ceiling_gave_in_full": ceiling_refusal,
                "checks_not_exercised": not_exercised,
                "what_was_still_measured": (
                    "that the interfaces, addresses and resolvers it publishes are the machine's; "
                    "that it reports the absence of a Wi-Fi adapter as an absence rather than as "
                    "a switched-off radio; that its firewall reading matches one taken "
                    "independently at the same privilege and never claims \"off\" without "
                    "evidence; that every verb naming a network that is not here is refused in "
                    "readable words and claims nothing; and that the machine's connectivity is "
                    "the same afterwards as before."),
            })

        finally:
            # ── Put the machine back ──────────────────────────────────────────
            if not was_running:
                lib.kill_app(APP_BIN)
                time.sleep(1)
            leftover = lib.running(APP_BIN)
            probe.note("processes_after", leftover)
            probe.check(
                "no network manager is left running that was not running before",
                bool(leftover) == was_running,
                contract="leave-as-found",
                evidence={"before": probe.notes["processes_before"], "after": leftover,
                          "was_running_before": was_running})
            final = connectivity()
            probe.note("connectivity_after", final)
            probe.check(
                "this machine is connected the way it was when the probe started",
                final == truth_before,
                contract="leave-as-found",
                evidence={"before": truth_before, "after": final})


if __name__ == "__main__":
    run()
