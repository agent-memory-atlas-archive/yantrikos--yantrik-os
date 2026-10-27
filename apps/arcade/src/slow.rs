//! The three commands that reach a browser, behind one seam: the real browser, or under a test a
//! stand-in that answers when the test says. Blocking: from a job only.

use std::path::Path;

use crate::command::Verb;
use crate::library::Library;
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
        match verb {
            Verb::Verify => {
                let report = verify::verify_game(html, Some(&shot))?;
                let value = serde_json::to_value(&report).map_err(|e| e.to_string())?;
                library.write_verify(slug, &value)?;
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
            Verb::Screenshot => verify::screenshot_game(html, &shot).map(|()| format!("{} \u{2192} {}", slug, shot.display())),
            // The same file `screenshot` writes: if the Browser cannot draw the game, play renders
            // it headless there and says so.
            Verb::Play => play::play(html, &shot),
            other => Err(format!("{} is not a browser command", other.name())),
        }
    }
}

/// What an app started at `paths` reaches its browser through: the real one, or a test's.
#[cfg(not(test))]
pub fn for_app(_: &yantrik_app::Paths) -> std::sync::Arc<dyn Slow> {
    std::sync::Arc::new(Real)
}

#[cfg(test)]
pub use crate::tests::slow_for as for_app;
