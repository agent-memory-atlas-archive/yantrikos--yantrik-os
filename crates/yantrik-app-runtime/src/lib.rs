//! Yantrik App Runtime — shared infrastructure for standalone app binaries.
//!
//! Provides:
//! - `AppBuilder` for setting up standalone Slint app windows
//! - IPC client for communicating with the shell and services
//! - Common re-exports (Slint, serde_json, tracing, IPC types)
//!
//! # Quick start
//!
//! ```rust,ignore
//! use yantrik_app_runtime::prelude::*;
//!
//! slint::include_modules!();
//!
//! fn main() {
//!     init_tracing("notes");
//!     let app = NotesApp::new().unwrap();
//!     // wire callbacks...
//!     app.run().unwrap();
//! }
//! ```

// ── Re-exports ──────────────────────────────────────────────────────
pub use slint;
pub use serde_json;
pub use tracing;
pub use yantrik_ipc_contracts;
pub use yantrik_ipc_transport;

pub use yantrik_ipc_transport::SyncRpcClient;

pub mod companion;
pub mod control;
pub mod instance;
pub mod notify;
pub mod service;
pub mod theme;

/// Held by every test in this crate that sets `XDG_RUNTIME_DIR`, or binds a socket under it.
///
/// The environment is process-wide and the test harness runs tests on parallel threads. Two
/// `notify` tests point the variable at `/tmp` for their own reasons; a test that had started
/// a server under the runner's `/run/user/<uid>` then looked for its socket where the variable
/// pointed *now*. On GitHub's runners that lost the race on four pull requests in one night,
/// none of which touched this crate, and passed on every rerun. A poisoned lock is fine to
/// take: whatever the last holder panicked about was its own business.
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
pub(crate) fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Commonly-needed imports for app authors.
pub mod prelude {
    pub use crate::{companion, control, init_tracing, instance, notify, service, theme, SyncRpcClient};
    pub use serde_json;
    pub use slint;
    pub use tracing;
}

/// Initialize tracing-subscriber with an env filter for the app.
pub fn init_tracing(app_name: &str) {
    // `--version`, answered here because this is the one line every app binary runs first.
    //
    // A machine used to give three answers about what it was, one of them a hardcoded
    // `Yantrik Terminal v0.1.0` printed into the terminal's first pane. Sixteen app mains each
    // reporting their own `CARGO_PKG_VERSION` is the same defect with more places to forget, so
    // the flag is handled once, from the string yantrik-version resolves. Same reasoning as the
    // SLINT_FULLSCREEN line below: the shared line is the only place a rule holds everywhere.
    yantrik_version::handle_version_flag(app_name);

    // An app is not the OS.
    //
    // The session exports SLINT_FULLSCREEN=1 because the shell IS the desktop and must not be a
    // window on something else. Children inherit the environment, so any launch path that
    // forgets to strip it hands an app a fullscreen window with no titlebar, no taskbar and no
    // way to close it. That happened: the Apps grid spawned a bare Command, and Notes opened
    // over the whole screen with nothing to press.
    //
    // The launcher strips it, and now so does this. Belt and braces, because the cost of the
    // belt failing is a window the user cannot get out of, and the cost of this line is
    // nothing: no app in this OS ever wants to start fullscreen.
    std::env::remove_var("SLINT_FULLSCREEN");

    let crate_name = app_name.replace('-', "_");
    let directive = format!("{crate_name}=info");

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(directive.parse().expect("valid tracing directive"))
                // The runtime's own lines (instance guard, theme) must be visible too, or an
                // app that exits at once because another instance holds the slot says nothing.
                .add_directive("yantrik_app_runtime=info".parse().expect("valid directive")),
        )
        .init();
}

// Note: build-time helpers (slint_config) live in each app's build.rs
// since slint_build is a build-dependency, not a runtime dependency.
