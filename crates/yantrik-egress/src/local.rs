//! This machine's own addresses — never a destination — and the network it is on.
//!
//! Loopback is refused by its range, but a service bound to 0.0.0.0 also answers on every other
//! address the machine has (`192.168.4.44`, a docker bridge), and traffic to those stays on this
//! machine. Asked of the kernel each time (`getifaddrs`), so an address that came with a new
//! network is refused the moment it exists.
//!
//! The home network is not only the private ranges. A dual-stack home gives every device a global
//! IPv6 address from the ISP's prefix (2a02:8070:abcd:1::20 is the NAS), and some homes and most
//! servers sit on a public IPv4 subnet. So every prefix an interface of this machine is on, and
//! every router it sends through, is the local network too ([`place`]), whatever range it is in.
//! Read with the addresses, on every connection: a network change is seen by the next one.
//!
//! Known limit: the router's public WAN address (a request to it hairpins to its admin page)
//! cannot be known here without asking something outside, and nothing is asked; it is the
//! internet to this proxy.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use crate::policy::{place_of, Place};

/// What the kernel says about this machine's network, now.
#[derive(Clone, Debug, Default)]
pub struct Net {
    /// Every address on every interface.
    pub own: Vec<IpAddr>,
    /// Every prefix an interface is on, as its address and prefix length.
    pub links: Vec<(IpAddr, u8)>,
    /// Every router a route sends through (`/proc/self/net/route` and `ipv6_route`).
    pub gateways: Vec<IpAddr>,
}

/// This machine's addresses, the prefixes they are on and its routers, now — or `None` when the
/// kernel would not say (found on VM 520: the unit's `RestrictAddressFamilies` had left out the
/// netlink socket `getifaddrs` asks through, the list came back empty, and this machine's own LAN
/// address was let through).
#[cfg(unix)]
pub fn addresses() -> Option<Net> {
    let mut net = Net::default();
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills a list we free below with freeifaddrs; each node is read only
    // while the list is alive, and each address (and netmask) only for the family its header says
    // it is.
    unsafe {
        if libc::getifaddrs(&mut head) != 0 {
            return None;
        }
        let read = |sa: *const libc::sockaddr| -> Option<IpAddr> {
            if sa.is_null() {
                return None;
            }
            match (*sa).sa_family as libc::c_int {
                libc::AF_INET => Some(IpAddr::from(u32::from_be((*(sa as *const libc::sockaddr_in)).sin_addr.s_addr).to_be_bytes())),
                libc::AF_INET6 => Some(IpAddr::from((*(sa as *const libc::sockaddr_in6)).sin6_addr.s6_addr)),
                _ => None,
            }
        };
        let mut cur = head;
        while !cur.is_null() {
            if let Some(addr) = read((*cur).ifa_addr) {
                net.own.push(addr);
                if let Some(len) = read((*cur).ifa_netmask).and_then(|m| prefix_len(addr, m)) {
                    net.links.push((addr, len));
                }
            }
            cur = (*cur).ifa_next;
        }
        libc::freeifaddrs(head);
    }
    // A machine always has loopback; a list without it is a list the kernel did not give.
    if !net.own.iter().any(|a| a.is_loopback()) {
        return None;
    }
    // Through `self`: the unit's `ProcSubset=pid` hides `/proc/net`, which is only a link to it.
    // A table that cannot be read gives no routers; the prefixes above still cover the usual one.
    let v4 = std::fs::read_to_string("/proc/self/net/route").unwrap_or_default();
    let v6 = std::fs::read_to_string("/proc/self/net/ipv6_route").unwrap_or_default();
    net.gateways = gateways(&v4, &v6);
    Some(net)
}

#[cfg(not(unix))]
pub fn addresses() -> Option<Net> {
    None
}

/// The length of `mask` when it is a netmask of `addr`'s family (ones, then zeros); none for /0,
/// which is no link.
fn prefix_len(addr: IpAddr, mask: IpAddr) -> Option<u8> {
    let (bits, width) = match (addr, mask) {
        (IpAddr::V4(_), IpAddr::V4(m)) => (u128::from(u32::from(m)) << 96, 32),
        (IpAddr::V6(_), IpAddr::V6(m)) => (u128::from(m), 128),
        _ => return None,
    };
    let len = bits.leading_ones();
    (len > 0 && len <= width && bits.checked_shl(len).unwrap_or(0) == 0).then_some(len as u8)
}

