//! Network service — interfaces, connectivity and resolvers from `/proc`, Wi-Fi and the firewall
//! from the tools this OS ships.
//!
//! Methods, all of them named by `yantrik_ipc_contracts::network::method` rather than spelled out
//! here, because a list in a doc comment is exactly what the app's five calls disagreed with:
//!
//! ```text
//!   network.interfaces     {}                        -> Vec<NetworkInterfaceInfo>
//!   network.status         {}                        -> NetworkStatus
//!   network.dns            {}                        -> DnsConfig
//!   network.wifi_state     {}                        -> WifiState
//!   network.wifi_known     {}                        -> Vec<KnownNetwork>
//!   network.firewall       {}                        -> FirewallState
//!   network.dns_set        { servers }               -> DnsSetResult   (re-read)
//!   network.wifi_radio     { enabled }               -> WifiState      (re-read)
//!   network.wifi_scan      { rescan }                -> Vec<ScannedNetwork>
//!   network.wifi_connect   { ssid, password? }       -> WifiState      (re-read)
//!   network.wifi_disconnect{}                        -> WifiState      (re-read)
//!   network.wifi_forget    { ssid }                  -> WifiForgetResult (re-read)
//! ```
//!
//! # The half that was never written
//!
//! `apps/network-manager` called `network.wifi_toggle`, `network.wifi_scan`,
//! `network.wifi_connect`, `network.wifi_disconnect` and `network.wifi_forget`. This service
//! implemented `network.interfaces`, `network.status` and `network.dns` and answered everything
//! else `Unknown method`. Not one name in common. The read half was repaired in an earlier pass —
//! the window used to show "Not connected" on a machine with a routable address because nothing
//! ever asked — and the write half was left exactly as it was, five verbs into a wall.
//!
//! The six write and read methods below are that half. Every one of them re-reads the machine
//! after it acts and answers with what it then saw, and returns nmcli's own first line of stderr
//! when it failed. An action that cannot be verified is an error, not a success: `wifi_radio`
//! with the radio still off afterwards fails, rather than reporting the request back as a result.
//!
//! # No secret reaches a log line
//!
//! The one method that takes a password hands it to nmcli on stdin (see `nmcli::run_with_secret`)
//! and never puts it in `argv`, in an error, or in a trace. `WifiConnectParams` has a hand-written
//! `Debug` so that a `{:?}` cannot leak it either.

mod firewall;
mod nmcli;

use yantrik_ipc_contracts::control_surface::{describe_json, Action, View};
use yantrik_ipc_contracts::network::{
    method, ConnectionType, DnsConfig, DnsSetParams, DnsSetResult, FirewallState, KnownNetwork,
    NetworkInterfaceInfo, NetworkStatus, RadioState, ScannedNetwork, WifiConnectParams,
    WifiForgetParams, WifiForgetResult, WifiRadioParams, WifiScanParams, WifiState,
};
use yantrik_service_sdk::prelude::*;

use nmcli::Trouble;

fn main() {
    ServiceBuilder::new("network")
        .handler(NetworkHandler)
        .run();
}

struct NetworkHandler;

/// Read the parameters of one method, naming the method when they do not fit.
///
/// The same helper calendar-service grew for the same reason: a caller that sends the wrong shape
/// hears which method rejected it, instead of the request quietly deserialising to a default.
fn params_for<T: serde::de::DeserializeOwned>(
    method_name: &str,
    params: serde_json::Value,
) -> Result<T, ServiceError> {
    serde_json::from_value(params).map_err(|e| ServiceError {
        code: -32602,
        message: format!("{method_name}: {e}"),
    })
}

impl ServiceHandler for NetworkHandler {
    fn service_id(&self) -> &str {
        "network"
    }

