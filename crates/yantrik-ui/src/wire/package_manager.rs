//! Package Manager wire module — list, search, info, install, remove, upgrade.
//!
//! The commands and their parsers live in `wire::apt`; this module is the screen's plumbing.
//! It used to shell out to `apk` directly, which is Alpine's package manager. This OS is
//! Debian — `apk` is not installed — so the one screen whose purpose is installing software
//! could not install software, and the failure was invisible because every call site treated
//! "could not run apk" as a non-fatal empty result.
//!
//! All heavy operations run in background threads.
//! UI is updated via Slint Timers polling oneshot channels.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use slint::{ComponentHandle, ModelRc, Timer, TimerMode, VecModel};

use crate::app_context::AppContext;
use crate::{App, PackageData};

/// One row of the package list, as the screen models it.
#[derive(Clone, Debug)]
struct PkgEntry {
    name: String,
    version: String,
    description: String,
    installed: bool,
    upgradable: bool,
    size_text: String,
    repo: String,
}

/// Everything the detail pane shows about one package.
#[derive(Clone, Debug, Default)]
struct PkgDetail {
    name: String,
    version: String,
    description: String,
    maintainer: String,
    dependencies: String,
    size: String,
    repo: String,
    installed: bool,
    upgradable: bool,
}

/// Wire package manager callbacks.
pub fn wire(ui: &App, ctx: &AppContext) {
    let pkg_cache: Rc<RefCell<Vec<PkgEntry>>> = Rc::new(RefCell::new(Vec::new()));
    let poll_timer: Rc<RefCell<Option<Timer>>> = Rc::new(RefCell::new(None));

    // ── Search callback ──
    {
        let ui_weak = ui.as_weak();
        let cache = pkg_cache.clone();
        ui.on_pkg_search(move |query| {
            let query_str = query.to_string();
            let cache_ref = cache.borrow();

            if query_str.is_empty() {
                // Show all cached packages
                if let Some(ui) = ui_weak.upgrade() {
                    let items = cache_to_model(&cache_ref);
                    ui.set_pkg_packages(ModelRc::new(VecModel::from(items)));
                    ui.set_pkg_status_text(
                        format!("{} packages", cache_ref.len()).into(),
                    );
                }
                return;
            }

            // Filter from cache
            let lower = query_str.to_lowercase();
            let filtered: Vec<&PkgEntry> = cache_ref
                .iter()
                .filter(|p| {
                    p.name.to_lowercase().contains(&lower)
                        || p.description.to_lowercase().contains(&lower)
                })
                .collect();

            if let Some(ui) = ui_weak.upgrade() {
                let items: Vec<PackageData> = filtered
                    .iter()
                    .map(|p| pkg_to_model(p))
                    .collect();
                let count = items.len();
                ui.set_pkg_packages(ModelRc::new(VecModel::from(items)));
                ui.set_pkg_status_text(
                    format!("{} packages matching '{}'", count, query_str).into(),
                );
                ui.set_pkg_selected_index(-1);
                clear_detail(&ui);
            }
        });
    }

    // ── Filter changed callback ──
    {
        let ui_weak = ui.as_weak();
        let cache = pkg_cache.clone();
        ui.on_pkg_filter_changed(move |filter_idx| {
            let cache_ref = cache.borrow();
            let filtered: Vec<&PkgEntry> = match filter_idx {
                1 => cache_ref.iter().filter(|p| p.installed).collect(),
                2 => cache_ref.iter().filter(|p| p.upgradable).collect(),
                _ => cache_ref.iter().collect(),
            };

            if let Some(ui) = ui_weak.upgrade() {
                let items: Vec<PackageData> = filtered
                    .iter()
                    .map(|p| pkg_to_model(p))
                    .collect();
                let label = match filter_idx {
                    1 => "installed",
                    2 => "upgradable",
                    _ => "total",
                };
                ui.set_pkg_status_text(
                    format!("{} {} packages", items.len(), label).into(),
                );
                ui.set_pkg_packages(ModelRc::new(VecModel::from(items)));
                ui.set_pkg_selected_index(-1);
                clear_detail(&ui);
            }
        });
    }

    // ── Refresh callback — loads installed + upgradable in background ──
    {
        let ui_weak = ui.as_weak();
        let timer_ref = poll_timer.clone();
        let cache = pkg_cache.clone();
        ui.on_pkg_refresh(move || {
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_pkg_is_loading(true);
                ui.set_pkg_status_text("Updating package database...".into());
                ui.set_pkg_error_text("".into());
            }

            let (tx, rx) = mpsc::channel::<Result<Vec<PkgEntry>, String>>();

            // Background thread: refresh the index, then list installed and upgradable.
            std::thread::spawn(move || {
                // Refreshing the index is best-effort: it needs the network, and a machine that
                // is offline should still be able to see and remove what it already has.
                let update = crate::wire::apt::update_command();
                if let Some((bin, args)) = update.split_first() {
                    let _ = std::process::Command::new(bin).args(args).output();
                }

                let installed = match crate::wire::apt::list_installed() {
                    Ok(list) => list,
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        return;
                    }
                };

                // Which of them have something newer waiting. Not fatal if it fails: a machine
                // that has never refreshed its index simply has nothing to report.
                let upgradable = crate::wire::apt::list_upgradable();

                let mut packages: Vec<PkgEntry> = installed
                    .into_iter()
                    .map(|p| PkgEntry {
                        upgradable: upgradable.iter().any(|u| *u == p.name),
                        name: p.name,
                        version: p.version,
                        description: p.description,
                        installed: p.installed,
                        size_text: p.size_text,
                        repo: p.repo,
                    })
                    .collect();

                packages.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
                let _ = tx.send(Ok(packages));
            });

            // Poll for result
            let weak = ui_weak.clone();
            let handle = timer_ref.clone();
            let cache_inner = cache.clone();
            let timer = Timer::default();
            timer.start(TimerMode::Repeated, Duration::from_millis(50), move || {
                if let Ok(result) = rx.try_recv() {
                    if let Some(ui) = weak.upgrade() {
                        match result {
                            Ok(packages) => {
                                let count = packages.len();
                                let upgradable = packages.iter().filter(|p| p.upgradable).count();
                                let items = cache_to_model(&packages);
                                *cache_inner.borrow_mut() = packages;
                                ui.set_pkg_packages(ModelRc::new(VecModel::from(items)));
                                ui.set_pkg_upgradable_count(upgradable as i32);
                                ui.set_pkg_status_text(
                                    format!("{} installed packages, {} upgradable", count, upgradable).into(),
                                );
                                ui.set_pkg_is_loading(false);
                            }
                            Err(err) => {
                                ui.set_pkg_error_text(err.into());
                                ui.set_pkg_is_loading(false);
                                ui.set_pkg_status_text("Error loading packages".into());
                            }
                        }
                    }
                    *handle.borrow_mut() = None;
                }
            });
            *timer_ref.borrow_mut() = Some(timer);
        });
    }

    // ── Select package — fetch details in background ──
    {
        let ui_weak = ui.as_weak();
        let timer_ref = poll_timer.clone();
        let cache = pkg_cache.clone();
        ui.on_pkg_select_package(move |idx| {
            let cache_ref = cache.borrow();
            let active_filter = if let Some(ui) = ui_weak.upgrade() {
                ui.get_pkg_active_filter()
            } else {
                return;
            };

            // Resolve the actual package from filtered view
            let filtered: Vec<&PkgEntry> = match active_filter {
                1 => cache_ref.iter().filter(|p| p.installed).collect(),
                2 => cache_ref.iter().filter(|p| p.upgradable).collect(),
                _ => cache_ref.iter().collect(),
            };

            let pkg = match filtered.get(idx as usize) {
                Some(p) => (*p).clone(),
                None => return,
            };

            let pkg_name = pkg.name.clone();

            // Set basic detail immediately from cache
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_pkg_detail_name(pkg.name.clone().into());
                ui.set_pkg_detail_version(pkg.version.clone().into());
                ui.set_pkg_detail_description(pkg.description.clone().into());
                ui.set_pkg_detail_installed(pkg.installed);
                ui.set_pkg_detail_upgradable(pkg.upgradable);
                ui.set_pkg_detail_size(pkg.size_text.clone().into());
                ui.set_pkg_detail_repo(pkg.repo.clone().into());
                ui.set_pkg_detail_maintainer("".into());
                ui.set_pkg_detail_dependencies("".into());
            }

            // Fetch full detail in background
            let (tx, rx) = mpsc::channel::<PkgDetail>();

            std::thread::spawn(move || {
                let detail = fetch_pkg_detail(&pkg_name, pkg.installed, pkg.upgradable);
                let _ = tx.send(detail);
            });

            let weak = ui_weak.clone();
            let handle = timer_ref.clone();
            let timer = Timer::default();
            timer.start(TimerMode::Repeated, Duration::from_millis(50), move || {
                if let Ok(detail) = rx.try_recv() {
                    if let Some(ui) = weak.upgrade() {
                        ui.set_pkg_detail_description(detail.description.into());
                        ui.set_pkg_detail_maintainer(detail.maintainer.into());
                        ui.set_pkg_detail_dependencies(detail.dependencies.into());
                        if !detail.size.is_empty() {
                            ui.set_pkg_detail_size(detail.size.into());
                        }
                        if !detail.repo.is_empty() {
                            ui.set_pkg_detail_repo(detail.repo.into());
                        }
                    }
                    *handle.borrow_mut() = None;
                }
            });
            *timer_ref.borrow_mut() = Some(timer);
        });
    }

    // ── Install package ──
    {
        let ui_weak = ui.as_weak();
        let timer_ref = poll_timer.clone();
        ui.on_pkg_install_package(move |name| {
            let pkg_name = name.to_string();
            run_pkg_action(
                &ui_weak,
                &timer_ref,
                pkg_name.clone(),
                "install",
                &crate::wire::apt::install_command(&pkg_name),
            );
        });
    }

    // ── Remove package ──
    {
        let ui_weak = ui.as_weak();
        let timer_ref = poll_timer.clone();
        ui.on_pkg_remove_package(move |name| {
            let pkg_name = name.to_string();
            run_pkg_action(
                &ui_weak,
                &timer_ref,
                pkg_name.clone(),
                "remove",
                &crate::wire::apt::remove_command(&pkg_name),
            );
        });
    }

    // ── Upgrade single package ──
    {
        let ui_weak = ui.as_weak();
        let timer_ref = poll_timer.clone();
        ui.on_pkg_upgrade_package(move |name| {
            let pkg_name = name.to_string();
            run_pkg_action(
                &ui_weak,
                &timer_ref,
                pkg_name.clone(),
                "upgrade",
                &crate::wire::apt::upgrade_one_command(&pkg_name),
            );
        });
    }

    // ── Upgrade all ──
    {
        let ui_weak = ui.as_weak();
        let timer_ref = poll_timer.clone();
        ui.on_pkg_upgrade_all(move || {
            run_pkg_action(
                &ui_weak,
                &timer_ref,
                "all packages".to_string(),
                "upgrade",
                &crate::wire::apt::upgrade_all_command(),
            );
        });
    }

    // ── Apply changes (currently not batched — direct actions) ──
    {
        ui.on_pkg_apply_changes(move || {
            // Actions are applied immediately in this implementation
        });
    }

    // ── Cancel changes ──
    {
        ui.on_pkg_cancel_changes(move || {
            // No-op in direct-action mode
        });
    }

    // ── AI Explain callback (explain selected package) ──
    let bridge = ctx.bridge.clone();
    let ai_state = super::ai_assist::AiAssistState::new();
    let ui_weak = ui.as_weak();
    let ai_st = ai_state.clone();
    ui.on_pkg_ai_explain(move || {
        let Some(ui) = ui_weak.upgrade() else { return };
        let name = ui.get_pkg_detail_name().to_string();
        if name.is_empty() { return; }

        let version = ui.get_pkg_detail_version().to_string();
        let desc = ui.get_pkg_detail_description().to_string();
        let deps = ui.get_pkg_detail_dependencies().to_string();
        let size = ui.get_pkg_detail_size().to_string();

        let info = format!(
            "Version: {}\nDescription: {}\nSize: {}\nDependencies: {}",
            version, desc, size, deps
        );
        let prompt = super::ai_assist::package_explain_prompt(&name, &info);

        super::ai_assist::ai_request(
            &ui.as_weak(),
            &bridge,
            &ai_st,
            super::ai_assist::AiAssistRequest {
                prompt,
                timeout_secs: 30,
                set_working: Box::new(|ui, v| ui.set_pkg_ai_is_working(v)),
                set_response: Box::new(|ui, s| ui.set_pkg_ai_response(s.into())),
                get_response: Box::new(|ui| ui.get_pkg_ai_response().to_string()),
            },
        );
    });

    // ── AI Intent callback (natural language → package suggestion) ──
    let bridge2 = ctx.bridge.clone();
    let ai_st2 = ai_state.clone();
    let ui_weak = ui.as_weak();
    ui.on_pkg_ai_intent(move |query| {
        let intent = query.to_string();
        if intent.is_empty() { return; }

        let prompt = super::ai_assist::intent_to_package_prompt(&intent);

        super::ai_assist::ai_request(
            &ui_weak,
            &bridge2,
            &ai_st2,
            super::ai_assist::AiAssistRequest {
                prompt,
                timeout_secs: 30,
                set_working: Box::new(|ui, v| ui.set_pkg_ai_is_working(v)),
                set_response: Box::new(|ui, s| ui.set_pkg_ai_response(s.into())),
                get_response: Box::new(|ui| ui.get_pkg_ai_response().to_string()),
            },
        );
    });

    // ── AI Dismiss ──
    let ui_weak = ui.as_weak();
    ui.on_pkg_ai_dismiss(move || {
        if let Some(ui) = ui_weak.upgrade() {
            ui.set_pkg_ai_panel_open(false);
        }
    });
}

