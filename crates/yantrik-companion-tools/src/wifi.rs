//! Wi-Fi tools — status, scan, connect, disconnect, radio.
//!
//! # What these used to do
//!
//! Four tools, each shelling out on their own, in a file whose first line claimed support for
//! `nmcli`, `iwctl` and `wpa_cli`. This OS ships NetworkManager and nothing else —
//! `deploy/yantrik-os/build-debian-iso.sh` installs `network-manager`, there is no `iwd` and no
//! `iwctl` anywhere in the build — so two of the three backends were code for a machine that does
//! not exist, and their `wlan0` hardcoding was wrong for any machine that did.
//!
//! - **`wifi_connect` put the password in `argv`.** `cmd.args(["password", password])` leaves the
//!   passphrase in `/proc/<pid>/cmdline`, world-readable, for as long as nmcli runs — which for
//!   an association is up to twenty-five seconds. Every other user and every other process on the
//!   machine could read it out of `ps`. This is the fault that made this file urgent.
//! - **`wifi_disconnect` hardcoded `wlan0`.** On a machine whose adapter is `wlp3s0` — which is
//!   every machine with predictable interface names, so most of them — it disconnected nothing
//!   and said "WiFi disconnected."
//! - **`wifi_scan` split `nmcli -t` output on a bare `:`.** nmcli escapes a colon inside a value
//!   as `\:`, and an SSID is free-form: `Cafe: Free` is an ordinary network name and came back
//!   truncated to `Cafe`, which is a network this machine cannot join while the list looks fine.
//! - **`wifi_status` grepped the device list for the literal string `wifi`** and read signal
//!   strength out of `/proc/net/wireless`, so a machine with no adapter got an empty answer that
//!   read the same as a machine whose radio was off.
//! - **Nothing re-read anything.** `nmcli ... connect` exiting zero produced "Connected to
//!   '<ssid>'" whether or not this machine ended up on that network.
//!
//! # What they are now
//!
//! Callers of `services/network-service`, which owns Wi-Fi on this machine (commit `d189ac1`),
//! through [`crate::networking::backend::NetworkBackend`]. The service passes the secret to nmcli
//! on stdin with `--ask` and never in `argv`; it reads adapter presence from
//! `/sys/class/net/<iface>/wireless` rather than guessing a device name; it parses nmcli's
//! escaping; and every write re-reads the machine and fails if what it asked for is not what it
//! then saw. The answers below are that re-read, not the request echoed back.
//!
//! # Grades
//!
//! `wifi_disconnect` and `wifi_radio` are **dangerous**, matching
//! `apps/network-manager/src/main.rs`. What they destroy is not data, it is the channel: a mind
//! driving this machine from somewhere else that turns the radio off has cut the wire the undo
//! would have travelled down, and nothing in this tool list can put it back. `wifi_disconnect`
//! was `Sensitive` here, one grade below the same action on the same service through the app —
//! the same verb should not be cheaper because the mind asked for it.

use std::sync::Arc;

use super::{PermissionLevel, Tool, ToolContext, ToolRegistry};

use crate::networking::backend::{NetworkBackend, ServiceNetwork};
use yantrik_ipc_contracts::network::{
    ScannedNetwork, WifiConnectParams, WifiRadioParams, WifiScanParams, WifiState,
};

pub fn register(reg: &mut ToolRegistry) {
    register_with(reg, Arc::new(ServiceNetwork));
}

/// Register against a given backend. The tests use it; `register` is the machine's own.
pub fn register_with(reg: &mut ToolRegistry, net: Arc<dyn NetworkBackend>) {
    reg.register(Box::new(WifiStatusTool { net: net.clone() }));
    reg.register(Box::new(WifiScanTool { net: net.clone() }));
    reg.register(Box::new(WifiConnectTool { net: net.clone() }));
    reg.register(Box::new(WifiDisconnectTool { net: net.clone() }));
    reg.register(Box::new(WifiRadioTool { net }));
}

/// How a failure from the service reads to a model.
fn refused(what: &str, why: &str) -> String {
    format!("Could not {what}: {why}")
}