    fn handle(
        &self,
        method_name: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, ServiceError> {
        match method_name {
            method::INTERFACES => Ok(serde_json::to_value(read_interfaces()?).unwrap()),
            method::STATUS => Ok(serde_json::to_value(read_status()?).unwrap()),
            method::DNS => Ok(serde_json::to_value(read_dns()?).unwrap()),
            method::WIFI_STATE => Ok(serde_json::to_value(wifi_state()).unwrap()),
            method::WIFI_KNOWN => Ok(serde_json::to_value(wifi_known()?).unwrap()),
            method::FIREWALL => Ok(serde_json::to_value(firewall_state()).unwrap()),

            method::WIFI_RADIO => {
                let p: WifiRadioParams = params_for(method_name, params)?;
                Ok(serde_json::to_value(wifi_radio(p.enabled)?).unwrap())
            }
            method::WIFI_SCAN => {
                let p: WifiScanParams = params_for(method_name, params)?;
                Ok(serde_json::to_value(wifi_scan(p.rescan)?).unwrap())
            }
            method::WIFI_CONNECT => {
                let p: WifiConnectParams = params_for(method_name, params)?;
                Ok(serde_json::to_value(wifi_connect(&p)?).unwrap())
            }
            method::DNS_SET => {
                let p: DnsSetParams = params_for(method_name, params)?;
                Ok(serde_json::to_value(dns_set(&p)?).unwrap())
            }
            method::WIFI_DISCONNECT => Ok(serde_json::to_value(wifi_disconnect()?).unwrap()),
            method::WIFI_FORGET => {
                let p: WifiForgetParams = params_for(method_name, params)?;
                Ok(serde_json::to_value(wifi_forget(&p.ssid)?).unwrap())
            }

            "app.describe" => Ok(describe_json("network", &describe_view()?, &network_actions())),
            _ => Err(ServiceError {
                code: -1,
                message: format!("Unknown method: {method_name}"),
            }),
        }
    }
}

// ══════════════════════════════════════════════════════════════════════
// Failures, as codes a caller can branch on
// ══════════════════════════════════════════════════════════════════════

/// One code per distinguishable trouble, so a caller does not have to read English to tell a
/// missing adapter from a wrong password. The sentence is nmcli's wherever nmcli wrote one.
fn service_error(trouble: &Trouble) -> ServiceError {
    let code = match trouble {
        Trouble::NmcliMissing => -32020,
        Trouble::NmcliUnstartable(_) => -32021,
        Trouble::NetworkManagerDown => -32022,
        Trouble::NoWifiAdapter => -32023,
        Trouble::PermissionDenied(_) => -32024,
        Trouble::WrongPassword(_) => -32025,
        Trouble::SsidNotFound(_) => -32026,
        Trouble::TimedOut(_) => -32027,
        Trouble::Said(_) => -32028,
    };
    ServiceError {
        code,
        message: trouble.message(),
    }
}

/// A change that ran without error and did not do what it was asked.
///
/// `docker rm` exiting zero on a container that is still listed is the same shape of bug, and it
/// is the one this whole pass exists to remove: an action must not report what it asked for as
/// what happened.
fn disagreed(what: &str) -> ServiceError {
    ServiceError {
        code: -32029,
        message: what.to_string(),
    }
}

// ══════════════════════════════════════════════════════════════════════
// Wi-Fi
// ══════════════════════════════════════════════════════════════════════

/// Whether this machine has a Wi-Fi adapter, read from the kernel rather than from nmcli.
///
/// `/sys/class/net/<iface>/wireless` exists for a wireless interface and for nothing else. Asked
/// here rather than of NetworkManager because it answers on a machine where NetworkManager is not
/// running, is not installed, or has the device marked unmanaged — and "there is no adapter" and
/// "I could not ask" are the two answers that must never be confused. It is also exactly the test
/// `tests/conformance/probes/network-manager.py` uses for its own ground truth, so the app and the
/// probe are reading the same thing.
fn wifi_adapter_name() -> Option<String> {
    let entries = std::fs::read_dir("/sys/class/net").ok()?;
    let mut found: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let wireless = e.path().join("wireless");
            wireless.exists().then_some(name)
        })
        .collect();
    found.sort();
    found.into_iter().next()
}

/// The device list, or the trouble that stopped it being read.
fn devices() -> Result<Vec<nmcli::Device>, Trouble> {
    let exit = nmcli::run(
        nmcli::QUICK_WAIT_SECS,
        &["-t", "-f", nmcli::DEVICE_FIELDS, "device", "status"],
    );
    nmcli::outcome(&exit).map(|text| nmcli::parse_devices(&text))
}

/// Everything the window and `describe` say about Wi-Fi, gathered once.
///
/// Never returns an error. The absence of an adapter, an absent nmcli and a stopped
/// NetworkManager are all *states of this machine* worth reporting, and turning them into a
/// failed read would put the app back where it started: a blank pane and a default drawn as a
/// fact.
fn wifi_state() -> WifiState {
    let adapter = wifi_adapter_name();
    let Some(device_name) = adapter else {
        return WifiState {
            adapter_present: false,
            reason: Some(Trouble::NoWifiAdapter.message()),
            ..WifiState::default()
        };
    };

    let mut state = WifiState {
        adapter_present: true,
        device: Some(device_name.clone()),
        radio: RadioState::Unknown,
        ..WifiState::default()
    };

    let device_rows = match devices() {
        Ok(rows) => rows,
        Err(trouble) => {
            // The adapter is in the machine and its state could not be read. Both halves are
            // said: `adapter_present` stays true, and the radio stays `unknown` rather than
            // becoming the `false` this app used to draw.
            state.reason = Some(trouble.message());
            return state;
        }
    };

    state.radio = match nmcli::outcome(&nmcli::run(nmcli::QUICK_WAIT_SECS, &["-t", "radio", "wifi"]))
    {
        Ok(text) => match nmcli::parse_radio(&text) {
            Some(true) => RadioState::On,
            Some(false) => RadioState::Off,
            None => RadioState::Unknown,
        },
        Err(trouble) => {
            state.reason = Some(trouble.message());
            RadioState::Unknown
        }
    };

    let device = nmcli::wifi_device(&device_rows);
    // The scan list is read from NetworkManager's cache — no rescan — because this runs on every
    // three-second refresh and a rescan every three seconds would keep the radio off the air.
    let scanned = read_scan_cached(&[]).unwrap_or_default();
    state.connected_ssid = nmcli::connected_ssid(&scanned, device);
    if let Some(row) = scanned.iter().find(|n| n.is_connected) {
        state.signal = Some(row.signal);
        if !row.rate.is_empty() {
            state.rate = Some(row.rate.clone());
        }
    }

    if let Ok(text) = nmcli::outcome(&nmcli::run(
        nmcli::QUICK_WAIT_SECS,
        &["-t", "-f", nmcli::DEVICE_SHOW_FIELDS, "device", "show", device_name.as_str()],
    )) {
        let detail = nmcli::parse_device_show(&text);
        state.ip_address = detail.address;
        state.gateway = detail.gateway;
        state.subnet = detail.prefix.and_then(nmcli::prefix_to_mask);
    }

    state
}

