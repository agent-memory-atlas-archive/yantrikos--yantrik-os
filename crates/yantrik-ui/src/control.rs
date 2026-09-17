//! The shell describes itself.
//!
//! Every app under `apps/` now publishes `app.describe` / `app.act`, and the desktop is the one
//! window that matters most: it is where the companion lives. Without this, "what is on my
//! desktop right now" was answerable only by screenshotting the shell and asking a vision model
//! to read a status bar we wrote ourselves.
//!
//! Published on the same bus under `app-shell`, so `list_apps` finds it beside the others.
//!
//! The surface is deliberately narrow. The shell already serves the companion — memory, tools and
//! answers all arrive over `companion.*` on its own socket — so this covers only what those cannot
//! say: which screen is up, which windows are open, which services are running, and what the
//! status bar is reporting about the machine.

use slint::ComponentHandle;
use yantrik_app_runtime::control::{Action, App as ControlSurface, Param, View};

use crate::App;

/// The screens a caller may ask for by name.
///
/// Not every screen the shell can render. Boot, onboarding and login are states the shell enters
/// on its own and jumping into one would leave the session somewhere it cannot get back from;
/// locking has its own action, because locking a machine is an act rather than a view change.
/// Settings sections, by the name a caller would say.
///
/// Mirrors the list built in `wire::settings` — the ids are the same ints the sidebar uses.
const SETTINGS_SECTIONS: &[(&str, i32)] = &[
    ("appearance", 0),
    ("ai", 1),
    ("desktop", 2),
    ("network", 3),
    ("accounts", 4),
    ("privacy", 5),
    ("system", 6),
    ("skills", 7),
    ("harnesses", 8),
];

/// Name to `current-screen` id, and the ids are the ones `app.slint` actually renders.
///
/// They were not. `("terminal", 16)` sent a caller to the ABOUT screen: 16 is about, terminal
/// is 14, and 14 has no branch in app.slint at all because the terminal became a separate app
/// binary and the shell screen went away. Nobody noticed because nothing compares this list to
/// the file that decides what a number means. A photograph of `show_screen screen=terminal`
/// showing "About" is how it surfaced.
///
/// `screens_match_the_shell` in the tests below now reads app.slint and checks every id here
/// against the `if current-screen == N` branches, so this cannot drift again in silence.
const SCREENS: &[(&str, i32)] = &[
    ("desktop", 1),
    ("bond", 4),
    ("personality", 5),
    ("memory", 6),
    ("settings", 7),
    ("files", 8),
    ("notifications", 9),
    ("system", 10),
    ("images", 11),
    ("editor", 12),
    ("media", 13),
    ("about", 16),
    ("packages", 21),
    ("devices", 27),
    ("permissions", 28),
];

/// What `describe` calls the screen the shell is on.
///
/// Only the states a caller cannot ASK for belong in this match; everything else comes from
/// SCREENS, so a name can only be defined once. Two entries here were simply wrong -- 21 was
/// reported as "email" and 27 as "snippets", when 21 renders the package manager and 27 the
/// device dashboard. `describe` would tell an agent it was looking at email while the package
/// manager was on screen, which is worse than saying nothing.
pub(crate) fn screen_name(id: i32) -> &'static str {
    match id {
        0 => "boot",
        2 => "onboarding",
        3 => "lock",
        32 => "login",
        other => SCREENS
            .iter()
            .find(|(_, id)| *id == other)
            .map(|(name, _)| *name)
            .unwrap_or("unknown"),
    }
}

/// Join names the way a person would read them out: "a", "a and b", "a, b and c".
///
/// This line is the first thing anyone sees of the desktop, and "calendar and email and notes"
/// reads like a machine wrote it.
fn list_of(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// How much of the conversation `describe` reports, newest last.
///
/// The desktop could be asked a question by an agent and then had no way to tell it what came
/// back: the answer existed only as pixels, so confirming a reply meant screenshotting the shell
/// and reading the bubble with a vision model — the exact thing this surface exists to replace.
/// Six turns is enough to see a question and its answer with context around them, and short
/// enough that `describe` stays a glance.
const CONVERSATION_TAIL: usize = 6;

/// How much of one message travels. A long answer is read in the window; this is for confirming
/// what was said, not for moving a transcript through a control surface.
const MESSAGE_CLIP: usize = 600;

/// Cut to a length without splitting a character, and say that it was cut.
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max).collect();
    format!("{head}… ({} characters total)", text.chars().count())
}

