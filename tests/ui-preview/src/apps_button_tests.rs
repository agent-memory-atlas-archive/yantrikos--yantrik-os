//! The taskbar's Apps button, pressed on the screens it did nothing on (#219): the whole shell,
//! app.slint's App, with a real pointer press on the button. The launcher is drawn on the desktop
//! screen, so from Files, Notifications, Recipes or Settings the press flipped a property nothing
//! drew. Now it goes to the desktop, puts the Lens away, and the launcher is on screen.
use super::*;

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
    draw();

    // The button is the first thing on the taskbar, at its left end. Found by pressing along the
    // taskbar from the left until the launcher opens, so a change of padding does not break this.
    let taskbar_y = height as f32 - 20.0;
    let mut button_x = None;
    for x in (6..160).step_by(6) {
        click(w, x as f32, taskbar_y);
        draw();
        if ui.get_app_grid_open() {
            button_x = Some(x as f32);
            break;
        }
    }
    let button_x = button_x.expect("the Apps button opens the launcher from the desktop");
    click(w, button_x, taskbar_y);
    draw();
    assert!(!ui.get_app_grid_open(), "a second press closes it again");

    // Files, Notifications, Recipes, Settings: the screens the live tour pressed it on.
    for (screen, name) in [(8, "Files"), (9, "Notifications"), (35, "Recipes"), (7, "Settings")] {
        ui.set_app_grid_open(false);
        ui.set_current_screen(screen);
        ui.set_lens_open(true);
        let before = draw();
        click(w, button_x, taskbar_y);
        let after = draw();
        assert!(ui.get_app_grid_open(), "the Apps button opens the launcher from {name}");
        assert_eq!(ui.get_current_screen(), 1, "from {name} it goes to the desktop, where the launcher is drawn");
        assert!(!ui.get_lens_open(), "and from {name} it puts the Lens away, which would cover the launcher");
        // It is on screen: the middle of the display changed.
        let (x0, x1, y0, y1) = (320usize, 960usize, 200usize, 600usize);
        let changed = (y0..y1)
            .flat_map(|y| (x0..x1).map(move |x| y * width as usize + x))
            .filter(|&i| before.as_slice()[i] != after.as_slice()[i])
            .count();
        assert!(changed > 20_000, "from {name}, the launcher is drawn: only {changed} pixels changed");
        if screen == 8 {
            save(&after, output, width, height)?;
        }
    }

    println!(
        "PASS: the Apps button opens the launcher from the desktop and closes it again; from Files, Notifications, Recipes and Settings it goes to the desktop, puts the Lens away and the launcher is drawn"
    );
    Ok(())
}