/// Run a package action (install/remove/upgrade) in a background thread.
fn run_pkg_action(
    ui_weak: &slint::Weak<App>,
    timer_ref: &Rc<RefCell<Option<Timer>>>,
    pkg_name: String,
    action: &str,
    // Owned, because the commands are now built rather than written as literals: `wire::apt`
    // composes them so the sudo prefix and DEBIAN_FRONTEND live in one place with tests.
    args: &[String],
) {
    if let Some(ui) = ui_weak.upgrade() {
        ui.set_pkg_is_applying(true);
        ui.set_pkg_status_text(
            format!("{}ing {}...", capitalize(action), pkg_name).into(),
        );
        ui.set_pkg_error_text("".into());
    }

    let action_str = action.to_string();
    let args_owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let (tx, rx) = mpsc::channel::<Result<String, String>>();

    std::thread::spawn(move || {
        if args_owned.is_empty() {
            let _ = tx.send(Err("No command".to_string()));
            return;
        }
        let cmd = &args_owned[0];
        let cmd_args = &args_owned[1..];

        match std::process::Command::new(cmd).args(cmd_args).output() {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let _ = tx.send(Ok(stdout));
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let msg = if stderr.contains("Permission denied") || stderr.contains("not permitted") {
                    "Requires root privileges. Configure sudo/doas for the yantrik user.".to_string()
                } else {
                    format!("{} {}", stdout.trim(), stderr.trim())
                };
                let _ = tx.send(Err(msg));
            }
            Err(e) => {
                let _ = tx.send(Err(format!("Failed to execute: {}", e)));
            }
        }
    });

    let weak = ui_weak.clone();
    let handle = timer_ref.clone();
    let action_label = action_str.clone();
    let pkg = pkg_name.clone();
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, Duration::from_millis(50), move || {
        if let Ok(result) = rx.try_recv() {
            if let Some(ui) = weak.upgrade() {
                ui.set_pkg_is_applying(false);
                match result {
                    Ok(_) => {
                        ui.set_pkg_status_text(
                            format!("Successfully {}ed {}", action_label, pkg).into(),
                        );
                        // Trigger a refresh to update the list
                        ui.invoke_pkg_refresh();
                    }
                    Err(err) => {
                        ui.set_pkg_error_text(err.into());
                        ui.set_pkg_status_text("Operation failed".into());
                    }
                }
            }
            *handle.borrow_mut() = None;
        }
    });
    *timer_ref.borrow_mut() = Some(timer);
}

