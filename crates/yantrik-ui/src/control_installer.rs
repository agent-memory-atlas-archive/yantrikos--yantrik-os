//! The installer describes itself, and can be filled in and run without a mouse.
//!
//! The first-run wizard was the one screen in the OS that only a human could operate. Everything
//! else publishes `app.describe` / `app.act`; onboarding published the single word "onboarding"
//! and nothing more. So an agent handed a freshly booted machine had exactly one option, and it
//! is the option we built this control surface to kill: photograph the screen, guess at pixel
//! coordinates, aim synthetic clicks, photograph it again to find out what happened.
//!
//! That is not a hypothetical. Driving this installer through the QEMU monitor took three hours
//! and never typed a character, because the harness was moving a pointer QEMU had not selected
//! and there was no way to tell from the outside — no caret, no focus, no state to read. A
//! surface that says what the form holds would have answered it in one call.
//!
//! What is deliberately *not* here: filling the form does not install anything. `install` is a
//! separate, `dangerous` action, because it repartitions a disk. Reading is free; erasing is not.

use slint::ComponentHandle;
use yantrik_app_runtime::control::{Action, App as ControlSurface, Param};

use crate::App;

/// The phases of the wizard, named so a caller can navigate by intent rather than by number.
///
/// 0-2 are the opening animation and 11-12 are consequences of installing, so none of them are
/// places to jump to; they are reported by `describe` but refused by `go_to`.
const STEPS: &[(&str, i32)] = &[
    ("identity", 3),
    ("interests", 4),
    ("location", 5),
    ("hardware", 6),
    ("ai-mode", 7),
    ("ai-provider", 8),
    ("ai-test", 9),
    ("summary", 10),
];

/// What the wizard is showing, in words.
pub fn step_name(phase: i32) -> &'static str {
    match phase {
        0 | 1 => "waking",
        2 => "greeting",
        11 => "installing",
        12 => "installed",
        other => STEPS
            .iter()
            .find(|(_, id)| *id == other)
            .map(|(name, _)| *name)
            .unwrap_or("unknown"),
    }
}

/// The fields a caller may write, and the property each one lands in.
///
/// Named for what the screen calls them, not for the Slint property: the label above the box
/// reads "Computer Name", so `hostname` and `computer_name` both work rather than only the one
/// that happens to match our internal spelling.
const FIELDS: &[&str] = &[
    "full_name",
    "username",
    "password",
    "password_confirm",
    "hostname",
    "companion_name",
    "location",
    "disk",
];

/// The disks the installer found, as a list rather than three numbered slots.
fn disks(ui: &App) -> Vec<serde_json::Value> {
    let slots = [
        (
            ui.get_onboard_disk_1_name(),
            ui.get_onboard_disk_1_size(),
            ui.get_onboard_disk_1_model(),
        ),
        (
            ui.get_onboard_disk_2_name(),
            ui.get_onboard_disk_2_size(),
            ui.get_onboard_disk_2_model(),
        ),
        (
            ui.get_onboard_disk_3_name(),
            ui.get_onboard_disk_3_size(),
            ui.get_onboard_disk_3_model(),
        ),
    ];
    slots
        .iter()
        .filter(|(name, _, _)| !name.is_empty())
        .map(|(name, size, model)| {
            serde_json::json!({
                "name": name.to_string(),
                "size": size.to_string(),
                "model": model.to_string(),
            })
        })
        .collect()
}

/// Why the machine cannot be installed yet, or `None` when it can.
///
/// The same checks the Next button and the Install button make, in the same order, so a caller
/// reading `blocked_by` learns exactly what a person would learn by clicking and being refused.
pub fn blocked_by(ui: &App) -> Option<String> {
    if !ui.get_onboard_installer_mode() {
        return Some("this is not the installer — the machine is already installed".into());
    }
    if ui.get_onboard_input_username().trim().is_empty() {
        return Some("username is required".into());
    }
    if ui.get_onboard_input_password().is_empty() {
        return Some("password is required".into());
    }
    if ui.get_onboard_input_password() != ui.get_onboard_input_password_confirm() {
        return Some("passwords do not match".into());
    }
    if ui.get_onboard_selected_disk().trim().is_empty() {
        let found = disks(ui);
        return Some(if found.is_empty() {
            "no disk to install to was found".into()
        } else {
            let names: Vec<String> = found
                .iter()
                .filter_map(|d| d["name"].as_str().map(String::from))
                .collect();
            format!("no disk chosen; this machine has: {}", names.join(", "))
        });
    }
    None
}

