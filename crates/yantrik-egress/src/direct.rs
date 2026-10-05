//! The direct set: where the mind account may connect without this proxy.
//!
//! The kernel holds the mind account to its way out (`yantrik-update mind-egress apply`, the
//! table `inet yantrik_mind_egress`): loopback, where this proxy listens; DNS to the machine's
//! resolvers; and this set. Everything else from the account is refused, whatever the program
//! does with its proxy variables.
//!
//! The set comes only from the person's policy, which only root and the desktop's owner can
//! change (the control socket), never from anything the mind account can write. An entry is a
//! rule that says `lan` and whose host is a literal address on the local network, one entry per
//! port: a LAN service the person allowed by address, which a program that ignores proxy variables
//! may then reach. A name never makes an entry; it keeps going through this proxy, which decides
//! on the address it resolves to. Neither does a public address: the internet is reached through
//! the proxy, where it is counted. In Private mode the set is empty.
//!
//! `yantrik-egress direct [STATE_DIR]` prints it, run by the updater as this proxy's own account
//! (so the policy is read by the code that enforces it, never parsed as root):
//!
//! ```text
//! private off
//! direct 192.168.4.42 8888
//! direct fd00::5 443
//! ```
//!
//! A policy that does not read is an error (exit 1), and the updater loads an empty set for it.
//! The updater checks every line again as root, and caps the set.

use std::net::IpAddr;
use std::path::Path;

use crate::policy::{place_of, Place, Policy};

/// The address a rule names, if its host is a literal one that may be in the direct set: an IPv4
/// address on the local network (10/8, 172.16/12, 192.168/16, 100.64/10), or a unique-local IPv6
/// one (fc00::/7). An IPv4 address written as IPv6 (`::ffff:a.b.c.d`) is the IPv4 one, which is
/// what the kernel sees on the wire.
fn direct_address(host: &str) -> Option<IpAddr> {
    let ip = match host.parse::<IpAddr>().ok()? {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    };
    let ok = match ip {
        IpAddr::V4(_) => place_of(ip) == Place::Lan,
        // Only fc00::/7 itself: an address that is "local" only because of the IPv4 address
        // carried inside it (NAT64, 6to4) leaves this machine as a packet to a router.
        IpAddr::V6(v6) => (v6.segments()[0] & 0xfe00) == 0xfc00,
    };
    ok.then_some(ip)
}

/// Every (address, port) the policy lets the mind reach directly, sorted, each once.
pub fn entries(policy: &Policy) -> Vec<(IpAddr, u16)> {
    let mut out: Vec<(IpAddr, u16)> = policy
        .rules
        .iter()
        .filter(|r| r.lan)
        .filter_map(|r| direct_address(&r.host).map(|ip| (ip, r)))
        .flat_map(|(ip, r)| r.ports.iter().filter(|p| **p != 0).map(move |p| (ip, *p)))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// What `yantrik-egress direct` prints for the state in `dir`.
pub fn export(dir: &Path) -> Result<String, String> {
    let policy = Policy::read(&dir.join("policy.yaml"))?;
    if crate::state::private_at(dir) {
        return Ok("private on\n".into());
    }
    let mut out = String::from("private off\n");
    for (ip, port) in entries(&policy) {
        out.push_str(&format!("direct {ip} {port}\n"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{Mode, Rule};

    fn rule(host: &str, ports: &[u16], lan: bool) -> Rule {
        Rule { host: host.into(), ports: ports.to_vec(), http: true, lan, why: "because".into() }
    }

    #[test]
    fn only_lan_rules_naming_a_local_address_make_entries() {
        let p = Policy {
            mode: Mode::Audit,
            rules: vec![
                rule("192.168.4.42", &[8888, 8080], true),
                rule("fd00::5", &[443], true),
                rule("::ffff:10.0.0.7", &[11434], true),
                // A name: through the proxy, which classifies what it resolves to.
                rule("searx.lan", &[8888], true),
                rule("*.home.arpa", &[443], true),
                // Not `lan`: the person did not say the local network.
                rule("192.168.4.43", &[22], false),
                // Public, loopback, link-local, NAT64: never direct.
                rule("1.1.1.1", &[443], true),
                rule("127.0.0.1", &[7440], true),
                rule("169.254.169.254", &[80], true),
                rule("64:ff9b::c0a8:414", &[80], true),
                rule("2606:4700::1111", &[443], true),
                // The same entry twice is one entry.
                rule("192.168.4.42", &[8888], true),
            ],
        };
        let got: Vec<String> = entries(&p).iter().map(|(ip, port)| format!("{ip} {port}")).collect();
        assert_eq!(got, ["10.0.0.7 11434", "192.168.4.42 8080", "192.168.4.42 8888", "fd00::5 443"]);
    }

    #[test]
    fn the_export_says_private_mode_and_refuses_a_policy_that_does_not_read() {
        let d = std::env::temp_dir().join(format!("yantrik-egress-direct-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        assert_eq!(export(&d).unwrap(), "private off\n", "no policy: nothing direct");

        std::fs::write(
            d.join("policy.yaml"),
            "mode: enforce\nrules:\n  - host: 192.168.4.42\n    ports: [8888]\n    http: true\n    lan: true\n    why: SearXNG\n  - host: api.x.ai\n    ports: [443]\n    why: the model\n",
        )
        .unwrap();
        assert_eq!(export(&d).unwrap(), "private off\ndirect 192.168.4.42 8888\n");

        std::fs::write(d.join("private"), "on\n").unwrap();
        assert_eq!(export(&d).unwrap(), "private on\n", "Private mode: nothing direct");
        std::fs::remove_file(d.join("private")).unwrap();

        std::fs::write(d.join("policy.yaml"), "mode: enforce\nrules:\n  - host: 192.168.4.42\n    ports: [0]\n    lan: true\n    why: x\n").unwrap();
        assert!(export(&d).is_err(), "a rule the person could not have written: no set at all");
        std::fs::write(d.join("policy.yaml"), "mode: enforce\nrules: [\n").unwrap();
        assert!(export(&d).is_err(), "not a policy");
        std::fs::write(d.join("policy.yaml"), b"mode: audit\n\xff\n").unwrap();
        assert!(export(&d).is_err(), "not text");
        let _ = std::fs::remove_dir_all(&d);
    }
}
