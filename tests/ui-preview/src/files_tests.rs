use super::*;
use slint::platform::{Key, WindowEvent};
fn chord(window: &MinimalSoftwareWindow, modifier: Key, text: impl Into<slint::SharedString>) {
    window.dispatch_event(WindowEvent::KeyPressed {
        text: modifier.into(),
    });
    key(window, text.into());
    window.dispatch_event(WindowEvent::KeyReleased {
        text: modifier.into(),
    });
}
pub fn run(window: &MinimalSoftwareWindow) -> Result<(), Box<dyn std::error::Error>> {
    let ui = FilesProbe::new()?;
    ui.set_entries(slint::ModelRc::new(slint::VecModel::from(
        (0..1000)
            .map(|i| FileEntry {
                name: format!("File {i:04}.txt").into(),
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    )));
    ui.show()?;
    window.set_size(slint::PhysicalSize::new(800, 600));
    let draw = || {
        slint::platform::update_timers_and_animations();
        let mut p = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(800, 600);
        window.request_redraw();
        window.draw_if_needed(|r| { r.render(p.make_mut_slice(), 800); });
    };
    ui.set_loading(true); draw();
    ui.set_loading(false); draw();
    chord(window, Key::Control, "f"); draw(); key(window, "after load".into());
    assert_eq!(ui.get_query(), "after load", "Loading restores keyboard focus");
    key(window, Key::Escape.into()); draw();
    click(window, 280., 210.);
    assert_eq!(ui.get_selected(), 0);
    key(window, Key::End.into());
    draw();
    assert_eq!(ui.get_selected(), 999);
    click(window, 280., 540.);
    assert!(ui.get_selected() > 990, "End scrolls selection into view");
    key(window, Key::Home.into());
    draw();
    assert_eq!(ui.get_selected(), 0);
    chord(window, Key::Shift, Key::DownArrow);
    assert_eq!(ui.get_selected(), 1);
    assert!(ui.get_shift_selected());
    chord(window, Key::Control, "c");
    assert_eq!(ui.get_action(), "copy");
    chord(window, Key::Control, "x");
    assert_eq!(ui.get_action(), "cut");
    chord(window, Key::Control, "v");
    assert_eq!(ui.get_action(), "paste");
    chord(window, Key::Control, "f");
    draw();
    key(window, "Budget".into());
    assert_eq!(ui.get_query(), "Budget");
    key(window, Key::Escape.into());
    draw();
    assert_eq!(ui.get_query(), "");
    chord(window, Key::Control, "f"); draw(); key(window, "fresh".into());
    assert_eq!(ui.get_query(), "fresh", "Clearing search also clears the actual input text");
    key(window, Key::Escape.into()); draw();
    chord(window, Key::Control, "l");
    draw();
    chord(window, Key::Control, "a");
    key(window, "/tmp".into());
    key(window, "\n".into());
    assert_eq!(ui.get_action(), "path:/tmp");
    click(window, 540., 24.);
    draw();
    key(window, "New folder test".into());
    key(window, "\n".into());
    assert_eq!(ui.get_action(), "folder:New folder test");
    ui.set_selected(-1);
    ui.set_grid(true);
    draw();
    click(window, 220., 220.);
    assert_eq!(ui.get_selected(), 0);
    click(window, 390., 220.);
    assert_eq!(ui.get_selected(), 1);
    click(window, 220., 340.);
    assert_eq!(ui.get_selected(), 4);
    key(window, Key::End.into());
    draw();
    assert_eq!(ui.get_selected(), 999);
    click(window, 720., 535.);
    assert!(
        ui.get_selected() > 980,
        "Grid End scrolls selected row into view"
    );
    for _ in 0..5 { slint::platform::update_timers_and_animations(); let mut p=slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(800,600); window.draw_if_needed(|r| {r.render(p.make_mut_slice(),800);}); std::thread::sleep(std::time::Duration::from_millis(100)); }
    let mut redraws=0;
    for _ in 0..10 {slint::platform::update_timers_and_animations(); let mut p=slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(800,600); if window.draw_if_needed(|r| {r.render(p.make_mut_slice(),800);}) {redraws+=1;} std::thread::sleep(std::time::Duration::from_millis(100));}
    assert_eq!(redraws,0,"Idle Files should not request continuous redraws");
    println!("PASS: Files idle with 1000 entries requested zero redraws in one second");
    println!("PASS: Files keyboard scrolling over 1000 entries, Shift selection, clipboard shortcuts, search, location, new-folder dialog and compact grid hit targets");
    Ok(())
}
