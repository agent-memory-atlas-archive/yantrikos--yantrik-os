//! Image Viewer wiring - prev/next navigation, fit toggle, rotate/flip,
//! EXIF metadata, and slideshow control.
//!
//! The rules themselves - which files are pictures, which sit beside this one, what the file
//! says about itself - live in `yantrik-image-core`, because the standalone Images app needs
//! exactly the same answers and used to have none of them.

use std::path::PathBuf;

use slint::ComponentHandle;
use yantrik_image_core::{dimensions_from_size, read_info, Gallery, ImageInfo};

use crate::app_context::AppContext;
use crate::App;

/// State for the image viewer.
#[derive(Default)]
pub struct ImageViewerState {
    gallery: Gallery,
}

impl ImageViewerState {
    /// Open an image file - populates the sibling list and sets current index.
    pub fn open(&mut self, path: &PathBuf) {
        self.gallery = Gallery::open(path);
    }

    /// Current image path.
    pub fn current_path(&self) -> Option<&PathBuf> {
        self.gallery.current()
    }

    /// Navigate to previous image.
    pub fn prev(&mut self) {
        self.gallery.prev();
    }

    /// Navigate to next image.
    pub fn next(&mut self) {
        self.gallery.next();
    }

    /// Counter text like "3 / 12".
    pub fn counter_text(&self) -> String {
        self.gallery.counter_text()
    }
}

/// Apply EXIF info to the UI.
fn apply_exif_info(ui: &App, info: &ImageInfo) {
    ui.set_viewer_exif_dimensions(info.dimensions.clone().into());
    ui.set_viewer_exif_file_size(info.file_size.clone().into());
    ui.set_viewer_exif_format(info.format.clone().into());
    ui.set_viewer_exif_camera(info.camera.clone().into());
    ui.set_viewer_exif_focal_length(info.focal_length.clone().into());
    ui.set_viewer_exif_iso(info.iso.clone().into());
    ui.set_viewer_exif_exposure(info.exposure.clone().into());
    ui.set_viewer_exif_date_taken(info.date_taken.clone().into());
    ui.set_viewer_exif_gps(info.gps.clone().into());
}

/// Load the current image into the UI. Called from callbacks.rs when opening an image.
pub fn load_current_image(ui: &App, state: &ImageViewerState) {
    if let Some(path) = state.current_path() {
        let img = slint::Image::load_from_path(path);
        match img {
            Ok(image) => {
                // Get dimensions from the loaded image if EXIF didn't have them
                let img_size = image.size();
                ui.set_viewer_image(image);
                ui.set_viewer_notice(slint::SharedString::new());

                let mut exif_info = read_info(path);
                if exif_info.dimensions.is_empty() {
                    exif_info.dimensions = dimensions_from_size(img_size.width, img_size.height);
                }
                apply_exif_info(ui, &exif_info);
            }
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "Failed to load image");
                ui.set_viewer_image(slint::Image::default());
                ui.set_viewer_notice(format!("Could not open {}", path.display()).into());
                // Still show file metadata even if image load failed
                let exif_info = read_info(path);
                apply_exif_info(ui, &exif_info);
            }
        }
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        ui.set_viewer_file_name(name.to_string().into());
        ui.set_viewer_counter(state.counter_text().into());

        // Reset rotation/flip when navigating to a new image
        ui.set_viewer_rotation(0);
        ui.set_viewer_flip_h(false);
        ui.set_viewer_flip_v(false);
    }
}

/// Wire image viewer callbacks.
pub fn wire(ui: &App, ctx: &AppContext) {
    let state = ctx.image_viewer_state.clone();

    // Prev
    let ui_weak = ui.as_weak();
    let st = state.clone();
    ui.on_viewer_nav_prev(move || {
        st.borrow_mut().prev();
        if let Some(ui) = ui_weak.upgrade() {
            load_current_image(&ui, &st.borrow());
        }
    });

    // Next
    let ui_weak = ui.as_weak();
    let st = state.clone();
    ui.on_viewer_nav_next(move || {
        st.borrow_mut().next();
        if let Some(ui) = ui_weak.upgrade() {
            load_current_image(&ui, &st.borrow());
        }
    });

    // Fit toggle
    ui.on_viewer_toggle_fit(move || {
        // Handled purely in Slint via two-way binding
    });

    // ── Rotate / Flip callbacks ──
    // Rotation and flip state is tracked in Slint properties.
    // The callbacks allow the backend to react if needed (e.g., saving orientation).
    ui.on_viewer_rotate_left(|| {
        tracing::debug!("Image rotated left (CCW)");
    });
    ui.on_viewer_rotate_right(|| {
        tracing::debug!("Image rotated right (CW)");
    });
    ui.on_viewer_flip_horizontal(|| {
        tracing::debug!("Image flipped horizontally");
    });
    ui.on_viewer_flip_vertical(|| {
        tracing::debug!("Image flipped vertically");
    });

    // ── EXIF Info Panel toggle ──
    let ui_weak = ui.as_weak();
    let st = state.clone();
    ui.on_viewer_toggle_info(move || {
        if let Some(ui) = ui_weak.upgrade() {
            // Re-read EXIF when panel is opened (in case file changed)
            if ui.get_viewer_info_open() {
                let state = st.borrow();
                if let Some(path) = state.current_path() {
                    let mut exif_info = read_info(path);
                    // Try to get dimensions from current image if EXIF doesn't have them
                    if exif_info.dimensions.is_empty() {
                        let img = ui.get_viewer_image();
                        let size = img.size();
                        if size.width > 0 && size.height > 0 {
                            exif_info.dimensions = format!("{} \u{00d7} {}", size.width, size.height);
                        }
                    }
                    apply_exif_info(&ui, &exif_info);
                }
            }
        }
    });

    // The crop and batch handlers are gone with their buttons. They logged, set a progress
    // bar and reported "(logged)" in the status line while every file on disk stayed exactly
    // as it was.


    // ── Slideshow callbacks ──
    ui.on_viewer_slideshow_toggle(|| {
        tracing::debug!("Slideshow toggled");
    });
    ui.on_viewer_slideshow_stop(|| {
        tracing::debug!("Slideshow stopped");
    });

    // ── AI Describe callback ──
    let bridge = ctx.bridge.clone();
    let ai_state = super::ai_assist::AiAssistState::new();
    let ui_weak = ui.as_weak();
    let ai_st = ai_state.clone();
    ui.on_viewer_ai_describe(move || {
        let Some(ui) = ui_weak.upgrade() else { return };

        let filename = ui.get_viewer_file_name().to_string();
        if filename.is_empty() { return; }

        let prompt = super::ai_assist::image_describe_prompt(&filename);

        super::ai_assist::ai_request(
            &ui.as_weak(),
            &bridge,
            &ai_st,
            super::ai_assist::AiAssistRequest {
                prompt,
                timeout_secs: 30,
                set_working: Box::new(|ui, v| ui.set_viewer_ai_is_working(v)),
                set_response: Box::new(|ui, s| ui.set_viewer_ai_response(s.into())),
                get_response: Box::new(|ui| ui.get_viewer_ai_response().to_string()),
            },
        );
    });

    let ui_weak = ui.as_weak();
    ui.on_viewer_ai_dismiss(move || {
        if let Some(ui) = ui_weak.upgrade() {
            ui.set_viewer_ai_panel_open(false);
        }
    });
}
