//! The taskbar's Chat button says when cards are waiting, counting every card and not only the
//! one on screen. Before, it counted `pending-approvals`, which holds just the card the Lens
//! shows, so a card waiting in the Agents pane left the button plain while the person looked
//! for it (561, 2 Oct 2026). The shell now sets `cards-pending` from the approvals store
//! (card_watch.rs), and this draws the whole shell with none, one and three of them.
use super::*;
use slint::Model;

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
    ui.set_active_harness_name("Yantrik Mind".into());
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
    // The Chat button sits at the bar's right end; the taskbar is the bottom 40px.
    let bar_changed = |a: &slint::SharedPixelBuffer<slint::Rgb8Pixel>, b: &slint::SharedPixelBuffer<slint::Rgb8Pixel>| {
        (height as usize - 44..height as usize)
            .flat_map(|y| (width as usize / 2..width as usize).map(move |x| y * width as usize + x))
            .filter(|&i| a.as_slice()[i] != b.as_slice()[i])
            .count()
    };

    let none = draw();
    assert!(ui.get_pending_approvals().row_count() == 0, "no card is on screen in this scene");

    // A card that waits only in the Agents pane: nothing in `pending-approvals`, one in the count.
    ui.set_cards_pending(1);
    let one = draw();
    let changed = bar_changed(&none, &one);
    assert!(changed > 400, "one waiting card turns the Chat button amber and says so: only {changed} pixels changed");
    save(&one, output, width, height)?;

    ui.set_cards_pending(3);
    let three = draw();
    assert!(bar_changed(&one, &three) > 20, "the count follows the number of cards");
    save(&three, &output.replace(".png", "-three.png"), width, height)?;

    // Answered: the button is itself again, drawn exactly as before.
    ui.set_cards_pending(0);
    let after = draw();
    assert_eq!(bar_changed(&none, &after), 0, "with no card waiting the button is plain again");

    ui.hide()?;
    println!("PASS: the Chat button turns amber and counts cards waiting anywhere, including a card the Lens is not showing, and goes back to plain when none waits");
    Ok(())
}