/// How many directory entries `describe` will list.
///
/// A glance, not a transcript: a model reading 4,000 filenames has spent its context on
/// something it could have asked a narrower question about. The true count travels beside
/// the list, so a caller always knows it is looking at a window onto something larger.
const FILE_LISTING_CAP: usize = 40;

/// Publish the desktop on the service bus. Call from the UI thread before `run()`.
pub fn publish(ui: &App, ctx: &crate::app_context::AppContext) {
    // The catalogue, not a copy of it. The control surface answers from the same live list
    // the launcher shows, so an app installed a moment ago is launchable by name without
    // restarting the shell — which is what `accepted: true` ought to mean.
    let installed = ctx.installed_apps.clone();
    let refresh_catalogue = ctx.installed_apps.clone();
    let describe = {
        let weak = ui.as_weak();
        move || {
            let Some(ui) = weak.upgrade() else {
                return View::new("Yantrik — shutting down");
            };
            let screen = ui.get_current_screen();
            let bond = ui.get_bond_data();

            // From the launch registry, not the Slint window-list model. The model is only
            // refreshed while the desktop screen is showing, so a describe from any other screen
            // reported "0 windows open" even with apps running — the registry is refreshed by
            // launches and exits, not by which screen is up, so it is right everywhere.
            let open: Vec<serde_json::Value> = crate::windows::shell_windows()
                .into_iter()
                .map(|w| {
                    serde_json::json!({
                        "title": w.title,
                        "app": w.app_id,
                    })
                })
                .collect();

            let service_model = ui.get_services();
            let services: Vec<serde_json::Value> = {
                use slint::Model;
                (0..service_model.row_count())
                    .filter_map(|i| service_model.row_data(i))
                    .map(|s| {
                        serde_json::json!({
                            "id": s.id.to_string(),
                            "status": s.status.to_string(),
                            "note": s.note.to_string(),
                        })
                    })
                    .collect()
            };

            let down: Vec<&str> = services
                .iter()
                .filter(|s| {
                    matches!(s["status"].as_str(), Some("failed") | Some("stopped"))
                })
                .filter_map(|s| s["id"].as_str())
                .collect();

            // What the Files screen is showing. A directory listing is the thing an agent
            // most often needed and could not get without photographing the window.
            let files = if screen == 8 {
                use slint::Model;
                let entries = ui.get_file_browser_entries();
                let total = entries.row_count();
                let listing: Vec<serde_json::Value> = (0..total.min(FILE_LISTING_CAP))
                    .filter_map(|i| entries.row_data(i))
                    .map(|e| {
                        serde_json::json!({
                            "name": e.name.to_string(),
                            "dir": e.is_dir,
                            "size": e.size_text.to_string(),
                            "modified": e.modified_text.to_string(),
                        })
                    })
                    .collect();
                let sel = ui.get_file_selected_index();
                let selected = if sel >= 0 {
                    entries
                        .row_data(sel as usize)
                        .map(|e| serde_json::Value::String(e.name.to_string()))
                        .unwrap_or(serde_json::Value::Null)
                } else {
                    serde_json::Value::Null
                };
                serde_json::json!({
                    "path": ui.get_file_browser_path().to_string(),
                    "entries": listing,
                    "shown": total.min(FILE_LISTING_CAP),
                    "total": total,
                    "selected": selected,
                    "selection_count": ui.get_file_selection_count(),
                    "free_space": ui.get_file_free_space_text().to_string(),
                })
            } else {
                serde_json::Value::Null
            };

            // The wizard, when the wizard is up. This is the screen an agent is most likely to
            // meet first and, until it published anything, the only one it could not read.
            let installer = if screen == 2 {
                crate::control_installer::state(&ui)
            } else {
                serde_json::Value::Null
            };

            // The open document, when the editor is up — so an agent reads what it is editing
            // the same way it reads a directory or the weather.
            let editor = if screen == 12 {
                crate::control_editor::state(&ui)
            } else {
                serde_json::Value::Null
            };

            // The one line worth reading first: where the user is, what is open, and whether
            // anything is wrong. Trouble comes before window count, because trouble is the
            // reason to look.
            let summary = if screen == 2 {
                crate::control_installer::summary(&ui)
            } else if screen == 12 {
                crate::control_editor::summary(&ui)
            } else if !down.is_empty() {
                format!(
                    "Yantrik — {} screen, {} windows open, {} not running",
                    screen_name(screen),
                    open.len(),
                    list_of(&down)
                )
            } else if screen == 8 {
                // On the file screen the directory IS the answer to "where am I".
                format!(
                    "Yantrik — files at {}, {} items, {} windows open",
                    ui.get_file_browser_path(),
                    files["total"].as_u64().unwrap_or(0),
                    open.len()
                )
            } else {
                format!(
                    "Yantrik — {} screen, {} windows open, CPU {}%, memory {}",
                    screen_name(screen),
                    open.len(),
                    ui.get_bar_cpu_percent(),
                    ui.get_bar_mem_text()
                )
            };

            // Launches that died before they became a window. `open_app` defers, so it answers
            // "accepted" long before anything is on screen -- and when the app then exits, the
            // only account of it was a log line nobody reads. An agent that launched something
            // and sees nothing needs to be told why here, in the same place it reads everything
            // else.
            let failed: Vec<serde_json::Value> = crate::running::launch_failures()
                .into_iter()
                .map(|f| {
                    serde_json::json!({
                        "app": f.app_id,
                        "binary": f.binary,
                        "status": f.status,
                        "lived_ms": f.lived_ms,
                    })
                })
                .collect();

            // What was said. Roles and text, newest last, so a caller that asked a question can
            // read the answer instead of photographing it.
            let conversation: Vec<serde_json::Value> = {
                use slint::Model;
                let messages = ui.get_messages();
                let total = messages.row_count();
                (total.saturating_sub(CONVERSATION_TAIL)..total)
                    .filter_map(|i| messages.row_data(i))
                    .map(|m| {
                        serde_json::json!({
                            "role": m.role.to_string(),
                            "text": clip(m.content.as_str(), MESSAGE_CLIP),
                            // Still arriving. A caller polling for an answer needs to know the
                            // difference between "this is the reply" and "this is the reply so
                            // far", and an empty streaming bubble is the normal first state.
                            "streaming": m.is_streaming,
                        })
                    })
                    .collect()
            };

            View::new(summary)
                .with("screen", screen_name(screen))
                .with("conversation", serde_json::Value::Array(conversation))
                .with("screen_id", screen)
                .with("windows", serde_json::Value::Array(open))
                .with("failed_launches", serde_json::Value::Array(failed))
                // Which mind is answering, and what else could. An agent that can switch this
                // has to be able to see it first, and without the list it would be guessing at
                // ids for `use_harness`.
                // What is on START, in order. The person's choice, so an agent can read it
                // before proposing to change it.
                .with("pinned", crate::wire::settings::pinned_apps())
                .with(
                    "minds",
                    crate::wire::harness::host()
                        .map(|host| {
                            serde_json::Value::Array(
                                host.list()
                                    .iter()
                                    .map(|e| {
                                        serde_json::json!({
                                            "id": e.id,
                                            "name": e.name,
                                            "answering": e.active,
                                            "builtin": e.builtin,
                                            // What the mind said about itself when it attached:
                                            // its backend, its memory, wherever it is running.
                                            // The OS knows none of that on its own and does not
                                            // want to — this is the harness's own account.
                                            "detail": e.detail,
                                            "tools": e.capabilities.tools,
                                        })
                                    })
                                    .collect(),
                            )
                        })
                        .unwrap_or(serde_json::Value::Array(Vec::new())),
                )
                .with("files", files)
                .with("installer", installer)
                .with("editor", editor)
                .with("services", serde_json::Value::Array(services))
                .with("companion_online", ui.get_companion_online())
                .with("companion_status", ui.get_companion_status().to_string())
                .with("thinking", ui.get_is_thinking())
                .with("pending_suggestions", ui.get_pending_count())
                .with("memories", ui.get_memory_count())
                .with("bond", bond.bond_level.to_string())
                .with("bond_score", bond.bond_score as f64)
                .with("active_project", ui.get_active_project().to_string())
                .with("clock", ui.get_clock_text().to_string())
                .with("date", ui.get_date_text().to_string())
                .with("cpu_percent", ui.get_bar_cpu_percent())
                .with("memory", ui.get_bar_mem_text().to_string())
                .with("memory_percent", ui.get_bar_mem_percent())
                .with("disk", ui.get_bar_disk_text().to_string())
                .with("disk_percent", ui.get_bar_disk_percent())
                .with("wifi", ui.get_wifi_connected())
                .with(
                    "battery",
                    if ui.get_battery_available() {
                        serde_json::json!({
                            "percent": ui.get_battery_level(),
                            "charging": ui.get_battery_charging(),
                        })
                    } else {
                        serde_json::Value::Null
                    },
                )
                .with("do_not_disturb", ui.get_dnd_mode())
                .with("incognito", ui.get_settings_incognito_mode())
        }
    };

    let weak = ui.as_weak();
    let ui_for = move || weak.upgrade().ok_or_else(|| "the shell is gone".to_string());

    let open_ui = ui_for.clone();
    let screen_ui = ui_for.clone();
    let focus_ui = ui_for.clone();
    let dnd_ui = ui_for.clone();
    let ask_ui = ui_for.clone();
    let pin_ui = ui_for.clone();
    let pin_catalogue = ctx.installed_apps.clone();
    let lock_ui = ui_for;

    let surface = ControlSurface::new("shell")
        .describe(describe)
        .action(
            // Deferred, and the handshake with yantrik-mind is what proved it. This returned
            // `settled: true` while the shell still reported "0 windows open" and no app socket
            // had appeared — a driver reading that would report a launch it had only requested.
            //
            // `invoke_launch_app` reaches the dock's callback, and most of its branches
            // `spawn()` a process: the window arrives seconds later, if it arrives at all (a
            // failed spawn is logged, not returned). A couple of branches only switch screens and
            // do settle on return, but the caller cannot tell which branch it took, so the
            // conservative claim is the only honest one.
            Action::new("open_app", "Launch an app, or focus it if it is already running")
                .arg(Param::text("name").describe("App id, e.g. notes, email, terminal, files"))
                .defers(),
            move |args| {
                let ui = open_ui()?;
                let name = args["name"].as_str().unwrap_or_default().trim().to_string();
                if name.is_empty() {
                    return Err("`name` is empty".into());
                }
                // Checked before answering. The dispatch discovers an unknown id too, but only
                // after this function has already reported the launch as under way.
                let catalogue = installed.get();
                if !crate::wire::dock::is_known_app(&name, &catalogue) {
                    return Err(format!(
                        "no app `{name}` on this machine; it can launch: {}",
                        crate::wire::dock::BUILTIN_APP_IDS.join(", ")
                    ));
                }
                // The launcher's own path: it resolves the binary, enforces one window per app,
                // and focuses the running one instead of starting a second.
                ui.invoke_launch_app(name.clone().into());
                Ok(serde_json::json!({ "launching": name }))
            },
        )
        .action(
            // The launcher rescans when it opens; this is the same thing without a person
            // having to open it. An agent that installs a package and then wants to run it
            // needs a way to say "look again" that is not "press the Apps button".
            Action::new("refresh_apps", "Rescan the installed applications"),
            {
                let catalogue = refresh_catalogue.clone();
                move |_args| {
                    let count = catalogue.refresh();
                    Ok(serde_json::json!({ "apps": count }))
                }
            },
        )
        .action(
            // Parity with the pin on every tile in All apps. Deciding what sits on START is a
            // person's call, and an agent tidying a desktop on someone's behalf needs the same
            // verb rather than a way to fake the click.
            Action::new("pin_app", "Pin an app to START, or unpin it")
                .arg(Param::text("name").describe("App id, e.g. notes, files, browser, chromium"))
                .arg(Param::flag("pinned").describe("true to pin, false to unpin")),
            move |args| {
                let ui = pin_ui()?;
                let name = args["name"].as_str().unwrap_or_default().trim().to_string();
                let want = args["pinned"].as_bool().ok_or("`pinned` must be true or false")?;
                if name.is_empty() {
                    return Err("`name` is empty".into());
                }
                let installed = pin_catalogue.get();
                // Checked, because a pin for something that cannot launch is a START tile that
                // does nothing when clicked — the worst kind of shortcut.
                if !crate::wire::dock::is_known_app(&name, &installed) {
                    return Err(format!("no app `{name}` on this machine"));
                }
                if !crate::wire::pins::is_pinnable(&name) {
                    return Err(format!(
                        "`{name}` is the launcher, and its button is already on the taskbar"
                    ));
                }
                if crate::wire::pins::is_pinned(&name) != want {
                    crate::wire::pins::toggle(&name);
                }
                crate::wire::pins::publish(&ui, &installed);
                Ok(serde_json::json!({
                    "app": crate::wire::pins::pin_id(&name),
                    "pinned": want,
                    "start": crate::wire::settings::pinned_apps(),
                }))
            },
        )
        .action(
            // Talking to the desktop, without a keyboard.
            //
            // Every other verb here moves the shell around; this one uses it. It existed only as
            // a text field, so the one thing the desktop is FOR — asking it something — was the
            // one thing this surface could not do, and proving the mind was reachable meant
            // clicking at pixel coordinates and typing into whatever had focus. That test can
            // fail in silence in four different ways before a single byte reaches a harness.
            //
            // Deferred, because the answer streams: this returns when the question has been
            // asked, not when it has been answered. The answer arrives in `describe` under
            // `conversation`, where a caller can watch `streaming` go false.
            Action::new("send_message", "Ask the desktop something, as if typed into the Lens")
                .arg(Param::text("text").describe("What to say"))
                .defers(),
            move |args| {
                let ui = ask_ui()?;
                let text = args["text"].as_str().unwrap_or_default().trim().to_string();
                if text.is_empty() {
                    return Err("`text` is empty".into());
                }
                // The shell's own callback, not a private path beside it: whatever a person
                // typing gets — the mind picker, the bubbles, the streaming state — this gets
                // too, because it is the same call.
                ui.invoke_send_message(text.clone().into());
                Ok(serde_json::json!({
                    "asked": text,
                    // Named here because it is the whole question this action tends to be
                    // asked in service of: which mind is about to answer.
                    "mind": crate::wire::harness::host()
                        .map(|h| h.active_id())
                        .unwrap_or_else(|| crate::wire::harness::BUILTIN_ID.to_string()),
                }))
            },
        )
        .action(
            // Parity, deliberately: anything a person can do on the Harnesses screen, an agent
            // can do here. A control surface that could not change which mind is answering would
            // be the one decision on this desktop reserved for the mouse.
            Action::new("use_harness", "Choose which mind answers when the shell is asked something")
                .arg(Param::text("id").describe("Harness id, as `describe shell` lists under `minds`")),
            move |args| {
                let id = args["id"].as_str().unwrap_or_default().trim().to_string();
                if id.is_empty() {
                    return Err("`id` is empty".into());
                }
                let host = crate::wire::harness::host()
                    .ok_or_else(|| "the harness host is not running".to_string())?;
                host.set_active(&id)?;
                // The same memory the Settings screen writes. A choice made here is a choice
                // about the machine, and an agent that switches minds should not have its
                // decision quietly undone by the next restart any more than a person should.
                crate::wire::settings::set_preferred_mind(&id);
                Ok(serde_json::json!({
                    "answering": id,
                    "tools": host.list().iter().find(|e| e.id == id).map(|e| e.capabilities.tools),
                }))
            },
        )
        .action(
            Action::new("show_screen", "Switch the shell to one of its screens")
                .arg(
                    Param::text("screen")
                        .describe("desktop, files, settings, notifications, memory, system, permissions, bond, personality, about, packages, devices, images, editor, media"),
                )
                .arg(
                    Param::text("section")
                        .optional()
                        .describe("For `settings`: appearance, ai, desktop, network, accounts, privacy, system, skills, harnesses"),
                ),
            move |args| {
                let ui = screen_ui()?;
                let want = args["screen"].as_str().unwrap_or_default().trim().to_lowercase();

                // Sending someone to Settings and leaving them to find the section is a chore,
                // not a link — for a person following an instruction and for an agent alike.
                let section = args["section"].as_str().unwrap_or_default().trim().to_lowercase();
                if !section.is_empty() {
                    let id = SETTINGS_SECTIONS
                        .iter()
                        .find(|(name, _)| *name == section)
                        .map(|(_, id)| *id)
                        .ok_or_else(|| {
                            let names: Vec<&str> =
                                SETTINGS_SECTIONS.iter().map(|(n, _)| *n).collect();
                            format!(
                                "no settings section called `{section}`; there is: {}",
                                names.join(", ")
                            )
                        })?;
                    ui.set_settings_category(id);
                }

                let id = SCREENS
                    .iter()
                    .find(|(name, _)| *name == want)
                    .map(|(_, id)| *id)
                    .ok_or_else(|| {
                        let names: Vec<&str> = SCREENS.iter().map(|(n, _)| *n).collect();
                        format!("no screen called `{want}`; there is: {}", names.join(", "))
                    })?;
                // Set and invoke, in that order — the same pair every caller in the shell uses.
                // `navigate` is what loads a screen's data; setting the property alone shows an
                // empty one.
                ui.set_current_screen(id);
                ui.invoke_navigate(id);
                let mut showing = serde_json::json!({ "showing": want });
                if !section.is_empty() {
                    showing["section"] = section.into();
                }
                Ok(showing)
            },
        )
        .action(
            // Deferred for the same reason as `open_app`, and slightly worse: this spawns
            // `wlrctl toplevel focus` and the wiring discards the result with `let _ =`, so on a
            // machine without wlrctl it succeeds loudly and does nothing at all. Focus is also
            // the compositor's to grant, not ours to assert — we do not own labwc.
            Action::new("focus_window", "Bring an open window to the front")
                .defers()
                .arg(Param::text("title").describe("Window title, or part of one")),
            move |args| {
                let ui = focus_ui()?;
                let want = args["title"].as_str().unwrap_or_default().trim().to_lowercase();
                let windows = ui.get_window_list();
                let title = {
                    use slint::Model;
                    let rows: Vec<_> =
                        (0..windows.row_count()).filter_map(|i| windows.row_data(i)).collect();
                    rows.iter()
                        .find(|w| w.title.to_lowercase() == want)
                        .or_else(|| rows.iter().find(|w| w.title.to_lowercase().contains(&want)))
                        .map(|w| w.title.to_string())
                        .ok_or_else(|| {
                            let open: Vec<String> =
                                rows.iter().map(|w| w.title.to_string()).collect();
                            if open.is_empty() {
                                "no windows are open".to_string()
                            } else {
                                format!("no open window matches `{want}`; there is: {}", open.join(", "))
                            }
                        })?
                };
                ui.invoke_switch_window(title.clone().into());
                Ok(serde_json::json!({ "focused": title }))
            },
        )
        .action(
            Action::new("set_do_not_disturb", "Hold or release notifications")
                .arg(Param::flag("on")),
            move |args| {
                let ui = dnd_ui()?;
                let on = args["on"].as_bool().ok_or("`on` must be true or false")?;
                ui.set_dnd_mode(on);
                Ok(serde_json::json!({ "do_not_disturb": on }))
            },
        )
        .action(
            // Locking is not a view change: the person has to type their way back in. It gets its
            // own action and its own risk rather than hiding inside `show_screen`.
            Action::new("lock", "Lock the session").risk("sensitive"),
            move |_| {
                let ui = lock_ui()?;
                ui.invoke_lock_screen();
                Ok(serde_json::json!({ "locked": true }))
            },
        );

    // The installer and the updater keep their actions in their own modules: they are the
    // riskiest things the shell can be asked to do (one erases a disk, the other replaces every
    // binary and restarts) and they deserve to be read together, not buried at the end of a file
    // about status bars.
    let surface = crate::control_installer::actions(surface, ui);
    let surface = crate::control_update::actions(surface, ui);
    let surface = crate::control_files::actions(surface, ui);
    crate::control_editor::actions(surface, ui).serve();
}

