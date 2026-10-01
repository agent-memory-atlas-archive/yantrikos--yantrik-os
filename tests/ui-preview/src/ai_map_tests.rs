//! Settings → AI & Intelligence opens on "What runs on what". On VM 520 the page showed an
//! endpoint above "No AI providers configured" while five minds ran on Ollama Cloud and nothing
//! said so. The rows here are what runs_on::resolve makes of 520's facts (pinned by its
//! vm_520 unit test); this checks they are drawn, first on the page.

use super::*;
use slint::{ModelRc, VecModel};

fn row(name: &str, state: &str, runs_on: &str, source: &str) -> RunsOnRow {
    RunsOnRow { name: name.into(), state: state.into(), answering: state == "Answering", runs_on: runs_on.into(), source: source.into() }
}

pub fn run(w: &MinimalSoftwareWindow, output: &str) -> Result<(), Box<dyn std::error::Error>> {
    let ui = AiMapProbe::new()?;
    ui.show()?;
    w.set_size(slint::PhysicalSize::new(1280, 800));
    let draw = || {
        let mut p = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(1280, 800);
        for _ in 0..3 {
            slint::platform::update_timers_and_animations();
            w.request_redraw();
            w.draw_if_needed(|r| { r.render(p.make_mut_slice(), 1280); });
        }
        p
    };
    let before = draw();
    let own = "its own settings \u{b7} as it reported";
    ui.set_rows(ModelRc::new(VecModel::from(vec![
        row("Yantrik Mind", "Answering", "Ollama Cloud \u{b7} deepseek-v4.1-flash", own),
        row("Yantrik Companion", "Built in", "Custom endpoint \u{b7} aig.mycluster.cyou \u{b7} qwen3.8:27b", "set in /opt/yantrik/config.yaml"),
        row("DeepSeek", "Attached", "Ollama Cloud \u{b7} deepseek-v4.1-flash", own),
        row("Hermes Agent", "Attached", "provider not reported \u{b7} deepseek-v4.1-flash", own),
        row("OpenClaw", "Attached", "Ollama Cloud \u{b7} kimi-k3", own),
        row("Pi", "Attached", "Ollama Cloud \u{b7} deepseek-v4.1-flash", own),
    ])));
    ui.set_summary("Ollama Cloud runs 4 minds. Custom endpoint \u{b7} aig.mycluster.cyou runs 1 mind. 1 mind does not say what it runs on. No mind here runs on a signed-in account.".into());
    let after = draw();
    // The map is the first thing under the page title: the band below the title must change.
    let (x0, x1, y0, y1) = (300, 1200, 170, 420);
    let changed = (y0..y1)
        .flat_map(|y| (x0..x1).map(move |x| y * 1280 + x))
        .filter(|&i| before.as_slice()[i] != after.as_slice()[i])
        .count();
    let f = BufWriter::new(File::create(output)?);
    let mut e = png::Encoder::new(f, 1280, 800);
    e.set_color(png::ColorType::Rgb);
    e.set_depth(png::BitDepth::Eight);
    e.write_header()?.write_image_data(after.as_bytes())?;
    assert!(changed > 15_000, "the map did not draw at the top of the AI page ({changed} pixels changed)");
    println!("PASS ai map: six minds, what each runs on and where it is set, first on the AI page ({changed} pixels)");
    Ok(())
}
