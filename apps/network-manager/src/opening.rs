//! Which section the window opens on.
//!
//! It opened on Wi-Fi whatever the machine was, so a desktop with no Wi-Fi adapter opened on
//! "No adapter" while its traffic went out over a cable. It now opens on the interface that
//! carries traffic: the one the kernel's default route leaves by.
//!
//! Pure: the route table arrives as text, so the tests need no kernel and no window.

/// Where the kernel lists its IPv4 routes.
pub const ROUTE_TABLE: &str = "/proc/net/route";

/// The window's sections, as `active-tab` numbers them.
pub const WIFI: i32 = 0;
pub const ETHERNET: i32 = 1;

/// `RTF_UP`: a route the kernel will actually use.
const ROUTE_UP: u32 = 0x1;

/// The interface the default route leaves by, read from `/proc/net/route`.
///
/// A default route is destination 0.0.0.0 with mask 0.0.0.0. With more than one (a cable and
/// a radio both up), the kernel prefers the lowest metric, and so does this.
pub fn default_route_interface(table: &str) -> Option<String> {
    table
        .lines()
        // The header row: "Iface Destination Gateway Flags RefCnt Use Metric Mask MTU …".
        .skip(1)
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 8 || f[1] != "00000000" || f[7] != "00000000" {
                return None;
            }
            let flags = u32::from_str_radix(f[3], 16).ok()?;
            if flags & ROUTE_UP == 0 {
                return None;
            }
            Some((f[6].parse::<u32>().unwrap_or(u32::MAX), f[0].to_string()))
        })
        .min_by_key(|(metric, _)| *metric)
        .map(|(_, iface)| iface)
}

/// The section to open on.
///
/// Ethernet when the default route leaves by a wired interface. Ethernet too when there is no
/// Wi-Fi adapter and there is a wired interface, whatever the route table said: "No adapter"
/// is the one thing that section can tell this machine, and the rail already says it. Wi-Fi
/// otherwise, which is where the traffic goes or where a person comes to connect.
pub fn opening_tab(route_iface: Option<&str>, ethernet: &[String], wifi_adapter_present: bool) -> i32 {
    let wired_route = route_iface.is_some_and(|iface| ethernet.iter().any(|e| e == iface));
    if wired_route || (!wifi_adapter_present && !ethernet.is_empty()) {
        ETHERNET
    } else {
        WIFI
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str =
        "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT";

    fn table(rows: &[&str]) -> String {
        std::iter::once(HEADER).chain(rows.iter().copied()).collect::<Vec<_>>().join("\n")
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_default_route_is_the_all_zero_destination_and_mask() {
        let t = table(&[
            "eth0\t0000A8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0",
            "eth0\t00000000\t0104A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0",
        ]);
        assert_eq!(default_route_interface(&t).as_deref(), Some("eth0"));
    }

    #[test]
    fn with_two_default_routes_the_lower_metric_carries_the_traffic() {
        let t = table(&[
            "wlan0\t00000000\t0104A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0",
            "enp3s0\t00000000\t0104A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0",
        ]);
        assert_eq!(default_route_interface(&t).as_deref(), Some("enp3s0"));
    }

    #[test]
    fn a_route_that_is_not_up_and_a_table_with_no_default_name_nothing() {
        let down = table(&["eth0\t00000000\t0104A8C0\t0002\t0\t0\t100\t00000000\t0\t0\t0"]);
        assert_eq!(default_route_interface(&down), None);
        let local = table(&["eth0\t0000A8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0"]);
        assert_eq!(default_route_interface(&local), None);
        assert_eq!(default_route_interface(""), None);
    }

    /// The reported case: no Wi-Fi adapter, a cable carrying the traffic.
    #[test]
    fn a_wired_machine_with_no_radio_opens_on_ethernet() {
        let eth = names(&["eth0"]);
        assert_eq!(opening_tab(Some("eth0"), &eth, false), ETHERNET);
        // Even when the route table could not be read.
        assert_eq!(opening_tab(None, &eth, false), ETHERNET);
    }

    #[test]
    fn a_machine_on_its_radio_opens_on_wifi() {
        let eth = names(&["eth0"]);
        assert_eq!(opening_tab(Some("wlan0"), &eth, true), WIFI);
        assert_eq!(opening_tab(Some("eth0"), &eth, true), ETHERNET);
        // Nothing wired and nothing routed: Wi-Fi is where a person comes to connect.
        assert_eq!(opening_tab(None, &[], true), WIFI);
    }
}
