//! YantrikApp: what every app's process is made of (design/app-framework-2026-10-21.md).
//!
//! Step 1 of that design: `yantrik-app-runtime`'s modules, moved here unchanged, so the `App`
//! trait and the runner (step 2) are built beside them and not around them. Apps still reach
//! these through `yantrik_app_runtime`, which re-exports every one, until each moves in its
//! own framework pass.

pub use slint;

pub mod chrome;
pub mod companion;
pub mod control;
pub mod event_loop;
pub mod instance;
pub mod present;
pub mod theme;

pub use event_loop::run_until_closed;
// Moved to Foundation (L0): the one rule for an address, and the problem records every process
// writes. Named here so the modules above keep their `crate::problems` paths.
pub use yantrik_foundation::{place, problems};

/// Held by every test in this crate that sets `XDG_RUNTIME_DIR`, or binds a socket under it.
///
/// The environment is process-wide and the test harness runs tests on parallel threads. A test
/// that had started a server under the runner's `/run/user/<uid>` then looked for its socket
/// where the variable pointed *now*, and lost that race on four pull requests in one night. A
/// poisoned lock is fine to take: whatever the last holder panicked about was its own business.
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
pub(crate) fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}