/// The saved SSIDs, as plain strings, for marking the scan list.
fn saved_ssids() -> Vec<String> {
    wifi_known()
        .unwrap_or_default()
        .into_iter()
        .map(|k| k.ssid)
        .collect()
}

/// NetworkManager's cached access-point list, with no rescan.
///
/// `saved` is passed in rather than read here so the caller decides whether the extra
/// `nmcli connection show` is worth it. It is, for the list the window draws, which marks the
/// rows that will be joined without a password; it is not for [`wifi_state`], which runs on every
/// three-second refresh and wants only the in-use row.
fn read_scan_cached(saved: &[String]) -> Result<Vec<ScannedNetwork>, Trouble> {
    let exit = nmcli::run(
        nmcli::QUICK_WAIT_SECS,
        &["-t", "-f", nmcli::SCAN_FIELDS, "device", "wifi", "list"],
    );
    let text = nmcli::outcome(&exit)?;
    Ok(nmcli::parse_scan(&text, saved))
}

/// The SSID one Wi-Fi device is joined to, in as few calls as it takes.
///
/// [`read_status`] is on the same three-second path and needs nothing else about the radio, so it
/// asks this rather than building a whole [`WifiState`]. What it replaces returned `None`
/// unconditionally under a comment saying a future version could use nl80211 — so the header said
/// "WiFi" and never which network, on the one screen whose job is to say which network.
fn connected_ssid_for(device: &str) -> Option<String> {
    let rows = read_scan_cached(&[]).unwrap_or_default();
    if let Some(row) = rows.iter().find(|n| n.is_connected) {
        if !row.ssid.is_empty() {
            return Some(row.ssid.clone());
        }
    }
    // No in-use row in the scan cache: fall back to the profile name on the device, which is the
    // SSID for a profile NetworkManager made and is not for one somebody renamed.
    let text = nmcli::outcome(&nmcli::run(
        nmcli::QUICK_WAIT_SECS,
        &["-t", "-f", "GENERAL.CONNECTION", "device", "show", device],
    ))
    .ok()?;
    nmcli::parse_device_show(&text).connection
}

fn wifi_known() -> Result<Vec<KnownNetwork>, ServiceError> {
    let exit = nmcli::run(
        nmcli::QUICK_WAIT_SECS,
        &["-t", "-f", nmcli::CONNECTION_FIELDS, "connection", "show"],
    );
    nmcli::outcome(&exit)
        .map(|text| nmcli::parse_known(&text))
        .map_err(|t| service_error(&t))
}

/// The adapter, or a refusal naming its absence.
///
/// Every mutation starts here. A machine with no Wi-Fi hardware refuses before nmcli is run at
/// all, which is both faster and the only way to give the caller the one sentence that is
/// actually true about it.
fn require_adapter() -> Result<String, ServiceError> {
    wifi_adapter_name().ok_or_else(|| service_error(&Trouble::NoWifiAdapter))
}

fn wifi_scan(rescan: bool) -> Result<Vec<ScannedNetwork>, ServiceError> {
    require_adapter()?;
    if rescan {
        // A rescan that fails is reported and the cached list is not returned in its place: a
        // stale list presented as the result of a scan is a small version of the same lie.
        let exit = nmcli::run(nmcli::SCAN_WAIT_SECS, &["device", "wifi", "rescan"]);
        nmcli::outcome(&exit).map_err(|t| service_error(&t))?;
    }
    // The saved list is read here and not in `wifi_state`: the window's network list marks the
    // rows that need no password, and this is the one call that wants it.
    let saved = saved_ssids();
    read_scan_cached(&saved).map_err(|t| service_error(&t))
}