/// What the installer is showing right now.
///
/// Passwords travel as `true`/`false`, never as text. A control surface is read by whatever is
/// driving the machine and, on this path, written to a transcript; a password that reaches a
/// transcript is a password that has to be changed.
pub fn state(ui: &App) -> serde_json::Value {
    let phase = ui.get_onboard_phase();
    let pw = ui.get_onboard_input_password();
    let pw2 = ui.get_onboard_input_password_confirm();

    serde_json::json!({
        "installer_mode": ui.get_onboard_installer_mode(),
        "step": step_name(phase),
        "phase": phase,
        "fields": {
            "full_name": ui.get_onboard_input_full_name().to_string(),
            "username": ui.get_onboard_input_username().to_string(),
            "hostname": ui.get_onboard_input_hostname().to_string(),
            "companion_name": ui.get_onboard_input_companion().to_string(),
            "location": ui.get_onboard_input_location().to_string(),
            "password_set": !pw.is_empty(),
            "password_confirm_set": !pw2.is_empty(),
            "passwords_match": pw == pw2,
        },
        "disks": disks(ui),
        "selected_disk": ui.get_onboard_selected_disk().to_string(),
        "hardware": {
            "cpu": { "ok": ui.get_onboard_ai_hw_cpu_ok(), "detail": ui.get_onboard_ai_hw_cpu_label().to_string() },
            "ram": { "ok": ui.get_onboard_ai_hw_ram_ok(), "detail": ui.get_onboard_ai_hw_ram_label().to_string() },
            "gpu": { "ok": ui.get_onboard_ai_hw_gpu_ok(), "detail": ui.get_onboard_ai_hw_gpu_label().to_string() },
            "disk": { "ok": ui.get_onboard_ai_hw_disk_ok(), "detail": ui.get_onboard_ai_hw_disk_label().to_string() },
            "network_ok": ui.get_onboard_ai_hw_network_ok(),
            "runtime_ok": ui.get_onboard_ai_hw_runtime_ok(),
            "recommends": ui.get_onboard_ai_hw_recommend().to_string(),
        },
        "ai_test": {
            "status": ui.get_onboard_ai_test_status().to_string(),
            "model": ui.get_onboard_ai_test_model().to_string(),
            "latency_ms": ui.get_onboard_ai_test_latency_ms(),
            "privacy": ui.get_onboard_ai_test_privacy().to_string(),
        },
        "installing": ui.get_onboard_installing(),
        "progress": ui.get_onboard_install_progress(),
        "status": ui.get_onboard_install_status().to_string(),
        "error": ui.get_onboard_install_error().to_string(),
        "blocked_by": blocked_by(ui).map(serde_json::Value::String).unwrap_or(serde_json::Value::Null),
        "steps": STEPS.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
        "fields_writable": FIELDS,
    })
}

/// The line worth reading first when the machine is sitting in the wizard.
pub fn summary(ui: &App) -> String {
    let step = step_name(ui.get_onboard_phase());
    if !ui.get_onboard_installer_mode() {
        return format!("Yantrik — first-run setup, on the {step} step");
    }
    // Finished is checked first. The flag and the phase are set by different threads a beat
    // apart, and reading them the other way round reports "installing, 100%" to anyone who
    // asks in between — a state that sounds like work still to do.
    if ui.get_onboard_phase() == 12 {
        return "Yantrik installer — installed, waiting to reboot".into();
    }
    if ui.get_onboard_installing() {
        return format!(
            "Yantrik installer — installing, {}% — {}",
            ui.get_onboard_install_progress(),
            ui.get_onboard_install_status()
        );
    }
    match blocked_by(ui) {
        Some(why) => format!("Yantrik installer — on the {step} step, not ready to install: {why}"),
        None => format!(
            "Yantrik installer — on the {step} step, ready to install to {}",
            ui.get_onboard_selected_disk()
        ),
    }
}

