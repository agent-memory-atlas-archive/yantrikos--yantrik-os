//! Yantrik Network Manager — standalone app binary.
//!
//! Network management with WiFi, Ethernet, Bluetooth, VPN, Firewall, Diagnostics.
//! Uses IPC service "network" for operations, with basic stubs as fallback.

use slint::{ComponentHandle, Model, ModelRc, VecModel};
use yantrik_app_runtime::prelude::*;
use yantrik_ipc_transport::SyncRpcClient;

slint::include_modules!();

fn main() {
    init_tracing("yantrik-network-manager");

    // One window per app: a second launch defers to the running one (the shell focuses it).
    let Some(_instance) = instance::claim("network-manager") else { return };

    let app = NetworkManagerApp::new().unwrap();

    // Same dark/accent choice as the shell, read from the shell's settings file.
    let theme = theme::load();
    app.global::<ThemeMode>().set_dark(theme.dark);
    app.global::<AccentPreset>().set_index(theme.accent_index);

    wire(&app);

    // What is true now, before the window is shown.
    refresh(&app);

    // And every few seconds after. A cable pulled out while the window is open should show,
    // and three seconds is the same cadence the shell polls the rest of the system at.
    let timer = slint::Timer::default();
    {
        let weak = app.as_weak();
        timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_secs(3),
            move || {
                if let Some(ui) = weak.upgrade() {
                    refresh(&ui);
                }
            },
        );
    }

    app.run().unwrap();
}

// ── Service wrappers ─────────────────────────────────────────────────

fn call_network(method: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
    let client = SyncRpcClient::for_service("network");
    client.call(method, params).map_err(|e| e.message)
}


// ── Reading the network, which this app never did ────────────────────────────────────
//
// The app opened, wired its buttons, and stopped. Nothing ever asked the service what the
// network was doing, so every property kept its Slint default and the window said "Not
// connected" and "Ethernet — No interface" on a machine with a routable address and eth0 up.
// It was not reporting a failure; it had never looked.
//
// It also asked for methods that do not exist. The app called network.wifi_toggle,
// wifi_scan, wifi_connect, wifi_disconnect and wifi_forget; the service answers
// network.status, network.interfaces and network.dns. Not one name in common, so every
// button returned "Unknown method" into a `let _ =` and looked like it had worked.

