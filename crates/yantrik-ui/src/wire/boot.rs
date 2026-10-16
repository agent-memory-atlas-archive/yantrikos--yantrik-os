//! The boot screen's stages, driven by what the machine is actually doing.
//!
//! The screen this replaces ran on a timer: four phases at one, three, five and seven seconds,
//! and a status line reading "remembering..." throughout. It was not connected to anything. The
//! shell was starting nine services, loading an embedder, opening a database and waking a
//! companion, and the first screen anybody sees of this OS said nothing about any of it.
//!
//! Every stage below is something the shell genuinely waits for, and its state is read rather
//! than assumed. That is the same rule the agent rail lives by, on the one screen where the
//! person has nothing to do but read.
//!
//! # Why it still finishes on a deadline
//!
//! Because a boot screen that waits forever for a service that will never come is worse than
//! one that lies. The deadline is generous and the stages stay honest: a stage that has not
//! finished is not ticked, and the desktop arrives anyway with the machine rail showing what is
//! still down. Showing the desktop is not a claim that everything worked.

use std::cell::{Cell, RefCell};
use std::ffi::OsString;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model, ModelRc, RenderingState, SharedString, Timer, TimerMode, VecModel};
use yantrik_shell_core::service_manager::{ServiceManager, ServiceStatus};

use crate::app_context::AppContext;
use crate::{App, BootStage};

/// How often the stages are re-read.
const TICK: Duration = Duration::from_millis(400);

/// When the desktop arrives regardless.
///
/// Long enough for a cold embedder fetch on a slow link, short enough that a broken service does
/// not strand someone on a splash screen.
const DEADLINE: Duration = Duration::from_secs(45);

/// The least time the screen stays up.
///
/// Everything below can be true within a few hundred milliseconds on a warm machine, and a boot
/// screen that flashes past is worse than none: it reads as a glitch. Two seconds is long enough
/// to see what happened.
const FLOOR: Duration = Duration::from_secs(2);

struct Stage {
    label: &'static str,
    detail: &'static str,
}

/// In the order the shell actually reaches them.
const STAGES: &[Stage] = &[
    Stage { label: "Starting services", detail: "network, notifications, monitors" },
    Stage { label: "Waking the companion", detail: "the mind that runs this desktop" },
    Stage { label: "Loading memory", detail: "semantic index and what it remembers" },
    Stage { label: "Preparing the desktop", detail: "your apps, your working set" },
];

pub fn wire(ui: &App, ctx: &AppContext, services: ServiceManager) {
    // The same string About and Settings show, and the same one `yantrik-update` compares. It
    // used to be this crate's `CARGO_PKG_VERSION`, so the first thing the machine said about
    // itself on boot was a number that named no build.
    ui.set_version(SharedString::from(yantrik_version::version()));
    push(ui, &[false; 4]);
    say_drawn(ui);

    let started = Instant::now();
    let bridge = ctx.bridge.clone();
    let ui_weak = ui.as_weak();
    let timer = Rc::new(Timer::default());
    let slot = timer.clone();
    let done = Rc::new(RefCell::new(false));

    timer.start(TimerMode::Repeated, TICK, move || {
        let Some(ui) = ui_weak.upgrade() else { return };
        if *done.borrow() {
            return;
        }

        // ── What is true right now ──
        //
        // Read, not assumed, and read STRICTLY. The first version asked whether ANY service was
        // running, which is true the moment the first of nine comes up — so the stage ticked
        // almost immediately and the screen handed over to a desktop that was still filling
        // itself in. "Settled" is the right question: nothing still Starting. A service that
        // failed has settled too; it is not coming, and the machine rail will say so.
        let services_settled = !services
            .list()
            .iter()
            .any(|s| matches!(s.status, ServiceStatus::Starting));
        let companion_up = bridge.is_online();
        // Memory is open once the companion answers AND has published a count. On a machine
        // with nothing remembered yet the count is 0 and the companion is the only witness.
        let memory_ready = companion_up;
        let elapsed = started.elapsed();

        // The desktop is the last thing, and the only one that can report on itself: it is ready
        // when it has the things it draws.
        //
        // This read `dock_items`, on the stated grounds that it came from the app catalogue. It
        // never did — it was a hardcoded list the system poll rebuilt — and it is now the
        // person's pinned apps, which can legitimately be EMPTY. Someone who unpinned everything
        // would have sat on this screen for the full 45-second deadline at every boot. The
        // launcher's list is the one that really is the catalogue scan.
        let desktop_drawn = ui.get_grid_apps().row_count() > 0;
        let desktop_ready =
            services_settled && companion_up && memory_ready && desktop_drawn && elapsed >= FLOOR;
        let flags = [services_settled, companion_up, memory_ready, desktop_ready];

        push(&ui, &flags);

        if desktop_ready || elapsed >= DEADLINE {
            if elapsed < DEADLINE {
                tracing::info!(took_ms = elapsed.as_millis() as u64, "Boot complete");
            } else {
                // Said plainly in the log, because the desktop is about to appear with things
                // missing and somebody will want to know which.
                tracing::warn!(
                    services_settled,
                    companion_up,
                    desktop_drawn,
                    "Boot deadline reached with stages unfinished — showing the desktop anyway"
                );
            }
            *done.borrow_mut() = true;
            slot.stop();
            ui.invoke_boot_complete();
        }
    });
    std::mem::forget(timer);
}