/// Add the installer's actions to the shell's control surface, on machines that have an
/// installer.
///
/// An installed desktop has no disk to install to and no wizard to fill in, and advertising
/// `installer_install` there — marked `dangerous`, offering to erase a disk — is worse than
/// useless: every caller that lists the shell's actions has to read it and work out that it
/// would refuse. The live image sets `installer-mode`; nothing else does.
pub fn actions(surface: ControlSurface, ui: &App) -> ControlSurface {
    if !ui.get_onboard_installer_mode() {
        return surface;
    }
    let ui_for = {
        let weak = ui.as_weak();
        move || weak.upgrade().ok_or_else(|| "the shell is gone".to_string())
    };
    let set_ui = ui_for.clone();
    let goto_ui = ui_for.clone();
    let install_ui = ui_for.clone();
    let reboot_ui = ui_for;

    surface
        .action(
            Action::new(
                "installer_set",
                "Fill in one field of the first-run wizard, as if typed into it",
            )
            .arg(Param::text("field").describe(
                "full_name, username, password, password_confirm, hostname, companion_name, location, disk",
            ))
            .arg(Param::text("value")),
            move |args| {
                let ui = set_ui()?;
                let field = args["field"].as_str().unwrap_or_default().trim().to_lowercase();
                let value = args["value"].as_str().unwrap_or_default().to_string();

                // Aliases for what the screen calls things, so a caller reading the labels off a
                // description does not have to guess our internal spelling.
                let field = match field.as_str() {
                    "computer_name" | "computer" => "hostname",
                    "name" => "full_name",
                    "companion" => "companion_name",
                    "target_disk" | "target" => "disk",
                    other => other,
                }
                .to_string();

                match field.as_str() {
                    "full_name" => ui.set_onboard_input_full_name(value.clone().into()),
                    "username" => ui.set_onboard_input_username(value.clone().into()),
                    "password" => ui.set_onboard_input_password(value.clone().into()),
                    "password_confirm" => {
                        ui.set_onboard_input_password_confirm(value.clone().into())
                    }
                    "hostname" => ui.set_onboard_input_hostname(value.clone().into()),
                    "companion_name" => ui.set_onboard_input_companion(value.clone().into()),
                    "location" => ui.set_onboard_input_location(value.clone().into()),
                    "disk" => {
                        // A typo here costs a disk, so the name has to be one the installer
                        // actually found rather than anything the caller cares to type.
                        let found = disks(&ui);
                        let names: Vec<String> = found
                            .iter()
                            .filter_map(|d| d["name"].as_str().map(String::from))
                            .collect();
                        let want = value.trim().trim_start_matches("/dev/");
                        if !names.iter().any(|n| n == want) {
                            return Err(if names.is_empty() {
                                "the installer found no disks on this machine".to_string()
                            } else {
                                format!("no disk `{want}` here; it found: {}", names.join(", "))
                            });
                        }
                        ui.set_onboard_selected_disk(want.into());
                    }
                    other => {
                        return Err(format!(
                            "no field `{other}` in the wizard; it takes: {}",
                            FIELDS.join(", ")
                        ))
                    }
                }

                // A password that went in echoes back as a flag, never as itself.
                let shown = if field.starts_with("password") {
                    serde_json::Value::Bool(!value.is_empty())
                } else {
                    serde_json::Value::String(value)
                };
                Ok(serde_json::json!({
                    "field": field,
                    "value": shown,
                    "blocked_by": blocked_by(&ui).map(serde_json::Value::String)
                        .unwrap_or(serde_json::Value::Null),
                }))
            },
        )
        .action(
            Action::new("installer_go_to", "Show a different step of the first-run wizard")
                .arg(Param::text("step").describe(
                    "identity, interests, location, hardware, ai-mode, ai-provider, ai-test, summary",
                )),
            move |args| {
                let ui = goto_ui()?;
                let want = args["step"].as_str().unwrap_or_default().trim().to_lowercase();
                let phase = STEPS
                    .iter()
                    .find(|(name, _)| *name == want)
                    .map(|(_, id)| *id)
                    .ok_or_else(|| {
                        let names: Vec<&str> = STEPS.iter().map(|(n, _)| *n).collect();
                        format!("no step called `{want}`; there is: {}", names.join(", "))
                    })?;
                if ui.get_onboard_installing() {
                    return Err("an install is running; the wizard cannot be moved".into());
                }
                ui.set_onboard_phase(phase);
                Ok(serde_json::json!({ "step": want }))
            },
        )
        .action(
            // Deferred and dangerous, and both words are meant. It repartitions the chosen disk
            // and everything on it is gone; and it returns the moment the work is handed to the
            // installer thread, which then reports through `progress` and `status` for several
            // minutes. A caller that treats the reply as "installed" is wrong on both counts.
            Action::new(
                "installer_install",
                "Erase the chosen disk and install Yantrik OS onto it",
            )
            .risk("dangerous")
            .defers(),
            move |_| {
                let ui = install_ui()?;
                if ui.get_onboard_installing() {
                    return Err("an install is already running".into());
                }
                // The form's own checks, run before we promise anything, so a refusal names the
                // field rather than failing halfway through partitioning.
                if let Some(why) = blocked_by(&ui) {
                    return Err(why);
                }
                let disk = ui.get_onboard_selected_disk();
                let companion = {
                    let typed = ui.get_onboard_input_companion();
                    if typed.is_empty() { ui.get_settings_companion_name() } else { typed }
                };
                ui.set_onboard_install_error(Default::default());
                ui.set_onboard_installing(true);
                ui.invoke_onboard_install_to_disk(
                    ui.get_onboard_input_username(),
                    ui.get_onboard_input_password(),
                    ui.get_onboard_input_full_name(),
                    ui.get_onboard_input_hostname(),
                    companion,
                    disk.clone(),
                );
                ui.set_onboard_phase(11);
                Ok(serde_json::json!({
                    "installing_to": disk.to_string(),
                    "watch": "describe the shell and read installer.progress and installer.status",
                }))
            },
        )
        .action(
            Action::new("installer_reboot", "Reboot into the installed system")
                .risk("dangerous"),
            move |_| {
                let ui = reboot_ui()?;
                if ui.get_onboard_phase() != 12 {
                    return Err(format!(
                        "the install has not finished — the wizard is on the {} step",
                        step_name(ui.get_onboard_phase())
                    ));
                }
                ui.invoke_onboard_install_reboot();
                Ok(serde_json::json!({ "rebooting": true }))
            },
        )
}