/// One Wi-Fi state, in words, with the interface name the *service* read rather than a guess.
///
/// `adapter_present` is answered before anything else, because "this machine has no Wi-Fi" and
/// "the radio is off" invite completely different next moves and the tools this replaces returned
/// the same empty string for both.
fn format_state(state: &WifiState) -> String {
    if !state.adapter_present {
        return match &state.reason {
            Some(reason) => format!("This machine has no Wi-Fi adapter ({reason})."),
            None => "This machine has no Wi-Fi adapter.".to_string(),
        };
    }

    let device = state.device.as_deref().unwrap_or("unknown interface");
    let mut out = format!("Wi-Fi ({device}): radio {}", state.radio.as_str());

    match &state.connected_ssid {
        Some(ssid) => {
            out.push_str(&format!(", joined to \"{ssid}\""));
            if let Some(signal) = state.signal {
                out.push_str(&format!(", signal {signal}%"));
            }
            if let Some(rate) = &state.rate {
                out.push_str(&format!(", {rate}"));
            }
        }
        None => out.push_str(", not joined to any network"),
    }
    out.push('.');

    for (label, value) in [
        ("IP", state.ip_address.as_ref()),
        ("gateway", state.gateway.as_ref()),
        ("subnet", state.subnet.as_ref()),
    ] {
        if let Some(value) = value {
            out.push_str(&format!("\n  {label}: {value}"));
        }
    }
    // A reason on a machine that *does* have an adapter is the service saying its answer is thin
    // — NetworkManager down, polkit refusing. Printed, because otherwise the thin answer reads as
    // a complete one.
    if let Some(reason) = &state.reason {
        out.push_str(&format!("\n  note: {reason}"));
    }
    out
}

// ── Wi-Fi status ──

pub struct WifiStatusTool {
    net: Arc<dyn NetworkBackend>,
}

impl Tool for WifiStatusTool {
    fn name(&self) -> &'static str { "wifi_status" }
    fn permission(&self) -> PermissionLevel { PermissionLevel::Safe }
    fn category(&self) -> &'static str { "wifi" }

    fn definition(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "wifi_status",
                "description": "Show whether this machine has Wi-Fi, what its radio is doing, and which network it is on",
                "parameters": { "type": "object", "properties": {} }
            }
        })
    }

    fn execute(&self, _ctx: &ToolContext, _args: &serde_json::Value) -> String {
        status_answer(&*self.net)
    }
}

fn status_answer(net: &dyn NetworkBackend) -> String {
    match net.wifi_state() {
        Ok(state) => format_state(&state),
        Err(why) => refused("read this machine's Wi-Fi state", &why),
    }
}

// ── Wi-Fi scan ──

pub struct WifiScanTool {
    net: Arc<dyn NetworkBackend>,
}

/// The access points a scan found.
///
/// A hidden network keeps a row. nmcli prints an empty SSID for one, and the tool this replaces
/// dropped every row whose first field was empty — which is how a list comes to say "No WiFi
/// networks found" with networks in range.
fn format_scan(networks: &[ScannedNetwork]) -> String {
    if networks.is_empty() {
        return "The adapter looked and found no networks in range.".to_string();
    }
    let mut out = String::from("Networks in range:\n");
    for network in networks.iter().take(20) {
        let name = if network.hidden {
            "(hidden network)".to_string()
        } else {
            format!("\"{}\"", network.ssid)
        };
        let mut marks = Vec::new();
        if network.is_connected {
            marks.push("joined");
        }
        if network.is_saved {
            marks.push("saved");
        }
        out.push_str(&format!(
            "  {name} — {}% signal, {}{}{}\n",
            network.signal,
            // nmcli's own word. `--` is an open network and is left as nmcli wrote it rather than
            // relabelled "Open", so a person comparing against `nmcli device wifi list` sees the
            // same text.
            if network.security.is_empty() { "security unknown" } else { &network.security },
            if network.rate.is_empty() { String::new() } else { format!(", {}", network.rate) },
            if marks.is_empty() { String::new() } else { format!(" [{}]", marks.join(", ")) },
        ));
    }
    if networks.len() > 20 {
        out.push_str(&format!("  … and {} more\n", networks.len() - 20));
    }
    out
}

