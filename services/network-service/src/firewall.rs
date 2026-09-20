//! Which firewall this machine runs, and whether it is running.
//!
//! # What was here before
//!
//! Nothing. `in property <bool> firewall-enabled: false` on the Network Manager screen, never
//! written from Rust, drawn as `firewall-enabled ? "{rule-count} rules" : "Off"` in warning
//! colour. A security audit of this OS recorded "Firewall: Off" as a finding. The app had never
//! looked at a firewall in its life; it was reporting a Slint default as a measurement.
//!
//! # What this OS actually ships
//!
//! Nothing, is the honest answer. `deploy/yantrik-os/build-debian-iso.sh` debootstraps
//! `--variant=minbase`, which is priority-required packages only, and its four `apt-get install`
//! lists contain no `nftables`, no `iptables`, no `ufw` and no `firewalld`;
//! `deploy/yantrik-os/cloud-init/user-data.yaml` installs none of them either. So on an ISO-built
//! machine the answer is very likely [`FirewallStatus::Absent`] — not "off" — and on a machine
//! built from a distribution cloud image it depends what that image carried. Which is precisely
//! why this is detected at runtime and reported with the tool's name, instead of being assumed.
//!
//! # Reading it without becoming root
//!
//! `nft list ruleset` and `ufw status` both need CAP_NET_ADMIN, which this session does not have.
//! The images built by `deploy/` do write `yantrik ALL=(ALL) NOPASSWD: ALL` into `/etc/sudoers.d`,
//! so `sudo -n nft list ruleset` would in fact succeed there — and this module does not use it.
//! A desktop app that silently becomes root to draw a status line is a surprise, the reading
//! would be unreproducible on any machine without that sudoers file, and "I could not read it"
//! is a true and useful thing to say. So: read unprivileged, and when that is refused, report
//! [`FirewallStatus::Unknown`] with the tool's own refusal as the reason.
//!
//! Nothing here changes a firewall. The four write controls that used to be on the screen —
//! toggle, allow port, block port, apply profile — were log lines and are gone; see
//! `design/network-2026-09-20.md` for why they are not coming back through this module.
//!
//! As with `nmcli.rs`, the decisions live in pure functions over captured text so they can be
//! tested on a machine with no firewall on it, which is every machine this was written on.

use std::process::Command;

use yantrik_ipc_contracts::network::{FirewallRuleInfo, FirewallState, FirewallStatus};

use crate::nmcli::{first_line, Exit};

/// The firewalls this looks for, in the order it prefers them.
///
/// nftables first because it is Debian's, and Debian is what the ISO is built from. `ufw` is a
/// front end to the same kernel subsystem, so a machine with both is reported as nftables, which
/// is the layer that is actually filtering.
pub const CANDIDATES: [(&str, &str); 3] = [
    ("nftables", "nft"),
    ("ufw", "ufw"),
    ("firewalld", "firewall-cmd"),
];

