//! What an app costs with nobody touching it: the frames its windows ask for, the timers that
//! fire and the jobs they start, over a span of the virtual clock (the kit's rule: nothing
//! redraws at rest).
//!
//! A window asks to be drawn when a property one of its frames read has changed: a model set to
//! a new `ModelRc` with the same rows in it, a string set to a new value, an animation in flight.
//! None of those shows in a screenshot or in a functional test; each is a full repaint on the
//! software rasteriser of a machine with no GPU. So the check is the one the display makes: draw
//! the window whenever it asks, and count how often that was.
//!
//! Slint's own timers and animations keep real time, not the virtual clock; an animation that
//! never ends asks for a frame at every step, and is counted the same way.

use std::time::{Duration, Instant};

use slint::ComponentHandle as _;
use yantrik_app::app::App;

use crate::{headless, TestApp};

/// How long a finite animation already in flight is given to end before a span is measured.
const SETTLE_FOR: Duration = Duration::from_secs(2);

/// The most jobs one step of the clock may start, one after another, before the span is called
/// a loop: a job whose result starts the next at once is never at rest.
const JOBS_PER_STEP: usize = 64;

/// What happened over a span of the virtual clock with no one touching the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AtRest {
    /// Frames the app's windows asked for and were drawn.
    pub frames: usize,
    /// Timers that fired, the app's and the framework's.
    pub fired: usize,
    /// Jobs that ran: what the timers started (a poll, a re-read), run as the runner's pool
    /// would, with what they returned applied.
    pub jobs: usize,
}

impl<A: App> TestApp<A> {
    /// Shows every open window at `width` × `height` physical pixels, as a person's window would
    /// be: the layout that decides what is on screen (a rail beside the content, or not) is the
    /// one a frame then reads.
    pub fn resize(&self, width: u32, height: u32) {
        self.with(move |u| {
            for id in u.core.windows_open() {
                if let Some(ui) = u.core.window(id) {
                    let _ = ui.show();
                    ui.window().set_size(slint::PhysicalSize::new(width, height));
                }
            }
        });
    }

    /// Draws each open window that asked to be drawn, after Slint's timers and animations have
    /// moved on: how many drew.
    pub fn draw(&self) -> usize {
        self.with(|u| {
            slint::platform::update_timers_and_animations();
            u.core
                .windows_open()
                .into_iter()
                .filter_map(|id| u.core.window(id))
                .filter(|ui| headless::draw_if_needed(ui.window()))
                .count()
        })
    }

    /// Everything owed drawn, and an animation already in flight (a theme's fade, a popover
    /// opening) given [`SETTLE_FOR`] of real time to end: `true` once a window asks for nothing.
    pub fn settle_frames(&self) -> bool {
        let until = Instant::now() + SETTLE_FOR;
        while Instant::now() < until {
            if self.draw() == 0 {
                return true;
            }
            std::thread::sleep(Duration::from_millis(16));
        }
        false
    }

    /// Moves the virtual clock over `span` in `step`s with nobody touching the app; after each
    /// step the jobs its timers started run and land, and whatever asks to be drawn is drawn:
    /// what that cost. What was owed before the span (the first frame, a reading that just
    /// landed) is drawn first and not counted.
    pub fn at_rest(&self, span: Duration, step: Duration) -> AtRest {
        assert!(!step.is_zero(), "a step of the clock moves it");
        self.settle_frames();
        let mut rest = AtRest::default();
        let mut moved = Duration::ZERO;
        while moved < span {
            let by = step.min(span - moved);
            rest.fired += self.advance_counting(by);
            let mut ran = 0;
            while self.run_one_job() {
                ran += 1;
                assert!(ran <= JOBS_PER_STEP, "{} jobs in one step at rest: one starts the next", ran);
            }
            rest.jobs += ran;
            rest.frames += self.draw();
            moved += by;
        }
        rest
    }
}
