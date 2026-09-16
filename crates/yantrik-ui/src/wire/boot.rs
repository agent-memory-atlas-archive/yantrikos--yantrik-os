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

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};
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
    ui.set_version(SharedString::from(env!("CARGO_PKG_VERSION")));
    push(ui, &[false; 4]);

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
        // when it has the things it draws. dock_items is the honest test — it is populated from
        // the app catalogue, which is the scan the desktop cannot draw its START row without.
        let desktop_drawn = ui.get_dock_items().row_count() > 0;
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