impl Tool for WifiScanTool {
    fn name(&self) -> &'static str { "wifi_scan" }
    /// `Standard`, not `Safe`. A rescan sweeps the bands and puts the radio off the air for a few
    /// seconds; it changes no connection. That is the grade the same action carries on the
    /// Network Manager app's surface.
    fn permission(&self) -> PermissionLevel { PermissionLevel::Standard }
    fn category(&self) -> &'static str { "wifi" }

    fn definition(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "wifi_scan",
                "description": "Scan for nearby Wi-Fi networks. Takes a few seconds; the radio is off the air while it sweeps.",
                "parameters": { "type": "object", "properties": {} }
            }
        })
    }

    fn execute(&self, _ctx: &ToolContext, _args: &serde_json::Value) -> String {
        scan_answer(&*self.net)
    }
}

fn scan_answer(net: &dyn NetworkBackend) -> String {
    // `rescan: true` — a tool call asking what is in range means now, not NetworkManager's cache
    // from some minutes ago. The service refuses and says so if there is no adapter to sweep with.
    match net.wifi_scan(&WifiScanParams { rescan: true }) {
        Ok(networks) => format_scan(&networks),
        Err(why) => refused("scan for Wi-Fi networks", &why),
    }
}

// ── Wi-Fi connect ──

pub struct WifiConnectTool {
    net: Arc<dyn NetworkBackend>,
}

impl Tool for WifiConnectTool {
    fn name(&self) -> &'static str { "wifi_connect" }
    fn permission(&self) -> PermissionLevel { PermissionLevel::Sensitive }
    fn category(&self) -> &'static str { "wifi" }

    fn definition(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "wifi_connect",
                "description": "Join a Wi-Fi network by SSID. Leave the password out for an open network or one this machine has already saved.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "ssid": {"type": "string", "description": "Network name (SSID)"},
                        "password": {"type": "string", "description": "Network password (WPA/WPA2/WPA3)"}
                    },
                    "required": ["ssid"]
                }
            }
        })
    }

    fn execute(&self, _ctx: &ToolContext, args: &serde_json::Value) -> String {
        connect_answer(&*self.net, args)
    }
}

fn connect_answer(net: &dyn NetworkBackend, args: &serde_json::Value) -> String {
    let ssid = args.get("ssid").and_then(|v| v.as_str()).unwrap_or_default().trim();
    if ssid.is_empty() {
        return "Error: ssid is required".to_string();
    }

    // The shell-metacharacter check the old tool did is gone with the shell. Nothing here builds
    // a command line: the SSID crosses the socket as a JSON string and the service hands it to
    // nmcli as one `argv` element. An SSID containing `;` or `$` is a legal network name and
    // refusing it was refusing to join a real network for a danger that no longer exists.
    let password = args
        .get("password")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|p| !p.is_empty());

    // `WifiConnectParams` has a hand-written `Debug` that prints `<given>`, so a `{:?}` in a log
    // line or a trace cannot leak this. Nothing below echoes it back either.
    match net.wifi_connect(&WifiConnectParams {
        ssid: ssid.to_string(),
        password,
    }) {
        // The service only returns `Ok` when it re-read the machine and found it joined to the
        // SSID asked for; anything else came back as an error naming what it is actually on.
        Ok(state) => format!("Joined \"{ssid}\".\n{}", format_state(&state)),
        Err(why) => refused(&format!("join \"{ssid}\""), &why),
    }
}

// ── Wi-Fi disconnect ──

pub struct WifiDisconnectTool {
    net: Arc<dyn NetworkBackend>,
}

impl Tool for WifiDisconnectTool {
    fn name(&self) -> &'static str { "wifi_disconnect" }
    /// **Dangerous**, and it was `Sensitive`. `stop` on a container is `sensitive` because `start`
    /// is one call away; there is no such call for a mind that has just taken this machine off the
    /// network it was reached over. See the module header.
    fn permission(&self) -> PermissionLevel { PermissionLevel::Dangerous }
    fn category(&self) -> &'static str { "wifi" }

    fn definition(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "wifi_disconnect",
                "description": "Leave the current Wi-Fi network. If this machine is reached over Wi-Fi, this cuts that connection and nothing here can restore it.",
                "parameters": { "type": "object", "properties": {} }
            }
        })
    }

    fn execute(&self, _ctx: &ToolContext, _args: &serde_json::Value) -> String {
        disconnect_answer(&*self.net)
    }
}

