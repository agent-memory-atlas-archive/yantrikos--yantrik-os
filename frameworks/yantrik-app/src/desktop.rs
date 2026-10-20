//! The desktop, through one typed door (design/app-framework-2026-10-21.md, §8): opening a URL
//! or a file with whatever the person uses for it, and putting text on the clipboard. An app asks
//! with `fx.desktop(request, |outcome| Command)`; the work runs as a job, and the outcome comes
//! back as the command, so an `update` never spawns a process or blocks on one. File pickers
//! and drag-and-drop join here when the first app on the framework needs them (§10).

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// What is asked of the desktop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopRequest {
    /// Open a web address in the person's browser. `http`, `https` and `mailto` only.
    OpenUrl(String),
    /// Open a file or folder with what the person uses for its kind.
    OpenPath(PathBuf),
    /// Put text on the clipboard.
    CopyText(String),
}

/// What came of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopOutcome {
    Done,
    /// It did not happen, with the sentence to show.
    Failed(String),
}

/// The schemes an app may hand to the browser. Anything else (`file:`, `javascript:`, a custom
/// scheme that launches a program) is refused here, before a process is started.
const OPENABLE_SCHEMES: [&str; 3] = ["http://", "https://", "mailto:"];

/// Does the request, blocking: only ever called on a job thread.
pub fn perform(request: DesktopRequest) -> DesktopOutcome {
    match request {
        DesktopRequest::OpenUrl(url) => open_url(&url),
        DesktopRequest::OpenPath(path) => open_path(&path),
        DesktopRequest::CopyText(text) => copy_text(&text),
    }
}

fn open_url(url: &str) -> DesktopOutcome {
    let url = url.trim();
    if url.is_empty() || url.len() > 8 * 1024 {
        return DesktopOutcome::Failed("That is not a web address.".into());
    }
    let lower = url.to_ascii_lowercase();
    if !OPENABLE_SCHEMES.iter().any(|s| lower.starts_with(s)) {
        return DesktopOutcome::Failed(
            "Only web (http, https) and mail addresses are opened from here.".into(),
        );
    }
    if url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return DesktopOutcome::Failed("That is not a web address.".into());
    }
    launch("xdg-open", url)
}

fn open_path(path: &std::path::Path) -> DesktopOutcome {
    if !path.is_absolute() {
        return DesktopOutcome::Failed("A file is opened by its full path.".into());
    }
    if !path.exists() {
        return DesktopOutcome::Failed(format!(
            "{} is not there any more.",
            yantrik_foundation::home::shown(path)
        ));
    }
    launch("xdg-open", &path.to_string_lossy())
}

/// Starts the opener and lets it go: a browser may run for an hour, and the app does not wait
/// for it. Failing to start it is the one failure there is to report.
fn launch(program: &str, argument: &str) -> DesktopOutcome {
    match Command::new(program)
        .arg(argument)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(_) => DesktopOutcome::Done,
        Err(e) => DesktopOutcome::Failed(format!("Nothing on this machine opens that ({e}).")),
    }
}

fn copy_text(text: &str) -> DesktopOutcome {
    use std::io::Write as _;
    let mut child = match Command::new("wl-copy")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return DesktopOutcome::Failed(format!("The clipboard is not available ({e})."));
        }
    };
    if let Some(mut stdin) = child.stdin.take() {
        if let Err(e) = stdin.write_all(text.as_bytes()) {
            return DesktopOutcome::Failed(format!("The text could not be copied ({e})."));
        }
    }
    match child.wait() {
        Ok(status) if status.success() => DesktopOutcome::Done,
        Ok(status) => DesktopOutcome::Failed(format!("The clipboard refused it ({status}).")),
        Err(e) => DesktopOutcome::Failed(format!("The text could not be copied ({e})."),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Refused before any process is started: nothing here opens a browser on the test machine.
    #[test]
    fn what_is_refused_before_anything_is_started() {
        for bad in ["", "file:///etc/passwd", "javascript:alert(1)", "ftp://x", "http://a b", " "] {
            assert!(
                matches!(open_url(bad), DesktopOutcome::Failed(_)),
                "{bad:?} must be refused"
            );
        }
        assert!(matches!(
            open_path(std::path::Path::new("relative/thing")),
            DesktopOutcome::Failed(_)
        ));
        let gone = std::env::temp_dir().join("yantrik-desktop-no-such-file");
        let DesktopOutcome::Failed(said) = open_path(&gone) else { panic!("failed") };
        assert!(said.contains("not there"), "{said}");
    }
}