fn wifi_radio(enabled: bool) -> Result<WifiState, ServiceError> {
    require_adapter()?;
    let word = if enabled { "on" } else { "off" };
    let exit = nmcli::run(nmcli::QUICK_WAIT_SECS, &["radio", "wifi", word]);
    nmcli::outcome(&exit).map_err(|t| service_error(&t))?;

    // What the machine says now, not what was asked for.
    let state = wifi_state();
    let want = if enabled { RadioState::On } else { RadioState::Off };
    if state.radio != want {
        return Err(disagreed(&format!(
            "nmcli accepted `radio wifi {word}` and the radio reads {} afterwards{}",
            state.radio.as_str(),
            state
                .reason
                .as_ref()
                .map(|r| format!(": {r}"))
                .unwrap_or_default()
        )));
    }
    Ok(state)
}

/// Join a network. The password, when there is one, never touches `argv`.
fn wifi_connect(params: &WifiConnectParams) -> Result<WifiState, ServiceError> {
    require_adapter()?;
    let ssid = params.ssid.trim();
    if ssid.is_empty() {
        return Err(ServiceError {
            code: -32602,
            message: "a network name is needed to connect".to_string(),
        });
    }

    let exit = match params.password.as_deref().filter(|p| !p.is_empty()) {
        // `--ask` plus the secret on stdin. The alternative, `… password <pw>`, leaves the
        // passphrase in /proc/<pid>/cmdline for the twenty-five seconds the connect may run,
        // readable by every other user on the machine.
        Some(secret) => nmcli::run_with_secret(
            nmcli::CONNECT_WAIT_SECS,
            &["device", "wifi", "connect", ssid],
            secret,
        ),
        // No secret: an open network, or one this machine already has credentials for.
        None => nmcli::run(
            nmcli::CONNECT_WAIT_SECS,
            &["device", "wifi", "connect", ssid],
        ),
    };
    nmcli::outcome(&exit).map_err(|t| service_error(&t))?;

    let state = wifi_state();
    match state.connected_ssid.as_deref() {
        Some(joined) if joined == ssid => Ok(state),
        Some(joined) => Err(disagreed(&format!(
            "nmcli reported success for \"{ssid}\" and this machine is on \"{joined}\""
        ))),
        None => Err(disagreed(&format!(
            "nmcli reported success for \"{ssid}\" and this machine is not joined to any network"
        ))),
    }
}

fn wifi_disconnect() -> Result<WifiState, ServiceError> {
    let device = require_adapter()?;
    let before = wifi_state();
    if before.connected_ssid.is_none() {
        return Err(ServiceError {
            code: -32030,
            message: "this machine is not joined to a Wi-Fi network".to_string(),
        });
    }
    let exit = nmcli::run(
        nmcli::QUICK_WAIT_SECS,
        &["device", "disconnect", device.as_str()],
    );
    nmcli::outcome(&exit).map_err(|t| service_error(&t))?;

    let state = wifi_state();
    if let Some(still) = state.connected_ssid.as_deref() {
        return Err(disagreed(&format!(
            "nmcli accepted the disconnect and this machine is still joined to \"{still}\""
        )));
    }
    Ok(state)
}

/// Delete a saved network.
///
/// Refused for the network this machine is currently joined to. `nmcli connection delete` on the
/// active profile takes the link down as a side effect, which would make a `sensitive` action do
/// a `dangerous` thing without saying so. Disconnect first, deliberately, and then forget.
fn wifi_forget(ssid: &str) -> Result<WifiForgetResult, ServiceError> {
    require_adapter()?;
    let ssid = ssid.trim();
    let known = wifi_known()?;
    let Some(entry) = known.iter().find(|k| k.ssid == ssid) else {
        return Err(ServiceError {
            code: -32031,
            message: format!("this machine has no saved network called \"{ssid}\""),
        });
    };
    if entry.is_active {
        return Err(ServiceError {
            code: -32032,
            message: format!(
                "\"{ssid}\" is the network this machine is using; deleting it would take the \
                 connection down. Disconnect first, then forget it."
            ),
        });
    }

    let exit = nmcli::run(
        nmcli::QUICK_WAIT_SECS,
        &["connection", "delete", "uuid", entry.uuid.as_str()],
    );
    nmcli::outcome(&exit).map_err(|t| service_error(&t))?;

    let after = wifi_known()?;
    if after.iter().any(|k| k.ssid == ssid) {
        return Err(disagreed(&format!(
            "nmcli accepted the delete and \"{ssid}\" is still in the saved list"
        )));
    }
    Ok(WifiForgetResult {
        forgotten: ssid.to_string(),
        known: after,
    })
}

// ══════════════════════════════════════════════════════════════════════
// Resolvers
// ══════════════════════════════════════════════════════════════════════

