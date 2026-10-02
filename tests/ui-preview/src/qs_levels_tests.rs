//! Quick Settings' two levels, drawn by the whole shell (app.slint's `App`) in the two shapes a
//! machine comes in (story 0.2).
//!
//! A VM has no backlight: `brightness-available` is false and the panel has no brightness row
//! at all, only Volume. A laptop has one: both rows, at the levels the machine reported. The
//! two renders are `qs-vm-shape.png` and `qs-laptop-shape.png`, next to the output path given.
//! A machine with no audio server gets no Volume row either, and a muted sink reads "Muted"
//! rather than a percent that is not being heard; pressing that label asks to mute or unmute.
use super::*;
use std::cell::Cell;

fn save(pixels: &slint::SharedPixelBuffer<slint::Rgb8Pixel>, path: &str, width: u32, height: u32) -> Result<(), Box<dyn std::error::Error>> {
    let mut encoder = png::Encoder::new(BufWriter::new(File::create(path)?), width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(pixels.as_bytes())?;
    println!("Rendered {path} ({width}×{height})");
    Ok(())
}

pub fn run(w: &MinimalSoftwareWindow, output: &str) -> Result<(), Box<dyn std::error::Error>> {
    let (width, height) = (1280u32, 800u32);
    let ui = App::new()?;
    ui.set_current_screen(1);
    ui.show()?;
    w.set_size(slint::PhysicalSize::new(width, height));
    let draw = || {
        let mut pixels = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(width, height);
        for _ in 0..2 {
            slint::platform::update_timers_and_animations();
            w.request_redraw();
            w.draw_if_needed(|r| { r.render(pixels.make_mut_slice(), width as usize); });
        }
        pixels
    };
    // Nothing is invented: before the machine has been read, no row is drawn and no level is set.
    assert_eq!(ui.get_volume_level(), 0, "no invented starting volume");
    assert_eq!(ui.get_brightness_level(), 0, "no invented starting brightness");
    assert!(!ui.get_brightness_available() && !ui.get_volume_available());

    ui.set_quick_settings_open(true);

    // VM shape: audio, no backlight.
    ui.set_volume_available(true);
    ui.set_volume_level(45);
    ui.set_brightness_available(false);
    let vm = draw();
    let dir = std::path::Path::new(output);
    let named = |name: &str| dir.with_file_name(name).to_string_lossy().into_owned();
    save(&vm, &named("qs-vm-shape.png"), width, height)?;

    // Laptop shape: both.
    ui.set_brightness_available(true);
    ui.set_brightness_level(70);
    let laptop = draw();
    save(&laptop, &named("qs-laptop-shape.png"), width, height)?;

    // The panel is 380px wide, centred under the status bar, so x=460 is inside it and clear of
    // every control. The rows of that column that differ between two renders are where the
    // panels differ: the laptop's has the brightness row, the VM's does not, so about a row's
    // height of the column changes and the VM's panel ends higher.
    let rows_differing = |a: &slint::SharedPixelBuffer<slint::Rgb8Pixel>, b: &slint::SharedPixelBuffer<slint::Rgb8Pixel>| -> Vec<usize> {
        (0..height as usize)
            .filter(|&y| a.as_slice()[y * width as usize + 460] != b.as_slice()[y * width as usize + 460])
            .collect()
    };
    let brightness_row = rows_differing(&vm, &laptop);
    assert!(
        brightness_row.len() > 40,
        "the laptop panel is taller than the VM's by the brightness row: only {} rows of the panel differ",
        brightness_row.len()
    );
    let panel_bottom = *brightness_row.last().unwrap() as i32;

    // A muted sink: the label says so, and pressing it asks to toggle.
    let pressed: Rc<Cell<u32>> = Rc::default();
    let p = pressed.clone();
    ui.on_volume_mute_toggled(move || p.set(p.get() + 1));
    ui.set_volume_muted(true);
    let muted = draw();
    save(&muted, &named("qs-laptop-muted.png"), width, height)?;
    assert!(muted.as_slice() != laptop.as_slice(), "a muted sink is drawn differently from a level");
    // The label sits at the right edge of the Volume row: found by sweeping down the column it
    // is in, reopening the panel after each press (a press on bare panel closes it).
    let mut found = false;
    for y in (60..panel_bottom).step_by(4) {
        ui.set_quick_settings_open(true);
        draw();
        click(w, 782.0, y as f32);
        draw();
        if pressed.get() > 0 {
            found = true;
            break;
        }
    }
    assert!(found, "pressing the Muted label asks to mute or unmute");

    // No audio server: the Volume row goes too, rather than a level nobody measured.
    ui.set_quick_settings_open(true);
    ui.set_volume_available(false);
    ui.set_brightness_available(false);
    let nothing = draw();
    assert!(rows_differing(&nothing, &vm).len() > 40, "with neither, the panel is shorter again");

    ui.hide()?;
    println!(
        "PASS: Quick Settings draws no brightness row without a backlight, no volume row without an audio server, shows Muted for a muted sink, and starts at no invented level"
    );
    Ok(())
}
