//! The private and special-use ranges: addresses that are not the internet.
//!
//! One list with a twin: `private_ranges.json` beside this crate is a copy of yantrik-mind's
//! `deploy/private_ranges.json`, which the Mind's own fetch guard reads. The proxy refuses every
//! address in it, in every mode, unless a person's rule that says `lan` covers it
//! (`crate::policy`), so the Mind can leave classifying destinations to the proxy when it is
//! behind one. Change both files or neither.

use std::net::IpAddr;
use std::sync::OnceLock;

#[derive(serde::Deserialize)]
struct List {
    v4: Vec<String>,
    v6: Vec<String>,
}

/// (network, prefix length) pairs, as u128 with IPv4 in the low 32 bits.
struct Ranges {
    v4: Vec<(u32, u32)>,
    v6: Vec<(u128, u32)>,
}

fn parse<T: std::str::FromStr>(cidr: &str) -> (T, u32) {
    let (net, len) = cidr.split_once('/').expect("a range is network/length");
    (net.parse().ok().expect("a range's network is an address"), len.parse().expect("a range's length is a number"))
}

fn ranges() -> &'static Ranges {
    static R: OnceLock<Ranges> = OnceLock::new();
    R.get_or_init(|| {
        let list: List = serde_json::from_str(include_str!("../private_ranges.json")).expect("private_ranges.json parses");
        Ranges {
            v4: list.v4.iter().map(|c| parse::<std::net::Ipv4Addr>(c)).map(|(n, l)| (u32::from(n), l)).collect(),
            v6: list.v6.iter().map(|c| parse::<std::net::Ipv6Addr>(c)).map(|(n, l)| (u128::from(n), l)).collect(),
        }
    })
}

/// Whether `ip` is in one of the ranges. An IPv4 address written as IPv6 (`::ffff:a.b.c.d`) is
/// judged by the IPv4 one.
pub fn special(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let a = u32::from(v4);
            ranges().v4.iter().any(|&(n, l)| l == 0 || (a ^ n) >> (32 - l) == 0)
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return special(IpAddr::V4(v4));
            }
            let a = u128::from(v6);
            ranges().v6.iter().any(|&(n, l)| l == 0 || (a ^ n) >> (128 - l) == 0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_reads_and_holds_what_is_not_the_internet() {
        for ip in [
            "10.1.2.3", "100.64.0.1", "127.0.0.1", "169.254.169.254", "172.31.255.255", "192.168.4.42", "198.18.0.1",
            "203.0.113.9", "224.0.0.251", "240.0.0.1", "0.1.2.3", "::1", "fd00::5", "fe80::1", "ff02::fb",
            "2001:0:4136:e378::1", "2002:c0a8:42::1", "64:ff9b::808:808", "2001:db8::1", "::ffff:192.168.4.42",
        ] {
            assert!(special(ip.parse().unwrap()), "{ip} is not the internet");
        }
        for ip in ["1.1.1.1", "8.8.8.8", "172.32.0.1", "100.128.0.1", "2606:4700::1111", "2a00:1450::1", "::ffff:8.8.8.8"] {
            assert!(!special(ip.parse().unwrap()), "{ip} is the internet");
        }
    }
}