fn disconnect_answer(net: &dyn NetworkBackend) -> String {
    match net.wifi_disconnect() {
        // The service re-reads and fails if the machine is still joined, so an `Ok` here means the
        // link is actually down — not that nmcli exited zero.
        Ok(state) => format!("Left the Wi-Fi network.\n{}", format_state(&state)),
        Err(why) => refused("disconnect from Wi-Fi", &why),
    }
}

// ── Wi-Fi radio ──

pub struct WifiRadioTool {
    net: Arc<dyn NetworkBackend>,
}

impl Tool for WifiRadioTool {
    fn name(&self) -> &'static str { "wifi_radio" }
    /// **Dangerous** for its worst argument. The ladder grades actions, not argument values, and
    /// `off` on a machine reached over Wi-Fi is unrecoverable from here — the call that would turn
    /// it back on travels over the link it just switched off.
    fn permission(&self) -> PermissionLevel { PermissionLevel::Dangerous }
    fn category(&self) -> &'static str { "wifi" }

    fn definition(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "wifi_radio",
                "description": "Turn the Wi-Fi radio on or off. Turning it off on a machine reached over Wi-Fi cuts that connection and nothing here can restore it.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "enabled": {"type": "boolean", "description": "true to turn the radio on, false to turn it off"}
                    },
                    "required": ["enabled"]
                }
            }
        })
    }

    fn execute(&self, _ctx: &ToolContext, args: &serde_json::Value) -> String {
        radio_answer(&*self.net, args)
    }
}

