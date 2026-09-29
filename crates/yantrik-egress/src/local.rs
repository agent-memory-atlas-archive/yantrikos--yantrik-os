//! This machine's own addresses — never a destination.
//!
//! Loopback is refused by its range, but a service bound to 0.0.0.0 also answers on every other
//! address the machine has (`192.168.4.44`, a docker bridge), and traffic to those stays on this
//! machine. Asked of the kernel each time (`getifaddrs`), so an address that came with a new
//! network is refused the moment it exists.

use std::net::IpAddr;

/// Every address on every interface, now.
#[cfg(unix)]
pub fn addresses() -> Vec<IpAddr> {
    let mut out = Vec::new();
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills a list we free below with freeifaddrs; each node is read only
    // while the list is alive, and each address only for the family its header says it is.
    unsafe {
        if libc::getifaddrs(&mut head) != 0 {
            return out;
        }
        let mut cur = head;
        while !cur.is_null() {
            let addr = (*cur).ifa_addr;
            if !addr.is_null() {
                match (*addr).sa_family as libc::c_int {
                    libc::AF_INET => {
                        let sin = &*(addr as *const libc::sockaddr_in);
                        out.push(IpAddr::from(u32::from_be(sin.sin_addr.s_addr).to_be_bytes()));
                    }
                    libc::AF_INET6 => {
                        let sin6 = &*(addr as *const libc::sockaddr_in6);
                        out.push(IpAddr::from(sin6.sin6_addr.s6_addr));
                    }
                    _ => {}
                }
            }
            cur = (*cur).ifa_next;
        }
        libc::freeifaddrs(head);
    }
    out
}

#[cfg(not(unix))]
pub fn addresses() -> Vec<IpAddr> {
    Vec::new()
}

/// Whether `ip` is one of this machine's, or an IPv4 one written as IPv6.
pub fn is_own(ip: IpAddr, own: &[IpAddr]) -> bool {
    let plain = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        v4 => v4,
    };
    own.contains(&plain)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn this_machine_has_its_loopback_and_it_is_its_own() {
        let own = addresses();
        assert!(own.contains(&"127.0.0.1".parse().unwrap()), "{own:?}");
        assert!(is_own("::ffff:127.0.0.1".parse().unwrap(), &own));
        assert!(!is_own("192.0.2.1".parse().unwrap(), &own), "a documentation address is nobody's");
    }
}