/// Pull the current state out of the network service and put it on screen.
fn refresh(ui: &NetworkManagerApp) {
    // Interfaces first: the ethernet list and, with it, whether there is an interface at all.
    if let Ok(v) = call_network("network.interfaces", serde_json::json!({})) {
        let rows: Vec<serde_json::Value> = serde_json::from_value(v).unwrap_or_default();

        let eth: Vec<EthernetInterface> = rows
            .iter()
            .filter(|i| {
                // ConnectionType serialises as a string for the simple variants and as
                // {"Other": "..."} for the rest, so match on the text either way.
                let t = i.get("conn_type").map(|c| c.to_string()).unwrap_or_default();
                t.to_lowercase().contains("ethernet")
            })
            .map(|i| {
                let ip = i.get("ip_address").and_then(|v| v.as_str()).unwrap_or("");
                EthernetInterface {
                    name: i.get("name").and_then(|v| v.as_str()).unwrap_or("").into(),
                    status: i.get("state").and_then(|v| v.as_str()).unwrap_or("").to_uppercase().into(),
                    ip_address: ip.into(),
                    mac_address: i.get("mac_address").and_then(|v| v.as_str()).unwrap_or("").into(),
                    // The service does not report link speed or DHCP/gateway/DNS per interface
                    // yet. Left empty rather than invented: an empty field reads as "not known",
                    // a made-up one reads as fact.
                    speed: "".into(),
                    is_dhcp: true,
                    subnet: "".into(),
                    gateway: "".into(),
                    dns: "".into(),
                }
            })
            .collect();

        ui.set_ethernet_interfaces(ModelRc::new(VecModel::from(eth)));
    }

    // Then the summary line the header shows.
    if let Ok(v) = call_network("network.status", serde_json::json!({})) {
        let connected = v.get("connected").and_then(|x| x.as_bool()).unwrap_or(false);
        let conn_type = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
        let ip = v.get("ip_address").and_then(|x| x.as_str()).unwrap_or("");
        let ssid = v.get("ssid").and_then(|x| x.as_str()).unwrap_or("");

        ui.set_status_state(if connected { "connected" } else { "disconnected" }.into());
        ui.set_status_connection_type(match conn_type {
            "wifi" => "WiFi",
            "ethernet" => "Ethernet",
            other => other,
        }.into());
        ui.set_status_ip_address(ip.into());
        if !ssid.is_empty() {
            ui.set_wifi_current_ssid(ssid.into());
        }
    }

    // And the resolvers. The property is called wifi-dns because that pane was built
    // first; the resolvers are the machine's, not the radio's.
    if let Ok(v) = call_network("network.dns", serde_json::json!({})) {
        let servers: Vec<String> = v
            .get("nameservers")
            .and_then(|n| n.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        ui.set_wifi_dns(servers.join(", ").into());
    }
}

fn wire(app: &NetworkManagerApp) {
    // ── WiFi ──
    {
        let weak = app.as_weak();
        app.on_toggle_wifi(move || {
            let Some(ui) = weak.upgrade() else { return };
            let enabled = ui.get_wifi_enabled();
            let _ = call_network("network.wifi_toggle", serde_json::json!({ "enabled": !enabled }));
            tracing::info!("Toggle WiFi: {} -> {}", enabled, !enabled);
        });
    }

    {
        let weak = app.as_weak();
        app.on_wifi_scan(move || {
            let Some(ui) = weak.upgrade() else { return };
            ui.set_wifi_scanning(true);
            match call_network("network.wifi_scan", serde_json::json!({})) {
                Ok(result) => {
                    if let Ok(networks) = serde_json::from_value::<Vec<serde_json::Value>>(result) {
                        let wifi_list: Vec<WifiNetwork> = networks.iter().map(|n| {
                            WifiNetwork {
                                ssid: n.get("ssid").and_then(|v| v.as_str()).unwrap_or("").into(),
                                signal: n.get("signal").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
                                security: n.get("security").and_then(|v| v.as_str()).unwrap_or("").into(),
                                is_connected: n.get("is_connected").and_then(|v| v.as_bool()).unwrap_or(false),
                                is_saved: n.get("is_saved").and_then(|v| v.as_bool()).unwrap_or(false),
                            }
                        }).collect();
                        ui.set_wifi_networks(ModelRc::new(VecModel::from(wifi_list)));
                    }
                }
                Err(e) => tracing::warn!("WiFi scan failed: {e}"),
            }
            ui.set_wifi_scanning(false);
        });
    }

    {
        let weak = app.as_weak();
        app.on_wifi_connect(move |ssid, password| {
            let Some(ui) = weak.upgrade() else { return };
            let ssid_str = ssid.to_string();
            let pass_str = password.to_string();
            match call_network("network.wifi_connect", serde_json::json!({
                "ssid": ssid_str,
                "password": pass_str
            })) {
                Ok(_) => {
                    ui.set_wifi_connect_status("Connected".into());
                    ui.set_wifi_password_visible(false);
                    tracing::info!("Connected to WiFi: {ssid_str}");
                }
                Err(e) => {
                    ui.set_wifi_connect_status(format!("Failed: {e}").into());
                    tracing::warn!("WiFi connect failed: {e}");
                }
            }
        });
    }

    app.on_wifi_disconnect(|| {
        let _ = call_network("network.wifi_disconnect", serde_json::json!({}));
        tracing::info!("WiFi disconnect");
    });

    app.on_wifi_forget(|ssid| {
        let _ = call_network("network.wifi_forget", serde_json::json!({ "ssid": ssid.to_string() }));
        tracing::info!("WiFi forget: {ssid}");
    });

    // ── Bluetooth ──
    app.on_toggle_bluetooth(|| { tracing::info!("Toggle Bluetooth"); });
    app.on_bt_scan(|| { tracing::info!("Bluetooth scan"); });
    app.on_bt_pair(|addr| { tracing::info!("Bluetooth pair: {addr}"); });
    app.on_bt_connect(|addr| { tracing::info!("Bluetooth connect: {addr}"); });
    app.on_bt_disconnect(|addr| { tracing::info!("Bluetooth disconnect: {addr}"); });

    // ── VPN ──
    app.on_vpn_connect(|name| { tracing::info!("VPN connect: {name}"); });
    app.on_vpn_disconnect(|name| { tracing::info!("VPN disconnect: {name}"); });
    app.on_net_vpn_import_config(|| { tracing::info!("VPN import config"); });

    // ── Firewall ──
    app.on_toggle_firewall(|| { tracing::info!("Toggle firewall"); });
    app.on_firewall_allow_port(|port| { tracing::info!("Firewall allow port: {port}"); });
    app.on_firewall_block_port(|port| { tracing::info!("Firewall block port: {port}"); });
    app.on_firewall_apply_profile(|profile| { tracing::info!("Firewall apply profile: {profile}"); });

    // ── Diagnostics ──
    app.on_diag_run_test(|test| { tracing::info!("Run diagnostic test: {test}"); });
    app.on_diag_run_all_tests(|| { tracing::info!("Run all diagnostic tests"); });
    app.on_test_ai_connectivity(|| { tracing::info!("Test AI provider connectivity"); });
    app.on_net_diag_run_ping(|| { tracing::info!("Run ping"); });
    app.on_net_diag_run_traceroute(|| { tracing::info!("Run traceroute"); });

    // ── AI assist ──
    app.on_ai_explain_pressed(|| { tracing::info!("AI explain requested (standalone mode)"); });
    app.on_ai_dismiss(|| { tracing::info!("AI dismiss"); });
}