/// glibc's resolver reads at most three `nameserver` lines out of `/etc/resolv.conf` — `MAXNS`
/// in `<resolv.h>`, and it has been 3 for as long as there has been a resolv.conf. A fourth
/// server accepted here would be written into the profile, appear in the readings, and never be
/// asked a question, which is a setting that looks applied and is not.
const MAX_RESOLVERS: usize = 3;

/// The profiles this machine currently has up.
fn active_connections() -> Result<Vec<nmcli::ActiveConnection>, Trouble> {
    let exit = nmcli::run(
        nmcli::QUICK_WAIT_SECS,
        &[
            "-t",
            "-f",
            nmcli::ACTIVE_FIELDS,
            "connection",
            "show",
            "--active",
        ],
    );
    nmcli::outcome(&exit).map(|text| nmcli::parse_active(&text))
}

/// The resolvers NetworkManager says it applied to one device.
fn device_dns(device: &str) -> Vec<String> {
    let exit = nmcli::run(
        nmcli::QUICK_WAIT_SECS,
        &[
            "-t",
            "-f",
            nmcli::DEVICE_DNS_FIELDS,
            "device",
            "show",
            device,
        ],
    );
    nmcli::outcome(&exit)
        .map(|text| nmcli::parse_device_dns(&text))
        .unwrap_or_default()
}

/// Which devices hold an IPv4 gateway. Asked of NetworkManager rather than of `/proc/net/route`
/// so that the answer and the profile it picks out come from the same place.
fn devices_with_gateway(active: &[nmcli::ActiveConnection]) -> Vec<String> {
    active
        .iter()
        .filter(|c| !c.device.is_empty())
        .filter(|c| {
            let exit = nmcli::run(
                nmcli::QUICK_WAIT_SECS,
                &[
                    "-t",
                    "-f",
                    nmcli::DEVICE_SHOW_FIELDS,
                    "device",
                    "show",
                    c.device.as_str(),
                ],
            );
            nmcli::outcome(&exit)
                .map(|text| nmcli::parse_device_show(&text).gateway.is_some())
                .unwrap_or(false)
        })
        .map(|c| c.device.clone())
        .collect()
}

