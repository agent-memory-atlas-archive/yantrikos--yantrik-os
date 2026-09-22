//! About screen wiring — populates system info fields at startup and keeps
//! the one field that moves (uptime) fresh for as long as the shell runs.

use slint::{ComponentHandle, Timer, TimerMode};

use crate::app_context::AppContext;
use crate::App;

pub fn wire(ui: &App, _ctx: &AppContext) {
    let ui_weak = ui.as_weak();
    populate_about_info(&ui_weak);

    // Kernel, CPU, RAM and disk are facts about the machine that a startup read
    // states correctly for the whole session. Uptime is not: the shell starts at
    // boot, so the one-time read said "0m" and the screen repeated it on a
    // machine up for hours. Refresh just that field, finer than the minutes it
    // displays.
    let weak = ui.as_weak();
    let uptime_timer = Timer::default();
    uptime_timer.start(TimerMode::Repeated, std::time::Duration::from_secs(30), move || {
        if let Some(ui) = weak.upgrade() {
            ui.set_about_uptime(read_uptime().into());
        }
    });
    std::mem::forget(uptime_timer);
}

fn populate_about_info(ui_weak: &slint::Weak<App>) {
    let Some(ui) = ui_weak.upgrade() else { return };

    // Hostname from /etc/hostname
    let hostname = std::fs::read_to_string("/etc/hostname")
        .unwrap_or_else(|_| "unknown".to_string())
        .trim()
        .to_string();
    ui.set_about_hostname(hostname.into());

    // Kernel version from /proc/version (third whitespace-delimited field)
    let kernel = std::fs::read_to_string("/proc/version")
        .ok()
        .and_then(|v| v.split_whitespace().nth(2).map(|s| s.to_string()))
        .unwrap_or_else(|| "unknown".to_string());
    ui.set_about_kernel(kernel.into());

    // CPU model name from /proc/cpuinfo (trimmed for display)
    let cpu = std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|c| {
            c.lines()
                .find(|l| l.starts_with("model name"))
                .and_then(|l| l.split(':').nth(1))
                .map(|s| {
                    s.trim()
                        .replace("(R)", "")
                        .replace("(TM)", "")
                        .replace("  ", " ")
                        .replace(" Processor", "")
                        .trim()
                        .to_string()
                })
        })
        .unwrap_or_else(|| "unknown".to_string());
    ui.set_about_cpu(cpu.into());

    // RAM total from /proc/meminfo
    let ram = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|m| {
            m.lines()
                .find(|l| l.starts_with("MemTotal:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|kb| kb.parse::<u64>().ok())
                .map(|kb| {
                    let gb = kb as f64 / 1024.0 / 1024.0;
                    format!("{:.1} GB", gb)
                })
        })
        .unwrap_or_else(|| "unknown".to_string());
    ui.set_about_ram(ram.into());

    // Disk info via df command
    // Try GNU df first (--output), fall back to plain df -h (BusyBox/Alpine)
    let disk = std::process::Command::new("df")
        .args(["-h", "--output=size,avail", "/"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| {
            let s = String::from_utf8_lossy(&out.stdout);
            let line = s.lines().nth(1)?;
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                Some(format!("{} free of {}", parts[1], parts[0]))
            } else {
                None
            }
        })
        .or_else(|| {
            // Fallback: plain `df -h /` — columns: Filesystem Size Used Avail Use% Mounted
            std::process::Command::new("df")
                .args(["-h", "/"])
                .output()
                .ok()
                .and_then(|out| {
                    let s = String::from_utf8_lossy(&out.stdout);
                    let line = s.lines().nth(1)?;
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 4 {
                        // parts[1]=Size, parts[3]=Avail
                        Some(format!("{} free of {}", parts[3], parts[1]))
                    } else {
                        None
                    }
                })
        })
        .unwrap_or_else(|| "unknown".to_string());
    ui.set_about_disk(disk.into());

    // Uptime from /proc/uptime — also re-read on a timer, see wire()
    ui.set_about_uptime(read_uptime().into());

    // Version from Cargo.toml + git hash
    let version = match option_env!("GIT_HASH") {
        Some(hash) => format!("{} ({})", env!("CARGO_PKG_VERSION"), hash),
        None => env!("CARGO_PKG_VERSION").to_string(),
    };
    ui.set_about_version(version.into());

    // Build date from build.rs
    let build_date = option_env!("BUILD_DATE").unwrap_or("unknown");
    ui.set_about_build_date(build_date.into());
}

/// The uptime field: /proc/uptime, formatted for display.
///
/// Shared with the System screen's uptime row, which had a second copy of this
/// that dropped the minutes — so About said "3d 1h 2m" where System said
/// "3d 1h" about the same boot.
pub(crate) fn read_uptime() -> String {
    std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|u| parse_uptime_secs(&u))
        .map(format_uptime)
        .unwrap_or_else(|| "\u{2014}".to_string())
}

/// The first field of /proc/uptime is seconds since boot.
fn parse_uptime_secs(proc_uptime: &str) -> Option<u64> {
    proc_uptime
        .split_whitespace()
        .next()
        .and_then(|s| s.parse::<f64>().ok())
        .map(|secs| secs as u64)
}

pub(crate) fn format_uptime(total_secs: u64) -> String {
    let days = total_secs / 86400;
    let hours = (total_secs % 86400) / 3600;
    let mins = (total_secs % 3600) / 60;
    if days > 0 {
        format!("{}d {}h {}m", days, hours, mins)
    } else if hours > 0 {
        format!("{}h {}m", hours, mins)
    } else {
        format!("{}m", mins)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_first_field_of_proc_uptime() {
        // The real file: seconds since boot, then idle seconds. This sample
        // was captured from the live machine while its About screen claimed
        // "0m" — it had been up 9h49m.
        assert_eq!(parse_uptime_secs("35374.59 70597.24\n"), Some(35374));
        assert_eq!(parse_uptime_secs("0.19 0.08\n"), Some(0));
        assert_eq!(parse_uptime_secs(""), None);
        assert_eq!(parse_uptime_secs("garbage 1.0"), None);
    }

    #[test]
    fn formats_known_durations() {
        assert_eq!(format_uptime(0), "0m");
        assert_eq!(format_uptime(59), "0m");
        assert_eq!(format_uptime(60), "1m");
        assert_eq!(format_uptime(3599), "59m");
        assert_eq!(format_uptime(3600), "1h 0m");
        assert_eq!(format_uptime(35374), "9h 49m");
        assert_eq!(format_uptime(90061), "1d 1h 1m");
        // Three days into the same boot, read off the machine again while the
        // System screen was showing "3d 1h" for it from its own formatter.
        assert_eq!(format_uptime(262922), "3d 1h 2m");
    }
}