fn radio_answer(net: &dyn NetworkBackend, args: &serde_json::Value) -> String {
    // No default. `network.wifi_toggle` used to send `{"enabled": !current}` computed from a
    // property nothing had written, so it asked to turn on a radio that was already on as often as
    // not; a missing argument here becomes the same guess one layer up.
    let Some(enabled) = args.get("enabled").and_then(|v| v.as_bool()) else {
        return "Error: `enabled` is required — true to turn the Wi-Fi radio on, false to turn it \
                off. This tool does not flip whatever it finds."
            .to_string();
    };

    match net.wifi_radio(&WifiRadioParams { enabled }) {
        // The service re-reads the radio and fails if it does not read back the way it was asked,
        // so this sentence is a reading.
        Ok(state) => format!(
            "Wi-Fi radio is {} now.\n{}",
            state.radio.as_str(),
            format_state(&state)
        ),
        Err(why) => refused(
            &format!("turn the Wi-Fi radio {}", if enabled { "on" } else { "off" }),
            &why,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::networking::backend::fake::FakeNetwork;
    use yantrik_ipc_contracts::network::RadioState;

    fn wired_machine() -> WifiState {
        // The VM this OS is tested on: ethernet only, `/sys/class/net/*/wireless` empty.
        WifiState {
            adapter_present: false,
            radio: RadioState::Unknown,
            ..Default::default()
        }
    }

    fn joined(ssid: &str) -> WifiState {
        WifiState {
            adapter_present: true,
            device: Some("wlp3s0".into()),
            radio: RadioState::On,
            connected_ssid: Some(ssid.into()),
            signal: Some(72),
            rate: Some("270 Mbit/s".into()),
            ip_address: Some("192.168.1.24".into()),
            gateway: Some("192.168.1.1".into()),
            subnet: Some("255.255.255.0".into()),
            reason: None,
        }
    }

    #[test]
    fn a_machine_with_no_adapter_is_told_apart_from_a_radio_that_is_off() {
        let absent = format_state(&wired_machine());
        assert!(absent.contains("no Wi-Fi adapter"), "{absent}");
        assert!(!absent.contains("off"), "{absent}");

        let off = format_state(&WifiState {
            adapter_present: true,
            device: Some("wlp3s0".into()),
            radio: RadioState::Off,
            ..Default::default()
        });
        assert!(off.contains("radio off"), "{off}");
        assert!(!off.contains("no Wi-Fi adapter"), "{off}");
    }

    #[test]
    fn the_interface_name_comes_from_the_service_and_is_never_wlan0() {
        // The tool this replaces hardcoded `wlan0` in three places. This machine's adapter is
        // `wlp3s0`, which is what a predictable-names kernel calls it.
        let text = format_state(&joined("Cafe: Free"));
        assert!(text.contains("wlp3s0"), "{text}");
        assert!(!text.contains("wlan0"), "{text}");
        // And an SSID with a colon in it survives, because nothing splits on one any more.
        assert!(text.contains("\"Cafe: Free\""), "{text}");
    }

    #[test]
    fn connecting_reports_the_network_the_machine_is_actually_on() {
        let net = FakeNetwork {
            wifi_connect: Some(Ok(joined("Cafe: Free"))),
            ..Default::default()
        };
        let text = connect_answer(
            &net,
            &serde_json::json!({ "ssid": "Cafe: Free", "password": "hunter2" }),
        );
        assert!(text.contains("Joined \"Cafe: Free\""), "{text}");
        assert!(text.contains("192.168.1.24"), "{text}");
        // Nothing echoes the secret back. The old tool did not either, but nothing checked.
        assert!(!text.contains("hunter2"), "the password must not appear in the answer: {text}");
        // And the secret did leave, as a typed field, rather than being dropped.
        let seen = net.connect_seen.lock().unwrap();
        assert_eq!(seen[0].ssid, "Cafe: Free");
        assert_eq!(seen[0].password.as_deref(), Some("hunter2"));
        // The contract's hand-written Debug is the backstop for a `{:?}` in a log line.
        assert!(format!("{:?}", seen[0]).contains("<given>"));
        assert!(!format!("{:?}", seen[0]).contains("hunter2"));
    }

    #[test]
    fn a_wrong_password_is_named_rather_than_reported_as_failure_to_connect() {
        let net = FakeNetwork {
            wifi_connect: Some(Err(
                "the network refused the password: Error: Connection activation failed: \
                 (7) Secrets were required, but not provided."
                    .into(),
            )),
            ..Default::default()
        };
        let text = connect_answer(&net, &serde_json::json!({ "ssid": "Cafe", "password": "wrong" }));
        assert!(text.starts_with("Could not join \"Cafe\""), "{text}");
        assert!(text.contains("refused the password"), "{text}");
        assert!(!text.contains("wrong"), "{text}");
    }

    #[test]
    fn a_connect_on_a_machine_with_no_adapter_says_which_of_the_two_things_is_wrong() {
        let net = FakeNetwork {
            wifi_connect: Some(Err("this machine has no Wi-Fi adapter".into())),
            ..Default::default()
        };
        let text = connect_answer(&net, &serde_json::json!({ "ssid": "anything" }));
        assert!(text.contains("no Wi-Fi adapter"), "{text}");
    }

    #[test]
    fn an_open_network_sends_no_password_field_at_all() {
        let net = FakeNetwork {
            wifi_connect: Some(Ok(joined("Airport Free"))),
            ..Default::default()
        };
        let _ = connect_answer(&net, &serde_json::json!({ "ssid": "Airport Free" }));
        assert_eq!(net.connect_seen.lock().unwrap()[0].password, None);

        // An empty string is "the box was cleared", not "the password is empty".
        let net2 = FakeNetwork {
            wifi_connect: Some(Ok(joined("Airport Free"))),
            ..Default::default()
        };
        let _ = connect_answer(
            &net2,
            &serde_json::json!({ "ssid": "Airport Free", "password": "" }),
        );
        assert_eq!(net2.connect_seen.lock().unwrap()[0].password, None);
    }

    #[test]
    fn an_ssid_with_shell_metacharacters_is_no_longer_refused() {
        // The old tool rejected `$`, `;`, backtick, `|` and `&` in an SSID because it was building
        // a command line. Nothing builds one now, and those are legal network names.
        let net = FakeNetwork {
            wifi_connect: Some(Ok(joined("Bar & Grill"))),
            ..Default::default()
        };
        let text = connect_answer(&net, &serde_json::json!({ "ssid": "Bar & Grill" }));
        assert!(!text.contains("invalid characters"), "{text}");
        assert_eq!(net.connect_seen.lock().unwrap()[0].ssid, "Bar & Grill");
    }

    #[test]
    fn a_hidden_network_keeps_its_row_in_a_scan() {
        let net = FakeNetwork {
            wifi_scan: Some(Ok(vec![
                ScannedNetwork {
                    ssid: "Cafe: Free".into(),
                    hidden: false,
                    signal: 80,
                    security: "WPA2".into(),
                    rate: "270 Mbit/s".into(),
                    is_connected: true,
                    is_saved: true,
                },
                ScannedNetwork {
                    ssid: String::new(),
                    hidden: true,
                    signal: 41,
                    security: "WPA3".into(),
                    ..Default::default()
                },
            ])),
            ..Default::default()
        };
        let text = scan_answer(&net);
        assert!(text.contains("(hidden network)"), "{text}");
        assert!(text.contains("\"Cafe: Free\""), "{text}");
        assert!(text.contains("[joined, saved]"), "{text}");
        // A tool call asking what is in range means now, not a cache from minutes ago.
        assert_eq!(*net.scan_seen.lock().unwrap(), vec![true]);
    }

    #[test]
    fn a_scan_that_found_nothing_is_not_the_same_sentence_as_a_scan_that_failed() {
        let empty = FakeNetwork {
            wifi_scan: Some(Ok(Vec::new())),
            ..Default::default()
        };
        assert!(scan_answer(&empty).contains("found no networks"), );

        let broken = FakeNetwork {
            wifi_scan: Some(Err("NetworkManager is not running".into())),
            ..Default::default()
        };
        let text = scan_answer(&broken);
        assert!(text.starts_with("Could not scan"), "{text}");
        assert!(text.contains("NetworkManager is not running"), "{text}");
    }

    #[test]
    fn the_radio_tool_refuses_to_guess_which_way_to_flip() {
        let net = FakeNetwork::default();
        let text = radio_answer(&net, &serde_json::json!({}));
        assert!(text.starts_with("Error:"), "{text}");
        // Nothing was asked of the service — a `FakeNetwork` with no script would have panicked.
        assert!(net.radio_seen.lock().unwrap().is_empty());
    }

    #[test]
    fn the_radio_answer_is_the_state_read_back_afterwards() {
        let net = FakeNetwork {
            wifi_radio: Some(Ok(WifiState {
                adapter_present: true,
                device: Some("wlp3s0".into()),
                radio: RadioState::On,
                ..Default::default()
            })),
            ..Default::default()
        };
        let text = radio_answer(&net, &serde_json::json!({ "enabled": true }));
        assert!(text.contains("radio is on now"), "{text}");
        assert_eq!(*net.radio_seen.lock().unwrap(), vec![true]);
    }

    #[test]
    fn a_disconnect_that_did_not_take_is_a_failure_and_not_a_cheerful_sentence() {
        // The old tool said "WiFi disconnected." on any zero exit from a command that named
        // `wlan0` — an interface most machines do not have.
        let net = FakeNetwork {
            wifi_disconnect: Some(Err(
                "nmcli accepted the disconnect and this machine is still joined to \"Cafe\"".into(),
            )),
            ..Default::default()
        };
        let text = disconnect_answer(&net);
        assert!(text.starts_with("Could not disconnect"), "{text}");
        assert!(text.contains("still joined"), "{text}");
    }

    #[test]
    fn the_two_actions_that_can_cut_the_wire_are_graded_dangerous() {
        // Matching apps/network-manager/src/main.rs, where the same two verbs on the same service
        // are `dangerous`. The same action must not be cheaper because the mind asked for it.
        let net: Arc<dyn NetworkBackend> = Arc::new(FakeNetwork::default());
        assert_eq!(
            WifiDisconnectTool { net: net.clone() }.permission(),
            PermissionLevel::Dangerous
        );
        assert_eq!(
            WifiRadioTool { net: net.clone() }.permission(),
            PermissionLevel::Dangerous
        );
        // And the ones that do not: joining a network leaves a wired link untouched and a failed
        // association leaves the previous one standing.
        assert_eq!(
            WifiConnectTool { net: net.clone() }.permission(),
            PermissionLevel::Sensitive
        );
        assert_eq!(WifiStatusTool { net }.permission(), PermissionLevel::Safe);
    }
}