/// Point this machine's resolvers somewhere else, through the profile that owns them.
///
/// # Why this is not a write to `/etc/resolv.conf`
///
/// The companion's `network_dns_set` did `std::fs::write("/etc/resolv.conf", …)` with a `.bak`
/// beside it and answered "DNS set to 1.1.1.1". Two things were wrong with that and both are
/// silent. The first is privilege: a desktop session cannot write that file, so the tool returned
/// its own `Try running as root` and the resolvers were unchanged — or, on the images `deploy/`
/// builds, where the session can become root for anything, it *did* write it, unscoped. The
/// second is ownership, and it survives being root: NetworkManager writes `/etc/resolv.conf`
/// itself from the active profile and rewrites it on the next carrier change, DHCP renew or
/// re-activation. So the good case was a setting with a half-life, and nothing said so.
///
/// Resolvers are a property of the connection profile. `ipv4.dns` on the profile plus
/// `ipv4.ignore-auto-dns yes` is what "use these servers, not the ones the router handed us"
/// means — without the second, NetworkManager keeps the DHCP servers in the list and the caller's
/// choice is merely first, so a resolver the caller thought it had removed still answers.
///
/// # What this costs
///
/// `nmcli connection up` re-activates the profile to apply the change. On the connection carrying
/// the default route that is a brief interruption: the link goes down and comes back with a new
/// lease. That is why the tool in front of this is graded `sensitive` rather than `standard`, and
/// why this is a bad thing to call down the connection you are calling over. `nmcli device
/// reapply` would apply the change in place and is the gentler instrument, but which properties
/// it picks up varies by NetworkManager version and nothing here has run against a live one; `up`
/// is the unambiguous one and the verification below is written against it.
fn dns_set(params: &DnsSetParams) -> Result<DnsSetResult, ServiceError> {
    use std::net::IpAddr;

    let bad = |message: String| ServiceError {
        code: -32602,
        message,
    };

    if params.servers.is_empty() {
        return Err(bad(
            "no DNS servers were given. An empty list is not read as \"clear the resolvers\": \
             leaving the connection carrying the default route with no resolver at all is not \
             something to ask for by omission"
                .to_string(),
        ));
    }
    if params.servers.len() > MAX_RESOLVERS {
        return Err(bad(format!(
            "{} DNS servers were given and glibc's resolver reads at most {MAX_RESOLVERS}; the \
             rest would be stored and never asked",
            params.servers.len()
        )));
    }

    // Parsed as addresses, not pattern-matched. The tool this replaces checked that every
    // character was a digit, a dot or a colon, which accepts `...`, `999.999.999.999` and `:`.
    let mut v4: Vec<String> = Vec::new();
    let mut v6: Vec<String> = Vec::new();
    for server in &params.servers {
        let server = server.trim();
        match server.parse::<IpAddr>() {
            Ok(IpAddr::V4(a)) => v4.push(a.to_string()),
            Ok(IpAddr::V6(a)) => v6.push(a.to_string()),
            Err(_) => {
                return Err(bad(format!(
                    "\"{server}\" is not an IP address. A DNS server is named by address here, \
                     not by hostname — a hostname would have to be resolved by the resolver this \
                     call is about to change"
                )))
            }
        }
    }

    let active = active_connections().map_err(|t| service_error(&t))?;
    let gateways = devices_with_gateway(&active);
    let Some(target) = nmcli::resolver_connection(&active, &gateways) else {
        return Err(ServiceError {
            code: -32033,
            message: "this machine has no active network connection to set resolvers on"
                .to_string(),
        });
    };

    // `modify uuid <uuid>`, never `modify <name>`: a Wi-Fi profile is named after its SSID and an
    // SSID may begin with a dash or contain a space.
    let joined_v4 = v4.join(" ");
    let joined_v6 = v6.join(" ");
    let mut argv: Vec<&str> = vec!["connection", "modify", "uuid", target.uuid.as_str()];
    if !v4.is_empty() {
        argv.extend_from_slice(&["ipv4.dns", joined_v4.as_str(), "ipv4.ignore-auto-dns", "yes"]);
    }
    if !v6.is_empty() {
        argv.extend_from_slice(&["ipv6.dns", joined_v6.as_str(), "ipv6.ignore-auto-dns", "yes"]);
    }
    let exit = nmcli::run(nmcli::QUICK_WAIT_SECS, &argv);
    nmcli::outcome(&exit).map_err(|t| service_error(&t))?;

    // Stored is not applied. The profile now says so on disk and the running link does not.
    let exit = nmcli::run(
        nmcli::APPLY_WAIT_SECS,
        &["connection", "up", "uuid", target.uuid.as_str()],
    );
    nmcli::outcome(&exit).map_err(|t| service_error(&t))?;

    let resolv_conf = read_dns()?;
    let applied = device_dns(&target.device);

    // Every server asked for has to turn up in one of the two readings, or this failed. A
    // `connection up` that exits zero having re-activated the profile without the new resolvers —
    // because the connection is shared with another setting, because a stub resolver sits in
    // front, because NetworkManager kept an old applied connection — is a success message over an
    // unchanged machine, which is the shape of bug this whole pass exists to remove.
    let missing: Vec<&String> = params
        .servers
        .iter()
        .filter(|wanted| {
            let wanted = wanted.trim();
            !applied.iter().any(|s| s == wanted)
                && !resolv_conf.nameservers.iter().any(|s| s == wanted)
        })
        .collect();
    if !missing.is_empty() {
        return Err(disagreed(&format!(
            "nmcli accepted the change and {} is not among this machine's resolvers afterwards. \
             /etc/resolv.conf says [{}]; NetworkManager says the {} device has [{}]",
            missing
                .iter()
                .map(|s| format!("\"{s}\""))
                .collect::<Vec<_>>()
                .join(", "),
            resolv_conf.nameservers.join(", "),
            target.device,
            applied.join(", ")
        )));
    }

    Ok(DnsSetResult {
        connection: target.name.clone(),
        device: target.device.clone(),
        resolv_conf,
        device_dns: applied,
    })
}

// ══════════════════════════════════════════════════════════════════════
// Firewall
// ══════════════════════════════════════════════════════════════════════

/// Which firewall is on this machine and what it is doing — read, never assumed.
///
/// Like [`wifi_state`] this never fails: "there is no firewall tool here" and "I was not allowed
/// to read the ruleset" are both answers, and the one thing that must not come back is a
/// confident `false`.
fn firewall_state() -> FirewallState {
    for (kind, binary) in firewall::CANDIDATES {
        let exit = match kind {
            "nftables" => firewall::run(binary, &["list", "ruleset"]),
            "ufw" => firewall::run(binary, &["status", "verbose"]),
            _ => firewall::run(binary, &["--state"]),
        };
        // A tool that is not installed is not this machine's firewall; try the next one.
        if matches!(exit, nmcli::Exit::Missing) {
            continue;
        }
        return match kind {
            "nftables" => firewall::read_nftables(&exit),
            "ufw" => firewall::read_ufw(&exit),
            _ => firewall::read_firewalld(&exit),
        };
    }
    // None of the three is installed. `absent`, with the list of what was looked for, comes back
    // from any of the three readers given a `Missing`; nftables is asked for the sentence.
    firewall::read_nftables(&nmcli::Exit::Missing)
}

// ══════════════════════════════════════════════════════════════════════
// Control surface (app.describe)
// ══════════════════════════════════════════════════════════════════════

