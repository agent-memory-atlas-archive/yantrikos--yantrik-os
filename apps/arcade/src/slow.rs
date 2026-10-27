//! The three commands that reach a browser, behind one seam: the real browser, or under a test a
//! stand-in that answers when the test says. Blocking: from a job only.

use std::path::Path;

use crate::command::Verb;
use crate::library::{stamp_of, Library};
use crate::{play, verify};

pub trait Slow: Send + Sync {
    /// Runs `verb` (one of play, verify, screenshot) on the built game `slug` whose file is
    /// `html`: the verdict in a sentence, or why it failed.
    fn run(&self, verb: Verb, library: &Library, slug: &str, html: &Path) -> Result<String, String>;
}

/// The Browser and the headless verifier.
#[cfg_attr(test, allow(dead_code))]
pub struct Real;

impl Slow for Real {
    fn run(&self, verb: Verb, library: &Library, slug: &str, html: &Path) -> Result<String, String> {
        let shot = library.screenshot_path(slug);
        // The build this run judges. If the game is built again or removed while the browser is
        // at work, what it found is about a file that is gone, and is dropped.
        let judged = stamp_of(html);
        // The screenshot as it was before this run, so a run that does not finish removes only
        // what it wrote itself, never an older finished job's.
        let before = stamp_of(&shot);
        let discard = || discard_own_shot(&shot, &before);
        let changed = || {
            discard();
            format!("{slug} was built again or removed while it was being checked, so that result was dropped; run it again")
        };
        // Evaluated once: stopped because the app is closing, so what the browser had found by
        // then is not a result, and none is kept.
        let stopped = |discard: &dyn Fn()| {
            if crate::browser::cancelled() {
                discard();
                return Some(Err(crate::browser::STOPPED.to_string()));
            }
            None
        };
        match verb {
            Verb::Verify => {
                let ran = verify::verify_game(html, Some(&shot));
                if let Some(stopped) = stopped(&discard) {
                    return stopped;
                }
                let report = ran?;
                let value = serde_json::to_value(&report).map_err(|e| e.to_string())?;
                if stamp_of(html) != judged {
                    return Err(changed());
                }
                library.write_verdict(slug, &value, &judged)?;
                // The verifier's own sentence, so the job, the games list and the banner agree,
                // and so an inconclusive run says it was the machine rather than reading as a
                // verdict on the game. It is still not a pass: the job lands as failed until a
                // machine settles it.
                if report.passed {
                    Ok(format!("{slug}: {}", report.summary_line()))
                } else {
                    Err(format!("{slug}: verification {}", report.summary_line()))
                }
            }
            Verb::Screenshot => {
                let ran = verify::screenshot_game(html, &shot);
                if let Some(stopped) = stopped(&discard) {
                    return stopped;
                }
                ran?;
                if stamp_of(html) != judged {
                    return Err(changed());
                }
                Ok(format!("{} \u{2192} {}", slug, shot.display()))
            }
            // The same file `screenshot` writes: if the Browser cannot draw the game, play renders
            // it headless there and says so.
            Verb::Play => play::play(html, &shot),
            other => Err(format!("{} is not a browser command", other.name())),
        }
    }
}

/// Removes `shot` if this run wrote it: its stamp is not the one it had before the run started. A
/// screenshot an earlier finished job left, which this run never touched, stays.
pub(crate) fn discard_own_shot(shot: &Path, before: &crate::library::Stamp) {
    if &stamp_of(shot) != before {
        let _ = std::fs::remove_file(shot);
    }
}

/// What an app started at `paths` reaches its browser through: the real one, or a test's.
#[cfg(not(test))]
pub fn for_app(_: &yantrik_app::Paths) -> std::sync::Arc<dyn Slow> {
    std::sync::Arc::new(Real)
}

#[cfg(test)]
pub use crate::tests::slow_for as for_app;

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("arcade-shot-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A run cancelled after it wrote its screenshot leaves none behind.
    #[test]
    fn a_run_that_did_not_finish_removes_the_screenshot_it_wrote() {
        let dir = folder("own");
        let shot = dir.join("screenshot.png");
        let before = stamp_of(&shot);
        std::fs::write(&shot, b"this run").unwrap();
        discard_own_shot(&shot, &before);
        assert!(!shot.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An older finished job's screenshot, which the run never touched, is left alone.
    #[test]
    fn an_older_jobs_screenshot_is_not_removed() {
        let dir = folder("older");
        let shot = dir.join("screenshot.png");
        std::fs::write(&shot, b"an earlier job").unwrap();
        let before = stamp_of(&shot);
        discard_own_shot(&shot, &before);
        assert_eq!(std::fs::read(&shot).unwrap(), b"an earlier job");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
