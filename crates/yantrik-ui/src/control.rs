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

/// Refuse, with a reason a caller can act on, to open or pin an app that will not open.
fn check_launchable(name: &str, installed: &[crate::apps::DesktopEntry]) -> Result<(), String> {
    use crate::wire::dock::{availability, launchable_app_ids, Availability};
    match availability(name, installed) {
        Availability::Ready => Ok(()),
        Availability::Missing(what) => Err(format!(
            "`{name}` is not installed on this machine: {what} was not found. It can open: {}",
            launchable_app_ids(installed).join(", ")
        )),
        // Not "unknown" and not "not installed", because it is neither, and both of those
        // invite the caller to try again — to rescan, to install a package, to guess at another
        // spelling. This build does not have the app and no action on this machine will produce
        // it, so the refusal says that, says what is missing under the screen, and says what
        // would have to be built. An agent reading it can stop, or go and write the missing half.
        Availability::Shelved(shelf) => Err(format!(
            "`{name}` is not part of this build. {} is shelved: {}. It comes back when {} \
             — see design/shelved-2026-09-20.md. It can open: {}",
            shelf.name,
            shelf.reason,
            shelf.returns_when,
            launchable_app_ids(installed).join(", ")
        )),
        Availability::Unknown => Err(format!(
            "no app `{name}` on this machine; it can open: {}",
            launchable_app_ids(installed).join(", ")
        )),
    }
}

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
///
/// Takes the service manager because the shell is the only process that owns service lifetimes:
/// `start_service` below is what makes the rail's "on demand" a mechanism rather than a caption
/// on a service nothing ever starts.
pub fn publish(
    ui: &App,
    ctx: &crate::app_context::AppContext,
    services: yantrik_shell_core::service_manager::ServiceManager,
) {
    // The Allow and Deny buttons, before anything can be asked for. They are Slint callbacks
    // and nothing else: granting is a click, never an action on this surface. See
    // `control_approvals`.
    crate::control_approvals::wire(ui);

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
                            "selected": e.selected,
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
                    "loading": ui.get_file_browser_loading(),
                    "notice": ui.get_file_notice().to_string(),
                    "operation_busy": ui.get_file_operation_busy(),
                    "operation": ui.get_file_operation_text().to_string(),
                    "operation_progress": ui.get_file_operation_progress(),
                    "trash": ui.get_file_trash_mode(),
                    "can_undo": ui.get_file_can_undo(),
                    "has_clipboard": ui.get_file_has_clipboard(),
                    "preview_open": ui.get_file_quick_look_open(),
                    "preview_name": ui.get_file_quick_look_name().to_string(),
                    "tabs": ui.get_file_tabs().row_count(),
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
                // What is waiting on a person right now. Published so a second mind, or a
                // test, can tell "the machine is waiting for someone to press a button" from
                // "the machine is hung" — the two look identical from outside otherwise.
                .with("pending_approvals", crate::control_approvals::pending_for_describe())
                // The owner's standing policy for callers on the socket, so a bridge can read
                // it instead of provoking a `CEILING:` refusal to find out. An approval cannot
                // exceed this, and a question the machine will refuse to answer should never
                // reach the person.
                .with("tool_permission", crate::control_approvals::machine_ceiling())
                // What the mind may do without being asked, and the rules a person has granted
                // for this session. The bridge takes this off the SAME read as the ceiling above
                // and makes the run/ask/refuse decision from it, so one `describe shell` answers
                // every question an `os_act` has to ask before it runs.
                .with("mind_mode", crate::control_approvals::mind_mode_for_describe())
                // And what it has already done unasked. A mode that stops the asking has to
                // replace the cards with something, or `auto` is only a quieter way of not
                // knowing. See `mind_mode`'s audit section.
                .with("mind_audit_recent", crate::control_approvals::mind_audit_for_describe())
                // Which mind is answering, and what else could. An agent that can switch this
                // has to be able to see it first, and without the list it would be guessing at
                // ids for `use_harness`.
                // What is on START, in order. The person's choice, so an agent can read it
                // before proposing to change it.
                .with("pinned", crate::wire::settings::pinned_apps())
                // What `open_app` accepts. It took a name and this state offered none.
                .with("apps", serde_json::Value::Array(crate::wire::dock::openable()))
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
                // What the machine is trying to tell the person, so that "is anything waiting
                // for me" is a read of the shell rather than a second call to the notifications
                // service — and so a mind can see what it has already said.
                .with("notifications", crate::wire::notifications::describe_summary())
                // The ask bar, so "is the Lens up, and what is in it" is a read rather than a
                // screenshot. `open_lens` answers from these same two properties.
                .with(
                    "lens",
                    serde_json::json!({
                        "open": ui.get_lens_open(),
                        "text": ui.get_lens_input_text().to_string(),
                        "chat": ui.get_lens_chat_mode(),
                    }),
                )
                .with("incognito", ui.get_settings_incognito_mode())
                .with("settings", serde_json::json!({"category":ui.get_settings_category(),"query":ui.get_settings_query().to_string(),"dark":ui.get_settings_dark_mode(),"accent":ui.get_settings_accent_color().to_string(),"wallpaper":ui.get_wallpaper_path().to_string(),"save_error":ui.get_settings_save_error(),"save_status":ui.get_settings_save_status().to_string(),"auto_lock_secs":ui.get_settings_auto_lock_secs()}))
        }
    };

    let weak = ui.as_weak();
    let ui_for = move || weak.upgrade().ok_or_else(|| "the shell is gone".to_string());

    let open_ui = ui_for.clone();
    let screen_ui = ui_for.clone();
    let focus_ui = ui_for.clone();
    let dnd_ui = ui_for.clone();
    let ask_ui = ui_for.clone();
    let lens_ui = ui_for.clone();
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
                // after this function has already reported the launch as under way — and a known
                // app whose program is not installed used to be answered "launching" as well.
                let catalogue = installed.get();
                check_launchable(&name, &catalogue)?;
                // The launcher's own path: it resolves the binary, enforces one window per app,
                // and focuses the running one instead of starting a second.
                ui.invoke_launch_app(name.clone().into());
                Ok(serde_json::json!({ "launching": name }))
            },
        )
        .action(
            // What "on demand" in the machine rail is supposed to mean. calendar, email and
            // notes are registered without autostart, so on a fresh session their sockets do
            // not exist; an app calling one got a connect failure and, in the calendar's case,
            // reported the appointment as saved anyway. Apps now ask for the service first,
            // and the manager that starts it is the same one the rail reads, so a running
            // service is never described as stopped.
            //
            // Standard, not sensitive: this starts one of the machine's own registered
            // services, which is what opening the app that needs it would have done.
            Action::new("start_service", "Start one of the machine's services if it is not running")
                .arg(Param::text("name").describe("Service id, as the machine rail lists it")),
            {
                let services = services.clone();
                move |args: &serde_json::Value| {
                    let name = args["name"].as_str().unwrap_or_default().trim().to_string();
                    if name.is_empty() {
                        return Err("`name` is empty".into());
                    }
                    // Already up is the outcome the caller wanted, not an error to handle.
                    if matches!(
                        services.status(&name),
                        Some(yantrik_shell_core::service_manager::ServiceStatus::Running)
                    ) {
                        return Ok(serde_json::json!({ "service": name, "state": "already running" }));
                    }
                    services.start(&name)?;
                    Ok(serde_json::json!({ "service": name, "state": "started" }))
                }
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
                // does nothing when clicked — the worst kind of shortcut. Unpinning is always
                // allowed: it is how a person clears a pin for something they removed.
                if want {
                    check_launchable(&name, &installed)?;
                } else if !crate::wire::dock::is_known_app(&name, &installed)
                    && !crate::wire::pins::is_pinned(&name)
                {
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
            // Opening the ask bar, without asking it anything.
            //
            // `send_message` already puts a question to the desktop, but it asks it and is done —
            // there was no way to leave the Lens standing open in front of a person with a draft
            // in it, which is what "here, have a look at this" is. The desktop advertises Ctrl+K
            // in two places for exactly this, and a compositor keybind needs a verb to call:
            // config/labwc/rc.xml binds Super+K to this action, because that is the only route
            // that works while another app holds the keyboard.
            //
            // `safe`: it shows a panel. Nothing is sent, nothing is spawned, nothing is written.
            //
            // The answer is the OBSERVED state, read back off the shell after the calls, not an
            // `accepted: true` — which was the point of the exercise. The Lens is drawn by the
            // desktop screen and nowhere else, so opening it means going to the desktop first;
            // if that did not take, `lens_open` comes back false and the caller knows.
            Action::new("open_lens", "Open the ask bar (the Lens) and put the cursor in it")
                .risk("safe")
                .arg(
                    Param::text("text")
                        .optional()
                        .describe("Put this in the field, ready to edit. It is NOT submitted — use send_message to ask"),
                ),
            move |args| {
                let ui = lens_ui()?;
                let text = args["text"].as_str().unwrap_or_default().to_string();

                // The Lens lives on the desktop screen. Set-then-invoke, the pair every caller
                // in the shell uses: the property shows the screen, `navigate` loads it.
                if ui.get_current_screen() != 1 {
                    ui.set_current_screen(1);
                    ui.invoke_navigate(1);
                }

                // Prefilled before the panel opens, so the results the Lens builds on open are
                // the results for this text rather than for an empty field.
                if !text.is_empty() {
                    ui.set_lens_input_text(text.clone().into());
                    // What typing it would have done. `lens_query` is the as-you-type search,
                    // not the submit — the field is left for a person to edit or send.
                    ui.invoke_lens_query(text.clone().into());
                }

                ui.set_lens_open(true);
                ui.invoke_open_lens();

                // Whoever calls this is, by construction, somewhere else: Ctrl+K already works
                // when the shell has the keyboard, so this action is what Super+K reaches for
                // from inside another window. The first time it ran on a machine with Notes in
                // front, it answered `lens_open: true` and was right — the Lens had opened,
                // underneath Notes, where nobody could see it or type into it. The shell is an
                // ordinary toplevel to the compositor, so it is asked to come forward the way
                // the taskbar asks for any other window. Off the UI thread: wlrctl is a process.
                std::thread::spawn(|| {
                    match std::process::Command::new("wlrctl")
                        .args(["toplevel", "focus", "title:Yantrik OS"])
                        .status()
                    {
                        Ok(status) if status.success() => {}
                        Ok(status) => tracing::warn!(
                            code = status.code().unwrap_or(-1),
                            "the Lens is open but the shell could not be brought in front of it"
                        ),
                        Err(e) => tracing::warn!(error = %e, "could not run wlrctl to raise the shell"),
                    }
                });

                Ok(serde_json::json!({
                    "lens_open": ui.get_lens_open(),
                    "screen": screen_name(ui.get_current_screen()),
                    "text": ui.get_lens_input_text().to_string(),
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
                if ui.get_dnd_mode()!=on { ui.invoke_toggle_dnd_mode(); }
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
    // Asking the person. Three actions, all `safe`, none of which decides anything — the
    // decision is a button in the Lens. See `control_approvals` for why that split is the
    // whole point.
    let surface = crate::control_approvals::actions(surface, ui);
    crate::control_editor::actions(surface, ui).serve();
}

#[cfg(test)]
mod screen_table_tests {
    use super::{SCREENS, SETTINGS_SECTIONS, check_launchable, screen_name};
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

    /// Asking to open a shelved app is refused, and the refusal says why and what would fix it.
    ///
    /// The three refusals have to read differently, because they ask different things of the
    /// caller. "Not installed" means install it. "No app by that name" means try another name.
    /// "Not part of this build" means neither will help — the app is in the tree and nothing on
    /// this machine will produce it — so the refusal carries the reason and what would have to be
    /// built, and an agent reading it can stop instead of retrying four spellings.
    #[test]
    fn opening_a_shelved_app_is_refused_with_its_reason() {
        for name in ["music", "music-player", "Music Player", "spreadsheet", "ySheets"] {
            let err = check_launchable(name, &[]).expect_err("a shelved app must not open");
            assert!(err.contains("not part of this build"), "{name}: {err}");
            assert!(err.contains("shelved"), "{name}: {err}");
            assert!(err.contains("It comes back when"), "{name}: {err}");
            assert!(err.contains("design/shelved-2026-09-20.md"), "{name}: {err}");
            // And the list it offers instead never names the app it has just refused.
            let offered = err.split("It can open: ").nth(1).unwrap_or("");
            assert!(
                !offered.split(", ").any(|id| crate::wire::dock::shelved(id).is_some()),
                "{name} was refused and then offered something shelved: {offered}"
            );
        }
        assert!(check_launchable("files", &[]).is_ok(), "a shipped app still opens");
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
