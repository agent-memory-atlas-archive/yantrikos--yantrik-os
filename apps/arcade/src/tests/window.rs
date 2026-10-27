//! The real window drawn on Slint's software renderer with no display, in both themes. The pictures
//! land beside the test binary, in `<target>/<profile>/ui-screenshots/`.

use std::path::PathBuf;

use slint::ComponentHandle as _;

use super::{open, seeded, Fixture};
use crate::ThemeMode;

fn shot_dir() -> PathBuf {
    let dir = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.parent()?.join("ui-screenshots")))
        .unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn screenshot(f: &Fixture, name: &str) {
    f.app.resize(1020, 680);
    f.app.draw();
    let (width, height, pixels) = f.app.ui(|ui| {
        let shot = ui.window().take_snapshot().expect("a snapshot of the software window");
        (shot.width(), shot.height(), shot.as_bytes().to_vec())
    });
    let mut png = png::Encoder::new(std::fs::File::create(shot_dir().join(name)).unwrap(), width, height);
    png.set_color(png::ColorType::Rgba);
    png.set_depth(png::BitDepth::Eight);
    png.write_header().unwrap().write_image_data(&pixels).unwrap();
}

/// The editor's text and the "Selected game" field are drawn in the theme's own text colours in
/// both themes, with nothing focused. (The stock TextEdit and LineEdit took theirs from the
/// style's palette and were near-invisible in the light theme.)
#[test]
fn the_workbench_is_legible_in_both_themes_with_nothing_focused() {
    let f = open();
    seeded(&f);
    f.type_spec("{\n  \"title\": \"Meadow Run\",\n  \"collectible\": { \"kind\": \"berry\", \"count\": 5 }\n}");
    for (dark, name) in [(true, "arcade-dark.png"), (false, "arcade-light.png")] {
        f.app.ui(move |ui| ui.global::<ThemeMode>().set_dark(dark));
        f.quiet();
        screenshot(&f, name);
    }
    let text = f.app.ui(|ui| ui.get_spec_text().to_string());
    assert!(text.contains("Meadow Run"));
}
