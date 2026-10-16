//! A screenshot, from the moment the frame is clean to the file and the clipboard.
//!
//! The bar and the toasts are never in the picture. Whoever asks hides them first
//! ([`super::hide_furniture`]), and the capture waits [`SETTLE`] for the compositor to draw the
//! frame after they are gone: a Wayland client cannot ask when its unmap reached the screen, so
//! the wait is a number of frames, not a signal. The same wait follows `slurp`, whose selection
//! overlay is a surface of its own.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use chrono::Local;

use super::grab::{self, Display, Format};
use super::names::{self, Kind};
use super::Who;

/// Six frames at 60 Hz, three at 30: the bar has no fade, so its last frame is gone well inside
/// this.
pub const SETTLE: Duration = Duration::from_millis(100);

/// How long the person may take to drag a rectangle before the selection is given up.
const SELECT_LIMIT: Duration = Duration::from_secs(300);

/// What part of the display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Area {
    /// The whole display.
    Screen,
    /// The person drags a rectangle with `slurp`.
    Drag,
    /// A rectangle already known, in grim's geometry (a mind's `region`, checked).
    Given(String),
}

/// Where the picture goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keep {
    /// Saved to ~/Pictures/Screenshots and copied to the clipboard: the person's screenshot.
    FileAndClipboard,
    /// The clipboard only (Ctrl+Print). Nothing is written to the home.
    Clipboard,
    /// Saved only: a mind's screenshot, which must not put the person's screen on the person's
    /// clipboard, where the next paste would carry it somewhere they did not mean.
    File,
}

pub struct Shot {
    pub display: Display,
    pub area: Area,
    pub delay: Duration,
    pub pointer: bool,
    pub keep: Keep,
    pub who: Who,
}

/// What came of it.
#[derive(Debug)]
pub enum Taken {
    Saved(PathBuf),
    Copied,
}

/// The person pressed Esc in `slurp`. Not an error anybody needs to be told about.
pub const CANCELLED: &str = "cancelled";

/// Ask the person for a rectangle. `Err(CANCELLED)` when they pressed Esc.
pub fn drag() -> Result<String, String> {
    let mut slurp = Command::new("slurp");
    slurp.env_remove("WAYLAND_SOCKET");
    let out = grab::run_bounded(slurp, SELECT_LIMIT, "the region selector (slurp)").map_err(|e| {
        // slurp exits 1 on Esc, which run_bounded reports as a failure with no words.
        if e.contains("failed") { CANCELLED.to_string() } else { e }
    })?;
    let geometry = String::from_utf8_lossy(&out).trim().to_string();
    if geometry.is_empty() {
        return Err(CANCELLED.into());
    }
    Ok(geometry)
}

/// Take it. Blocking — slurp waits for the person, then the delay, then grim: a worker.
/// The furniture is already hidden by the caller.
pub fn take(shot: Shot) -> Result<Taken, String> {
    let region = match &shot.area {
        Area::Screen => None,
        Area::Given(g) => Some(g.clone()),
        Area::Drag => {
            let g = drag()?;
            std::thread::sleep(SETTLE);
            Some(g)
        }
    };
    std::thread::sleep(shot.delay.max(SETTLE));
    // Nothing is captured while the screen is locked: a timer that ran out after Super+L must
    // not photograph whatever the lock covers.
    if super::locked_now() {
        return Err("the screen locked before the picture was taken, so none was".into());
    }
    let at = Local::now();
    match shot.keep {
        Keep::Clipboard => {
            let temp = private_temp()?;
            let result = grab::grab(shot.display, region.as_deref(), Format::Png, shot.pointer, &temp.0)
                .and_then(|()| copy_to_clipboard(&temp.0));
            result.map(|()| Taken::Copied)
        }
        Keep::File | Keep::FileAndClipboard => {
            let path = names::fresh_path(Kind::Screenshot, &names::home()?, &at)?;
            grab::grab(shot.display, region.as_deref(), Format::Png, shot.pointer, &path)?;
            if shot.keep == Keep::FileAndClipboard {
                // Saved is what matters; a clipboard that could not be reached is logged, and the
                // notification still offers the file.
                if let Err(e) = copy_to_clipboard(&path) {
                    tracing::warn!(error = %e, "The screenshot was saved but not copied to the clipboard");
                }
            }
            tracing::info!(path = %path.display(), by = %shot.who.words(), display = shot.display.name(), "Screenshot saved");
            Ok(Taken::Saved(path))
        }
    }
}

/// `wl-copy --type image/png < file`. wl-copy reads the whole file before it forks to serve the
/// clipboard, so the file may go as soon as this returns.
fn copy_to_clipboard(path: &std::path::Path) -> Result<(), String> {
    let file = std::fs::File::open(path).map_err(|e| format!("could not read the picture: {e}"))?;
    let mut cmd = Command::new("wl-copy");
    cmd.args(["--type", "image/png"]).env_remove("WAYLAND_SOCKET");
    let child = cmd
        .stdin(Stdio::from(file))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start wl-copy: {e}"))?;
    let out = child.wait_with_output().map_err(|e| format!("wl-copy: {e}"))?;
    if !out.status.success() {
        return Err(format!("wl-copy failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(())
}

/// A picture on its way to the clipboard: in the session's runtime directory, only this account
/// can read it, and it is removed however the copy ends.
struct Temp(PathBuf);

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn private_temp() -> Result<Temp, String> {
    use std::os::unix::fs::OpenOptionsExt;
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    let path = PathBuf::from(dir).join(format!(
        "yantrik-clip-{}-{}.png",
        std::process::id(),
        Local::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| format!("could not make a place for the picture: {e}"))?;
    Ok(Temp(path))
}