/// The routers in the kernel's route tables, `/proc/net/route`- and `ipv6_route`-shaped.
pub fn gateways(v4: &str, v6: &str) -> Vec<IpAddr> {
    const RTF_GATEWAY: u32 = 0x2;
    let mut out = Vec::new();
    // Iface Destination Gateway Flags …, each address one 32-bit word in host byte order.
    for line in v4.lines().skip(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        let (Some(gw), Some(flags)) = (cols.get(2), cols.get(3)) else { continue };
        let (Ok(gw), Ok(flags)) = (u32::from_str_radix(gw, 16), u32::from_str_radix(flags, 16)) else { continue };
        if flags & RTF_GATEWAY != 0 && gw != 0 {
            out.push(IpAddr::V4(Ipv4Addr::from(gw.to_ne_bytes())));
        }
    }
    // dest plen src plen nexthop metric refcnt use flags iface, addresses as 32 hex digits.
    for line in v6.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        let (Some(hop), Some(flags)) = (cols.get(4), cols.get(8)) else { continue };
        let (Ok(hop), Ok(flags)) = (u128::from_str_radix(hop, 16), u32::from_str_radix(flags, 16)) else { continue };
        if flags & RTF_GATEWAY != 0 && hop != 0 && cols[4].len() == 32 {
            out.push(IpAddr::V6(Ipv6Addr::from(hop)));
        }
    }
    out.sort();
    out.dedup();
    out
}

impl Net {
    /// Whether `ip` — or the IPv4 address it carries (mapped, NAT64, 6to4) — is on a prefix this
    /// machine is on, or is one of its routers.
    pub fn on_link(&self, ip: IpAddr) -> bool {
        let mut forms = vec![ip];
        if let IpAddr::V6(v6) = ip {
            let (seg, b) = (v6.segments(), v6.octets());
            if let Some(v4) = v6.to_ipv4_mapped() {
                forms.push(IpAddr::V4(v4));
            } else if seg[..6] == [0x64, 0xff9b, 0, 0, 0, 0] || seg[..3] == [0x64, 0xff9b, 1] {
                forms.push(IpAddr::V4(Ipv4Addr::new(b[12], b[13], b[14], b[15])));
            } else if seg[0] == 0x2002 {
                forms.push(IpAddr::V4(Ipv4Addr::new(b[2], b[3], b[4], b[5])));
            }
        }
        forms.iter().any(|&a| self.gateways.contains(&a) || self.links.iter().any(|&(net, len)| within(a, net, len)))
    }
}

fn within(ip: IpAddr, net: IpAddr, len: u8) -> bool {
    match (ip, net) {
        (IpAddr::V4(a), IpAddr::V4(n)) if len <= 32 => {
            let mask = u32::MAX.checked_shl(32 - u32::from(len)).unwrap_or(0);
            u32::from(a) & mask == u32::from(n) & mask
        }
        (IpAddr::V6(a), IpAddr::V6(n)) if len <= 128 => {
            let mask = u128::MAX.checked_shl(128 - u32::from(len)).unwrap_or(0);
            u128::from(a) & mask == u128::from(n) & mask
        }
        _ => false,
    }
}

/// Whether `ip` is one of this machine's, or an IPv4 one written as IPv6. When the machine's own
/// addresses are not known, every address on the local network might be one of them, and is
/// taken to be: refused, not let through.
pub fn is_own(ip: IpAddr, own: Option<&[IpAddr]>) -> bool {
    let plain = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        v4 => v4,
    };
    match own {
        Some(own) => own.contains(&plain),
        None => place_of(plain) == Place::Lan,
    }
}