#[cfg(test)]
mod screen_table_tests {
    use super::{SCREENS, SETTINGS_SECTIONS, screen_name};
    use std::path::Path;

    /// Elements that wrap a screen rather than being one.
    const CHROME: &[&str] = &[
        "WindowFrame", "Rectangle", "Text", "HorizontalLayout", "VerticalLayout",
        "Image", "TouchArea", "Flickable", "GridLayout", "Timer", "FocusScope",
    ];

    /// `app.slint` decides what a screen id means. This reads it: for each
    /// `if current-screen == N`, the id and the component actually drawn there.
    fn rendered() -> Vec<(i32, String)> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../yantrik-ui-slint/ui/app.slint");
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        let lines: Vec<&str> = src.lines().collect();

        let mut out = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            let Some(rest) = line.trim().strip_prefix("if current-screen == ") else {
                continue;
            };
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            let Ok(id) = digits.parse::<i32>() else { continue };

            // The first CamelCase element under the branch that is not chrome.
            let mut component = String::new();
            'scan: for l in lines.iter().skip(i).take(30) {
                for token in l.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
                    if token.len() < 4 || !token.starts_with(|c: char| c.is_ascii_uppercase()) {
                        continue;
                    }
                    if CHROME.contains(&token) {
                        continue;
                    }
                    if l.contains(&format!("{token} {{")) || l.contains(&format!("{token}{{")) {
                        component = token.to_string();
                        break 'scan;
                    }
                }
            }
            out.push((id, component));
        }
        out
    }

    fn rendered_ids() -> Vec<i32> {
        rendered().into_iter().map(|(id, _)| id).collect()
    }

    /// Every screen a caller can ask for must be one the shell actually draws.
    ///
    /// `("terminal", 16)` sat in this table pointing at the ABOUT screen. Terminal is 14, and
    /// 14 stopped being rendered when the terminal became its own app binary — so `yos act
    /// shell show_screen screen=terminal` quietly showed you About, and had done for as long
    /// as that was true. The list and the file that gives the numbers meaning were maintained
    /// by different hands and never compared.
    #[test]
    fn every_screen_a_caller_can_ask_for_is_one_the_shell_draws() {
        let rendered = rendered_ids();
        let missing: Vec<String> = SCREENS
            .iter()
            .filter(|(_, id)| !rendered.contains(id))
            .map(|(name, id)| format!("{name} -> {id}"))
            .collect();

        assert!(
            missing.is_empty(),
            "these screens are offered by the control surface but app.slint renders no \
             `if current-screen == N` branch for them:\n  {}\n\n\
             Either the screen was removed (drop it here) or the id is wrong. The ids are \
             defined by app.slint, not by this table.",
            missing.join("\n  ")
        );
    }

    /// One id, one name — in both directions.
    ///
    /// `screen_name` used to carry its own entries alongside SCREENS, and two of them
    /// disagreed with the shell: 21 was reported as "email" when it renders the package
    /// manager, and 27 as "snippets" when it renders the device dashboard. `describe` told
    /// callers which screen they were on, and for those two it was lying.
    #[test]
    fn a_screen_answers_to_exactly_one_name() {
        for (name, id) in SCREENS {
            assert_eq!(
                screen_name(*id), *name,
                "screen {id} is offered as `{name}` but describe() calls it `{}`",
                screen_name(*id)
            );
        }

        let mut ids: Vec<i32> = SCREENS.iter().map(|(_, id)| *id).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(before, ids.len(), "two names in SCREENS map to the same screen id");

        let mut names: Vec<&str> = SCREENS.iter().map(|(n, _)| *n).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "the same name appears twice in SCREENS");
    }

    /// The name a caller says must describe the screen that id draws.
    ///
    /// This is the check that would have caught `("terminal", 16)`. The other two would not:
    /// 16 IS rendered, and the name was self-consistent because both directions read the same
    /// table. What was wrong is the only thing neither could see — that the screen drawn at 16
    /// is AboutScreen, and nobody calls that a terminal.
    ///
    /// Matching a name against a component name is a heuristic, so it is deliberately loose:
    /// singular/plural is ignored, and an id whose component could not be parsed is skipped
    /// rather than failed. A loose check that runs beats a strict one that gets deleted.
    #[test]
    fn a_screens_name_matches_what_it_draws() {
        let drawn = rendered();
        let mut wrong = Vec::new();

        for (name, id) in SCREENS {
            let Some((_, component)) = drawn.iter().find(|(rid, c)| rid == id && !c.is_empty())
            else {
                continue; // not parseable from the markup; the other tests still cover the id
            };
            let stem = name.strip_suffix('s').unwrap_or(name).to_ascii_lowercase();
            if !component.to_ascii_lowercase().contains(&stem) {
                wrong.push(format!("`{name}` -> {id}, which draws {component}"));
            }
        }

        assert!(
            wrong.is_empty(),
            "these names do not describe the screen they point at:\n  {}\n\n\
             The ids come from app.slint. If the screen moved, take its id from the \
             `if current-screen == N` branch that renders it.",
            wrong.join("\n  ")
        );
    }

    /// The settings sections a caller can name are the ones the sidebar has.
    #[test]
    fn settings_sections_are_contiguous_from_zero() {
        let mut ids: Vec<i32> = SETTINGS_SECTIONS.iter().map(|(_, id)| *id).collect();
        ids.sort_unstable();
        let expected: Vec<i32> = (0..ids.len() as i32).collect();
        assert_eq!(
            ids, expected,
            "settings section ids are the sidebar's indices, so they run 0..n with no gaps"
        );
    }
}