/// Detail for one package, from `apt-cache show`.
///
/// The apk version of this walked a bespoke sectioned format looking for lines ending in
/// "description:" and "depends:". Debian's is RFC822, which `wire::apt::parse_detail` handles
/// and has tests for — including the lone `.` that means a blank line inside a description,
/// and the fact that `apt-cache show` prints every available version and only the first is the
/// one being described.
fn fetch_pkg_detail(name: &str, installed: bool, upgradable: bool) -> PkgDetail {
    let d = crate::wire::apt::detail(name, installed, upgradable);
    PkgDetail {
        name: d.name,
        version: d.version,
        description: d.description,
        maintainer: d.maintainer,
        dependencies: d.dependencies,
        size: d.size,
        repo: d.repo,
        installed: d.installed,
        upgradable: d.upgradable,
    }
}

/// Convert cached packages to Slint model items.
fn cache_to_model(cache: &[PkgEntry]) -> Vec<PackageData> {
    cache.iter().map(|p| pkg_to_model(p)).collect()
}

/// Convert a single PkgEntry to a Slint PackageData.
fn pkg_to_model(p: &PkgEntry) -> PackageData {
    PackageData {
        name: p.name.clone().into(),
        version: p.version.clone().into(),
        description: p.description.clone().into(),
        installed: p.installed,
        upgradable: p.upgradable,
        size_text: p.size_text.clone().into(),
        repo: p.repo.clone().into(),
    }
}

/// Clear the detail panel.
fn clear_detail(ui: &App) {
    ui.set_pkg_detail_name("".into());
    ui.set_pkg_detail_version("".into());
    ui.set_pkg_detail_description("".into());
    ui.set_pkg_detail_maintainer("".into());
    ui.set_pkg_detail_dependencies("".into());
    ui.set_pkg_detail_size("".into());
    ui.set_pkg_detail_repo("".into());
    ui.set_pkg_detail_installed(false);
    ui.set_pkg_detail_upgradable(false);
}

/// Capitalize first letter.
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
    }
}