/// Connectivity as data: "am I online, and how", plus interfaces, resolvers, Wi-Fi and firewall.
///
/// Read-only, deliberately. The verbs live on the Network Manager app's own surface (`app-network`
/// — `apps/network-manager/src/main.rs`), where they are graded, where a refusal reaches a person
/// on screen as well as the caller, and where there is exactly one path behind each button. A
/// service that published the same verbs unstated would be a second way into the same domain,
/// which is the split this fleet has been closing everywhere else.
fn describe_view() -> Result<View, ServiceError> {
    let status = read_status()?;
    let ifaces = read_interfaces().unwrap_or_default();
    let dns = read_dns().ok();
    let wifi = wifi_state();
    let fw = firewall_state();

    let summary = if status.connected {
        let where_ = status.ssid.clone().unwrap_or_else(|| status.conn_type.clone());
        let ip = status.ip_address.clone().unwrap_or_else(|| "no address".to_string());
        format!("Network — online via {where_}, {ip}")
    } else {
        "Network — offline".to_string()
    };

    let interfaces: Vec<serde_json::Value> = ifaces
        .iter()
        // Loopback is never the answer to "how am I connected"; drop it from the glance.
        .filter(|i| i.name != "lo")
        .map(|i| {
            serde_json::json!({
                "name": i.name,
                "type": i.conn_type.as_str(),
                "state": i.state,
                "ip": i.ip_address,
                "mac": i.mac_address,
            })
        })
        .collect();

    let mut view = View::new(summary)
        .with("connected", status.connected)
        .with("type", status.conn_type)
        .with("ssid", status.ssid.map(serde_json::Value::String).unwrap_or(serde_json::Value::Null))
        .with("ip_address", status.ip_address.map(serde_json::Value::String).unwrap_or(serde_json::Value::Null))
        .with("interfaces", serde_json::Value::Array(interfaces))
        .with("wifi", serde_json::to_value(&wifi).unwrap_or(serde_json::Value::Null))
        .with("firewall", serde_json::to_value(&fw).unwrap_or(serde_json::Value::Null));
    if let Some(dns) = dns {
        view = view
            .with("nameservers", serde_json::json!(dns.nameservers))
            .with("search_domains", serde_json::json!(dns.search_domains));
    }
    Ok(view)
}

/// See the note on [`describe_view`]: an empty action list is the honest statement that this
/// socket is for reading, and that the verbs are published by the app.
fn network_actions() -> Vec<Action> {
    Vec::new()
}

// ══════════════════════════════════════════════════════════════════════
// Linux implementation (reads /proc, /sys, /etc)
// ══════════════════════════════════════════════════════════════════════

#[cfg(unix)]
mod platform {
    use super::*;

    /// Read network interfaces from /proc/net/dev and enrich with /sys metadata.
    pub fn read_interfaces() -> Result<Vec<NetworkInterfaceInfo>, ServiceError> {
        let content = std::fs::read_to_string("/proc/net/dev").map_err(|e| ServiceError {
            code: -32000,
            message: format!("Cannot read /proc/net/dev: {e}"),
        })?;

        let mut interfaces = Vec::new();

        for line in content.lines().skip(2) {
            let line = line.trim();
            let (name, rest) = match line.split_once(':') {
                Some(pair) => pair,
                None => continue,
            };
            let name = name.trim();
            if name == "lo" {
                continue;
            }

            let values: Vec<u64> = rest
                .split_whitespace()
                .filter_map(|s| s.parse().ok())
                .collect();
            if values.len() < 10 {
                continue;
            }

            let rx_bytes = values[0];
            let tx_bytes = values[8];

            let mac_address = read_sys_attr(name, "address");
            let operstate = read_sys_attr(name, "operstate");
            let state = if operstate.is_empty() {
                "unknown".to_string()
            } else {
                operstate
            };

            let conn_type = detect_interface_type(name);
            let ip_address = read_interface_ip(name);

            interfaces.push(NetworkInterfaceInfo {
                name: name.to_string(),
                mac_address,
                ip_address,
                rx_bytes,
                tx_bytes,
                state,
                conn_type,
            });
        }

        Ok(interfaces)
    }

    /// Determine overall connectivity status.
    pub fn read_status() -> Result<NetworkStatus, ServiceError> {
        let interfaces = read_interfaces()?;

        for iface in &interfaces {
            if iface.state == "up" && iface.ip_address.is_some() {
                // Read through NetworkManager now, rather than the `None` the stub this replaces
                // returned for every machine. See `connected_ssid_for`.
                let ssid = if matches!(iface.conn_type, ConnectionType::Wifi) {
                    super::connected_ssid_for(&iface.name)
                } else {
                    None
                };

                return Ok(NetworkStatus {
                    connected: true,
                    conn_type: iface.conn_type.as_str().to_string(),
                    ssid,
                    ip_address: iface.ip_address.clone(),
                });
            }
        }

        Ok(NetworkStatus {
            connected: false,
            conn_type: "none".to_string(),
            ssid: None,
            ip_address: None,
        })
    }