/// Run a firewall tool once, read-only, and keep what it said.
pub fn run(command: &str, argv: &[&str]) -> Exit {
    match Command::new(command).args(argv).output() {
        Ok(out) => Exit::Ran {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Exit::Missing,
        Err(e) => Exit::Unstartable(e.to_string()),
    }
}

/// Whether a failure was a privilege refusal, and the tool's own words for it.
///
/// Every one of these tools says something different and all three mean the same thing: this
/// session may not read the ruleset. Their sentence is kept, because "you need to be root to run
/// this script" tells a person what to do and "unknown" does not.
pub fn refused_for_privilege(said: &str) -> bool {
    let low = said.to_lowercase();
    low.contains("operation not permitted")
        || low.contains("permission denied")
        || low.contains("you need to be root")
        || low.contains("must be root")
        || low.contains("not authorized")
        || low.contains("access denied")
}

/// The whole reading for nftables, from what `nft list ruleset` did.
pub fn read_nftables(exit: &Exit) -> FirewallState {
    match exit {
        Exit::Missing => absent(),
        Exit::Unstartable(why) => unknown(
            "nftables",
            format!("nft is on this machine but would not start: {why}"),
        ),
        Exit::Ran {
            code: Some(0),
            stdout,
            ..
        } => {
            let rules = parse_nft_ruleset(stdout);
            if rules.is_empty() {
                // nft ran, was allowed to read, and there is nothing loaded. This is the one
                // path on which "not filtering" is a thing that was measured rather than assumed.
                FirewallState {
                    kind: Some("nftables".to_string()),
                    state: FirewallStatus::Inactive,
                    rule_count: Some(0),
                    rules: Vec::new(),
                    reason: None,
                }
            } else {
                FirewallState {
                    kind: Some("nftables".to_string()),
                    state: FirewallStatus::Active,
                    rule_count: Some(rules.len() as i64),
                    rules,
                    reason: None,
                }
            }
        }
        Exit::Ran { stdout, stderr, .. } => {
            let said = said_by(stderr, stdout, "nft would not say");
            if refused_for_privilege(&said) {
                unknown(
                    "nftables",
                    format!(
                        "reading the nftables ruleset needs root and this session is not \
                         privileged: {said}"
                    ),
                )
            } else {
                unknown("nftables", said)
            }
        }
    }
}

/// The whole reading for ufw, from what `ufw status verbose` did.
pub fn read_ufw(exit: &Exit) -> FirewallState {
    match exit {
        Exit::Missing => absent(),
        Exit::Unstartable(why) => unknown(
            "ufw",
            format!("ufw is on this machine but would not start: {why}"),
        ),
        Exit::Ran {
            code: Some(0),
            stdout,
            ..
        } => {
            let Some(active) = parse_ufw_status(stdout) else {
                return unknown(
                    "ufw",
                    format!(
                        "ufw ran but said nothing this could read: {}",
                        first_line(stdout).unwrap_or_default()
                    ),
                );
            };
            let rules = parse_ufw_rules(stdout);
            FirewallState {
                kind: Some("ufw".to_string()),
                state: if active {
                    FirewallStatus::Active
                } else {
                    FirewallStatus::Inactive
                },
                rule_count: Some(rules.len() as i64),
                rules,
                reason: None,
            }
        }
        Exit::Ran { stdout, stderr, .. } => {
            let said = said_by(stderr, stdout, "ufw would not say");
            if refused_for_privilege(&said) {
                unknown(
                    "ufw",
                    format!("reading ufw's status needs root and this session is not privileged: {said}"),
                )
            } else {
                unknown("ufw", said)
            }
        }
    }
}

/// The whole reading for firewalld, from what `firewall-cmd --state` did.
///
/// firewalld answers this one over D-Bus and an ordinary user may ask, so this is the only tool
/// of the three that usually reports its state without privilege. It exits 252 when it is not
/// running, which is a successful reading of an inactive firewall and not a failure.
pub fn read_firewalld(exit: &Exit) -> FirewallState {
    match exit {
        Exit::Missing => absent(),
        Exit::Unstartable(why) => unknown(
            "firewalld",
            format!("firewall-cmd is on this machine but would not start: {why}"),
        ),
        Exit::Ran { stdout, stderr, .. } => {
            let word = first_line(stdout)
                .or_else(|| first_line(stderr))
                .unwrap_or_default()
                .to_lowercase();
            if word.contains("not running") {
                FirewallState {
                    kind: Some("firewalld".to_string()),
                    state: FirewallStatus::Inactive,
                    rule_count: None,
                    rules: Vec::new(),
                    reason: Some(
                        "firewalld is installed and its daemon is not running, so there is no \
                         ruleset to count"
                            .to_string(),
                    ),
                }
            } else if word.contains("running") {
                FirewallState {
                    kind: Some("firewalld".to_string()),
                    state: FirewallStatus::Active,
                    // `--state` says running and nothing about rules. `--list-all` would, and it
                    // is a second call for a number nobody on this screen acts on; `None` is the
                    // honest value and the screen renders it as unknown rather than as 0.
                    rule_count: None,
                    rules: Vec::new(),
                    reason: Some(
                        "firewalld reports itself running; its rule count was not read".to_string(),
                    ),
                }
            } else {
                let said = said_by(stderr, stdout, "firewall-cmd would not say");
                if refused_for_privilege(&said) {
                    unknown(
                        "firewalld",
                        format!("firewall-cmd refused this session: {said}"),
                    )
                } else {
                    unknown("firewalld", said)
                }
            }
        }
    }
}

/// No firewall tool on this machine. Not "off".
fn absent() -> FirewallState {
    FirewallState {
        kind: None,
        state: FirewallStatus::Absent,
        rule_count: None,
        rules: Vec::new(),
        reason: Some(format!(
            "no firewall tool is installed on this machine (looked for {})",
            CANDIDATES
                .iter()
                .map(|(_, bin)| *bin)
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// It could not be determined, and here is why. There is never an [`FirewallStatus::Unknown`]
/// without one: an unknown with no reason is the same thing as the confident "Off" this replaces.
fn unknown(kind: &str, reason: impl Into<String>) -> FirewallState {
    FirewallState {
        kind: Some(kind.to_string()),
        state: FirewallStatus::Unknown,
        rule_count: None,
        rules: Vec::new(),
        reason: Some(reason.into()),
    }
}

fn said_by(stderr: &str, stdout: &str, fallback: &str) -> String {
    first_line(stderr)
        .or_else(|| first_line(stdout))
        .unwrap_or_else(|| fallback.to_string())
}

// ══════════════════════════════════════════════════════════════════════
// The listings
// ══════════════════════════════════════════════════════════════════════

/// Every rule in an `nft list ruleset` dump, with the chain it sits in.
///
/// Counted by tracking brace depth rather than by matching keywords: the companion's own
/// `firewall_status` counts lines starting with one of six words (`meta`, `tcp`, `udp`, `ip`,
/// `ct`, `iif`) and calls the result "~N rules", which misses `oifname`, `icmp`, `jump`, `log`
/// and any rule written with a counter first. A rule is a line inside a chain block that is not
/// the chain's own `type … hook …` declaration and not a brace.
pub fn parse_nft_ruleset(text: &str) -> Vec<FirewallRuleInfo> {
    let mut rules = Vec::new();
    let mut chain = String::new();
    let mut depth: usize = 0;

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with("chain ") {
            // `chain input {`
            chain = line
                .trim_start_matches("chain ")
                .trim_end_matches('{')
                .trim()
                .to_string();
        }
        let opens = line.matches('{').count();
        let closes = line.matches('}').count();

        // A rule sits inside table > chain, so depth is 2 before the line is read.
        let is_rule = depth >= 2
            && opens == 0
            && closes == 0
            && !line.starts_with("type ")
            && !line.starts_with("policy ")
            && !line.starts_with("comment ");
        if is_rule {
            rules.push(FirewallRuleInfo {
                chain: chain.clone(),
                action: verdict(line),
                text: line.to_string(),
            });
        }

        depth = depth.saturating_add(opens).saturating_sub(closes);
    }
    rules
}

/// Whether `ufw status` says active. `None` when the output was not a status at all.
pub fn parse_ufw_status(text: &str) -> Option<bool> {
    for line in text.lines() {
        let low = line.trim().to_lowercase();
        if let Some(rest) = low.strip_prefix("status:") {
            return Some(rest.trim().starts_with("active"));
        }
    }
    None
}

/// The rule rows out of `ufw status`, which are the lines after the `--` underline.
pub fn parse_ufw_rules(text: &str) -> Vec<FirewallRuleInfo> {
    let mut rules = Vec::new();
    let mut in_table = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with("--") {
            in_table = true;
            continue;
        }
        if !in_table || line.is_empty() {
            continue;
        }
        if line.to_lowercase().starts_with("to ") && line.to_lowercase().contains("action") {
            continue;
        }
        rules.push(FirewallRuleInfo {
            chain: "ufw".to_string(),
            action: verdict(line),
            text: line.to_string(),
        });
    }
    rules
}

/// The verdict word in a rule line, in the tool's own vocabulary.
///
/// `unknown` rather than a guess when the line carries none: a rule with a `jump` or a `goto`
/// has its verdict somewhere else entirely, and labelling it ACCEPT would be a fabrication in a
/// security display.
fn verdict(line: &str) -> String {
    let low = line.to_lowercase();
    for word in ["accept", "allow", "drop", "deny", "reject", "log", "return", "jump", "goto"] {
        if low.split_whitespace().any(|t| t.trim_matches(',') == word) {
            return word.to_string();
        }
    }
    "unknown".to_string()
}
