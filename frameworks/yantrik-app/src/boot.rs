//! The first lines every app process runs: `--version`, the fullscreen guard, tracing and the
//! panic hook. The runner does this once; `yantrik_app_runtime::init_tracing` is this function.

use crate::problems;

/// Initialize tracing-subscriber with an env filter for the app, after answering `--version`.
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

    // A panic becomes a problem record before it becomes a stack trace on stderr. The record
    // is local and scrubbed; nothing sends it. See `problems`.
    problems::install_panic_hook(app_name);

    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;
    let filter = tracing_subscriber::EnvFilter::from_default_env()
        .add_directive(directive.parse().expect("valid tracing directive"))
        // The framework's own lines (instance guard, theme) must be visible too, or an app that
        // exits at once because another instance holds the slot says nothing.
        .add_directive("yantrik_app_runtime=info".parse().expect("valid directive"))
        .add_directive("yantrik_app=info".parse().expect("valid directive"));
    // A second init (a test harness after the app) is not an error worth stopping for.
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        // The same lines, kept in a small ring so a problem record can carry the last forty.
        .with(RecentLines)
        .try_init();
}

/// A tracing layer that remembers the most recent log lines of this process, for the
/// `log_tail` of a problem record. It formats nothing to a sink; `problems::note_log_line` holds
/// a bounded ring and that is all.
struct RecentLines;

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for RecentLines {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        struct Line(String);
        impl tracing::field::Visit for Line {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                use std::fmt::Write as _;
                if field.name() == "message" {
                    let _ = write!(self.0, "{value:?}");
                } else {
                    let _ = write!(self.0, " {}={:?}", field.name(), value);
                }
            }
        }
        let mut line = Line(String::new());
        event.record(&mut line);
        let meta = event.metadata();
        problems::note_log_line(&format!("{} {}: {}", meta.level(), meta.target(), line.0));
    }
}

/// The software renderer when the session asks for it, chosen before any component exists. One
/// implementation of the five-line switch seven apps carried.
pub fn choose_backend() {
    let software = std::env::var("LIBGL_ALWAYS_SOFTWARE")
        .map(|v| v == "1")
        .unwrap_or(false);
    let backend = std::env::var("SLINT_BACKEND").unwrap_or_default();
    if software && (backend.is_empty() || backend == "winit") {
        std::env::set_var("SLINT_BACKEND", "winit-software");
    }
}