    /// Read DNS configuration from /etc/resolv.conf.
    pub fn read_dns() -> Result<DnsConfig, ServiceError> {
        let content = std::fs::read_to_string("/etc/resolv.conf").unwrap_or_default();

        let mut nameservers = Vec::new();
        let mut search_domains = Vec::new();

        for line in content.lines() {
            let line = line.trim();
            if line.starts_with('#') || line.is_empty() {
                continue;
            }

            if let Some(rest) = line.strip_prefix("nameserver") {
                let ns = rest.trim();
                if !ns.is_empty() {
                    nameservers.push(ns.to_string());
                }
            } else if let Some(rest) = line.strip_prefix("search") {
                for domain in rest.split_whitespace() {
                    search_domains.push(domain.to_string());
                }
            }
        }

        Ok(DnsConfig {
            nameservers,
            search_domains,
        })
    }

    /// Read a sysfs attribute for a network interface.
    fn read_sys_attr(iface: &str, attr: &str) -> String {
        std::fs::read_to_string(format!("/sys/class/net/{iface}/{attr}"))
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }

    /// Detect interface type from name conventions and sysfs.
    fn detect_interface_type(name: &str) -> ConnectionType {
        // Check sysfs type field (1 = ethernet, 801 = wifi, etc.)
        if let Ok(content) = std::fs::read_to_string(format!("/sys/class/net/{name}/type")) {
            if content.trim() == "801" {
                return ConnectionType::Wifi;
            }
        }

        // Check if wireless directory exists
        if std::path::Path::new(&format!("/sys/class/net/{name}/wireless")).exists() {
            return ConnectionType::Wifi;
        }

        // Fall back to name-based heuristics
        if name.starts_with("wl") || name.starts_with("wlan") {
            ConnectionType::Wifi
        } else if name.starts_with("eth")
            || name.starts_with("en")
            || name.starts_with("eno")
            || name.starts_with("ens")
        {
            ConnectionType::Ethernet
        } else if name.starts_with("tun") || name.starts_with("tap") || name.starts_with("wg") {
            ConnectionType::Vpn
        } else if name.starts_with("br") || name.starts_with("docker") || name.starts_with("virbr")
        {
            ConnectionType::Bridge
        } else {
            ConnectionType::Other(name.to_string())
        }
    }

    /// The interface's IPv4 address, via `SIOCGIFADDR`, without shelling out.
    fn read_interface_ip(name: &str) -> Option<String> {
        get_ipv4_addr(name)
    }

    /// Get IPv4 address for an interface using libc ioctl.
    fn get_ipv4_addr(iface_name: &str) -> Option<String> {
        use std::mem;
        use std::os::unix::io::RawFd;

        let sock: RawFd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
        if sock < 0 {
            return None;
        }

        let mut ifr: libc::ifreq = unsafe { mem::zeroed() };
        let name_bytes = iface_name.as_bytes();
        let copy_len = name_bytes.len().min(libc::IFNAMSIZ - 1);
        unsafe {
            std::ptr::copy_nonoverlapping(
                name_bytes.as_ptr(),
                ifr.ifr_name.as_mut_ptr() as *mut u8,
                copy_len,
            );
        }

        let result = unsafe { libc::ioctl(sock, libc::SIOCGIFADDR as _, &mut ifr) };
        unsafe {
            libc::close(sock);
        }

        if result < 0 {
            return None;
        }

        let addr = unsafe { ifr.ifr_ifru.ifru_addr };
        // `sa_family` is `u8` on macOS/BSD and `u16` on Linux — compare via `u32`.
        if addr.sa_family as u32 != libc::AF_INET as u32 {
            return None;
        }

        let sin: libc::sockaddr_in = unsafe { mem::transmute(addr) };
        let ip = sin.sin_addr.s_addr.to_ne_bytes();
        Some(format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]))
    }
}

// ══════════════════════════════════════════════════════════════════════
// Windows stub (for compilation only — service runs on Linux)
// ══════════════════════════════════════════════════════════════════════

#[cfg(not(unix))]
mod platform {
    use super::*;

    pub fn read_interfaces() -> Result<Vec<NetworkInterfaceInfo>, ServiceError> {
        Ok(Vec::new())
    }

    pub fn read_status() -> Result<NetworkStatus, ServiceError> {
        Ok(NetworkStatus::default())
    }

    pub fn read_dns() -> Result<DnsConfig, ServiceError> {
        Ok(DnsConfig::default())
    }
}

fn read_interfaces() -> Result<Vec<NetworkInterfaceInfo>, ServiceError> {
    platform::read_interfaces()
}

fn read_status() -> Result<NetworkStatus, ServiceError> {
    platform::read_status()
}

fn read_dns() -> Result<DnsConfig, ServiceError> {
    platform::read_dns()
}