/// Where `ip` leads from this machine, on `net`: never a destination if it is this machine's
/// own; the local network if it is on one of this machine's prefixes or is one of its routers,
/// in whatever range; otherwise its range says ([`place_of`]).
pub fn place(ip: IpAddr, net: Option<&Net>) -> Place {
    if is_own(ip, net.map(|n| n.own.as_slice())) {
        return Place::Forbidden;
    }
    match place_of(ip) {
        Place::Internet if net.is_some_and(|n| n.on_link(ip)) => Place::Lan,
        p => p,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn this_machine_has_its_loopback_and_it_is_its_own() {
        let own = addresses().expect("the kernel lists this machine's addresses").own;
        assert!(own.contains(&"127.0.0.1".parse().unwrap()), "{own:?}");
        assert!(is_own("::ffff:127.0.0.1".parse().unwrap(), Some(&own)));
        assert!(!is_own("192.0.2.1".parse().unwrap(), Some(&own)), "a documentation address is nobody's");
    }

    /// Not knowing this machine's addresses refuses the local network rather than letting it all
    /// through: any address on it might be this machine's.
    #[test]
    fn unknown_own_addresses_fail_closed_for_the_local_network() {
        assert!(is_own("192.168.4.44".parse().unwrap(), None));
        assert!(is_own("10.0.0.5".parse().unwrap(), None));
        assert!(!is_own("93.184.215.14".parse().unwrap(), None), "the internet is not this machine");
    }

    #[test]
    fn a_netmask_is_its_prefix_length() {
        let len = |a: &str, m: &str| prefix_len(a.parse().unwrap(), m.parse().unwrap());
        assert_eq!(len("192.168.4.44", "255.255.255.0"), Some(24));
        assert_eq!(len("81.2.69.165", "255.255.255.240"), Some(28));
        assert_eq!(len("2a02:8070:abcd:1::5", "ffff:ffff:ffff:ffff::"), Some(64));
        assert_eq!(len("10.0.0.1", "0.0.0.0"), None, "/0 is no link");
        assert_eq!(len("10.0.0.1", "255.0.255.0"), None, "not a netmask");
        assert_eq!(len("10.0.0.1", "ffff::"), None, "another family");
    }

    /// Written the way the kernel writes them on a little-endian machine.
    #[cfg(target_endian = "little")]
    #[test]
    fn the_routers_are_read_from_the_route_tables() {
        let v4 = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
                  eth0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0\n\
                  eth0\t0004A8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0\n\
                  ppp0\t00000000\t01450251\t0003\t0\t0\t200\t00000000\t0\t0\t0\n";
        let v6 = "00000000000000000000000000000000 00 00000000000000000000000000000000 00 fe80000000000000021122fffe334455 00000400 00000001 00000000 00000003 eth0\n\
                  00000000000000000000000000000000 00 00000000000000000000000000000000 00 2a028070abcd00000000000000000001 00000400 00000001 00000000 00000003 eth0\n\
                  2a028070abcd00010000000000000000 40 00000000000000000000000000000000 00 00000000000000000000000000000000 00000100 00000001 00000000 00000001 eth0\n\
                  00000000000000000000000000000000 00 00000000000000000000000000000000 00 00000000000000000000000000000000 ffffffff 00000001 00000000 00200200 lo\n";
        let want: Vec<IpAddr> =
            ["81.2.69.1", "192.168.1.1", "2a02:8070:abcd::1", "fe80::211:22ff:fe33:4455"].iter().map(|a| a.parse().unwrap()).collect();
        let mut got = gateways(v4, v6);
        got.sort();
        let mut want = want;
        want.sort();
        assert_eq!(got, want);
    }

    /// A home device on the ISP's global prefix, on a public IPv4 subnet, or the router, is the
    /// local network, in any form it is written in; the rest of the internet is not.
    #[test]
    fn the_prefixes_this_machine_is_on_and_its_routers_are_the_local_network() {
        let net = Net {
            own: vec!["127.0.0.1".parse().unwrap(), "2a02:8070:abcd:1::5".parse().unwrap()],
            links: vec![("2a02:8070:abcd:1::5".parse().unwrap(), 64), ("81.2.69.165".parse().unwrap(), 28)],
            gateways: vec!["81.2.69.1".parse().unwrap(), "2a02:8070:abcd::1".parse().unwrap()],
        };
        for ip in [
            "2a02:8070:abcd:1::20", "2a02:8070:abcd:1:ffff::1", "81.2.69.170", "::ffff:81.2.69.170", "64:ff9b::5102:45aa",
            "2002:5102:45aa::1", "81.2.69.1", "2a02:8070:abcd::1",
        ] {
            assert_eq!(place(ip.parse().unwrap(), Some(&net)), Place::Lan, "{ip}");
        }
        for ip in ["2a02:8070:abcd:2::20", "81.2.69.176", "81.2.69.2", "1.1.1.1", "2606:4700::1111"] {
            assert_eq!(place(ip.parse().unwrap(), Some(&net)), Place::Internet, "{ip}");
        }
        assert_eq!(place("2a02:8070:abcd:1::5".parse().unwrap(), Some(&net)), Place::Forbidden, "this machine");
        assert_eq!(place("192.168.4.20".parse().unwrap(), Some(&net)), Place::Lan, "the private ranges as before");
    }
}