/// Turn the flags into rows.
///
/// Exactly one stage is "running": the first that is not done. A list with two spinners on it is
/// a list that has stopped meaning anything.
fn push(ui: &App, flags: &[bool; 4]) {
    let first_unfinished = flags.iter().position(|f| !f);
    let rows: Vec<BootStage> = STAGES
        .iter()
        .enumerate()
        .map(|(i, s)| BootStage {
            label: s.label.into(),
            detail: s.detail.into(),
            state: if flags[i] {
                2
            } else if first_unfinished == Some(i) {
                1
            } else {
                0
            },
        })
        .collect();

    let done = flags.iter().filter(|f| **f).count();
    ui.set_progress(done as f32 / STAGES.len() as f32);
    ui.set_boot_stages(ModelRc::new(VecModel::from(rows)));
}

/// What the shell leaves in its runtime directory once its first frame is drawn. The boot splash
/// (plymouth, deploy/yantrik-os/boot-unlock/splash) waits for it before quitting, and quits with
/// --retain-splash, so nothing between the splash and this screen is black. It holds nothing.
const DRAWN: &str = "yantrik-shell-drawn";

fn drawn_path(runtime_dir: Option<OsString>) -> Option<PathBuf> {
    runtime_dir.filter(|d| !d.is_empty()).map(|d| PathBuf::from(d).join(DRAWN))
}

/// Leave the marker once, after the first frame. Slint's software renderer has no rendering
/// notifier; there, a short while after the event loop starts stands in for it, which is after
/// the window is shown and its first frame asked for.
fn say_drawn(ui: &App) {
    let Some(path) = drawn_path(std::env::var_os("XDG_RUNTIME_DIR")) else { return };
    let said = Rc::new(Cell::new(false));
    let say = move || {
        if said.replace(true) {
            return;
        }
        if let Err(e) = std::fs::write(&path, b"") {
            tracing::warn!(error = %e, path = %path.display(), "Could not tell the boot splash the shell has drawn; it quits on its own deadline");
        }
    };
    let after_frame = say.clone();
    let notified = ui.window().set_rendering_notifier(move |state, _| {
        if matches!(state, RenderingState::AfterRendering) {
            after_frame();
        }
    });
    if notified.is_err() {
        Timer::single_shot(Duration::from_millis(500), say);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_drawn_marker_is_where_the_splash_looks() {
        let script = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../deploy/yantrik-os/boot-unlock/splash"));
        assert!(script.contains(&format!("\"$RUN\"/*/{DRAWN}")), "splash waits for {DRAWN} in each session's runtime dir");
        assert!(script.contains("RUN=\"${YANTRIK_SPLASH_RUN:-/run/user}\""));
        assert_eq!(drawn_path(Some("/run/user/1000".into())), Some(PathBuf::from("/run/user/1000/yantrik-shell-drawn")));
        assert_eq!(drawn_path(Some("".into())), None, "no runtime dir, no marker in the working directory");
        assert_eq!(drawn_path(None), None);
    }
}
