//! The Agents screen and an agent's own window, drawn by the production components from fixture
//! data, with real pointer events: the rows do not move under the pointer because the screen tells
//! the shell when the pointer is over the list, a row and a tab select, and a card opens.
use super::*;
use slint::{ModelRc, VecModel};
use std::cell::RefCell;

fn runs(lines: &[(&str, (u8, u8, u8), bool)]) -> ModelRc<AgentRunData> {
    let bg = slint::Color::from_rgb_u8(16, 23, 30);
    ModelRc::new(VecModel::from(
        lines
            .iter()
            .enumerate()
            .map(|(row, (text, fg, bold))| AgentRunData {
                text: (*text).into(),
                row: row as i32,
                col: 0,
                columns: text.chars().count() as i32,
                fg: slint::Color::from_rgb_u8(fg.0, fg.1, fg.2),
                bg,
                bold: *bold,
            })
            .collect::<Vec<_>>(),
    ))
}

fn call(name: &str, target: &str, summary: &str, arguments: &str, status: &str, output: &str) -> ToolCallData {
    ToolCallData {
        name: name.into(),
        target: target.into(),
        summary: summary.into(),
        arguments: arguments.into(),
        status: status.into(),
        output: output.into(),
    }
}

/// What the shell would put in the global for pi, two minutes into tidying a photos folder.
fn fill(g: &AgentsState, popped: bool) {
    const PLAIN: (u8, u8, u8) = (222, 230, 239);
    const GREEN: (u8, u8, u8) = (130, 207, 156);
    let tab = |id: &str, label: &str, count: i32| AgentTabData { id: id.into(), label: label.into(), count };
    g.set_tabs(ModelRc::new(VecModel::from(vec![
        tab("active", "Active", 2),
        tab("needs_you", "Needs you", 1),
        tab("complete", "Complete", 4),
        tab("all", "All", 6),
    ])));
    let row = |id: &str, mind: &str, title: &str, state: &str, label: &str, since: &str| AgentRowData {
        id: id.into(),
        mind: mind.into(),
        title: title.into(),
        state: state.into(),
        label: label.into(),
        since: since.into(),
        parent: "".into(),
    };
    g.set_rows(ModelRc::new(VecModel::from(vec![
        row("deepseek:main", "DeepSeek", "release notes for 0.4", "waiting_for_you", "waiting for you", "40s"),
        row("pi:main", "pi", "tidy the photos folder, dupes into Trash", "running_tool", "running a tool", "2m"),
    ])));
    g.set_selected("pi:main".into());
    g.set_has_agent(true);
    g.set_popped(popped);
    g.set_header(AgentHeaderData {
        id: "pi:main".into(),
        mind: "pi".into(),
        title: "tidy the photos folder, dupes into Trash".into(),
        state: "running_tool".into(),
        label: "running a tool".into(),
        since: "2m".into(),
        status: "".into(),
        note: "pi holds one conversation at a time — the same one the Lens talks to.".into(),
        can_send: false,
        send_hint: "pi is working — wait, or Stop it".into(),
        can_stop: true,
    });
    let item = |kind: &str, key: &str, text: &str| AgentItemData {
        kind: kind.into(),
        key: key.into(),
        text: text.into(),
        ..Default::default()
    };
    g.set_items(ModelRc::new(VecModel::from(vec![
        item("prompt", "t1", "tidy the photos folder, dupes into Trash"),
        item("text", "t1.0", "I'll find duplicates by hash first, then move the copies — not the originals."),
        AgentItemData {
            expanded: false,
            ..item("thinking", "t1.1", "Hashing is safer than names: a copy can be renamed. fdupes -r lists groups; keep the oldest of each.")
        },
        AgentItemData {
            call: call("agent_run", "", r#"agent_run command="fdupes -r ~/Pictures""#, "{\n  \"command\": \"fdupes -r ~/Pictures\"\n}", "done", " "),
            badge: "verified · exit 0".into(),
            output_kind: "terminal".into(),
            more: "214 lines in all".into(),
            can_open_all: true,
            ..item("card", "t1.2", "")
        },
        item("text", "t1.3", "38 duplicates in 17 groups. Asking before anything moves."),
        AgentItemData {
            call: call("os_act", "files.move", r#"os_act files.move from="~/Pictures/copy of a.jpg" to="~/.local/share/Trash""#, "{}", "", " "),
            badge: "reported".into(),
            ..item("card", "t1.4", "")
        },
        AgentItemData {
            call: call("agent_run", "", r#"agent_run command="du -sh ~/Pictures""#, "", "running", ""),
            badge: "verified".into(),
            live: true,
            output_kind: "terminal".into(),
            runs: runs(&[("4.1G\t/home/pranab/Pictures", PLAIN, false)]),
            rows: 1,
            ..item("card", "t1.5", "")
        },
        AgentItemData {
            call: call("agent_run", "", r#"agent_run command="ls ~/Pictures/raw""#, "", "failed", "ls: cannot access '/home/pranab/Pictures/raw': No such file or directory"),
            badge: "verified · exit 2".into(),
            explain: "Run by the shell itself; the exit code is the process's own.".into(),
            expanded: true,
            output_kind: "text".into(),
            ..item("card", "t1.6", "")
        },
        item("note", "t1.7", "Asked you: files.move 38 files → Trash"),
    ])));
    // The finished command, opened: its last screen, drawn from cells.
    let model = g.get_items();
    let mut done = slint::Model::row_data(&model, 3).unwrap();
    done.expanded = true;
    done.call.output = "".into();
    done.runs = runs(&[
        ("/home/pranab/Pictures/2024/beach.jpg", GREEN, true),
        ("/home/pranab/Pictures/copy of beach.jpg", PLAIN, false),
        ("", PLAIN, false),
        ("/home/pranab/Pictures/a.jpg", GREEN, true),
        ("/home/pranab/Pictures/old/a (1).jpg", PLAIN, false),
    ]);
    done.rows = 5;
    slint::Model::set_row_data(&model, 3, done);
    g.set_details(AgentDetailsData {
        mind: "pi".into(),
        model: "qwen3.8-27b".into(),
        since: "21:04".into(),
        turns: "1".into(),
        calls: "4 (1 failed)".into(),
        commands: "3 (1 failed)".into(),
        command_lines: "  exit 0  fdupes -r ~/Pictures\n running  du -sh ~/Pictures\n  exit 2  ls ~/Pictures/raw".into(),
        files: "none named".into(),
        file_lines: "".into(),
        approvals: "1 asked · 0 answered".into(),
        tokens: "41k in · 1.2k out".into(),
        cost: "".into(),
        refused: "".into(),
        basis: "Commands, files and approvals count only what the shell itself ran or asked. Calls include what the harness reported.".into(),
    });
}

fn save(pixels: &slint::SharedPixelBuffer<slint::Rgb8Pixel>, path: &str, width: u32, height: u32) -> Result<(), Box<dyn std::error::Error>> {
    let mut encoder = png::Encoder::new(BufWriter::new(File::create(path)?), width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(pixels.as_bytes())?;
    println!("Rendered {path} ({width}×{height})");
    Ok(())
}

fn hover(window: &MinimalSoftwareWindow, x: f32, y: f32) {
    window.dispatch_event(slint::platform::WindowEvent::PointerMoved { position: slint::LogicalPosition::new(x, y) });
}

pub fn run(w: &MinimalSoftwareWindow, output: &str) -> Result<(), Box<dyn std::error::Error>> {
    let (width, height) = (1280u32, 800u32);
    let ui = AgentsProbe::new()?;
    let g = ui.global::<AgentsState>();
    fill(&g, false);
    let log: Rc<RefCell<Vec<String>>> = Rc::default();
    {
        let l = log.clone();
        g.on_select(move |id| l.borrow_mut().push(format!("select:{id}")));
        let l = log.clone();
        g.on_select_tab(move |id| l.borrow_mut().push(format!("tab:{id}")));
        let l = log.clone();
        g.on_toggle(move |key, open| l.borrow_mut().push(format!("toggle:{key}:{open}")));
        let l = log.clone();
        g.on_pop_out(move |id| l.borrow_mut().push(format!("pop-out:{id}")));
        let l = log.clone();
        g.on_stop(move |id| l.borrow_mut().push(format!("stop:{id}")));
    }
    ui.show()?;
    w.set_size(slint::PhysicalSize::new(width, height));
    let draw = || {
        slint::platform::update_timers_and_animations();
        let mut pixels = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(width, height);
        w.request_redraw();
        w.draw_if_needed(|r| { r.render(pixels.make_mut_slice(), width as usize); });
        pixels
    };
    let first = draw();
    save(&first, output, width, height)?;

    // The pointer over the list tells the shell; leaving tells it again.
    hover(w, 150., 200.);
    draw();
    draw();
    assert!(g.get_list_hovered(), "the pointer over the list is reported, so rows hold still under it");
    hover(w, 640., 30.);
    draw();
    draw();
    assert!(!g.get_list_hovered(), "and its leaving is reported, so a row that needs you can rise");

    // A row selects its agent; a double click pops it out.
    click(w, 150., 101.);
    assert!(log.borrow().contains(&"select:deepseek:main".to_string()), "{:?}", log.borrow());
    // A tab filters.
    let tabs_y = 33.;
    for x in (20..420).step_by(8) {
        click(w, x as f32, tabs_y);
    }
    assert!(log.borrow().iter().any(|e| e == "tab:complete"), "{:?}", log.borrow());
    // Stop reaches the agent shown.
    for y in (height as i32 - 60..height as i32 - 16).step_by(4) {
        click(w, 1060., y as f32);
    }
    assert!(log.borrow().iter().any(|e| e == "stop:pi:main"), "{:?}", log.borrow());

    // A card opens from its line, and says which card to the shell.
    click(w, 600., 476.);
    assert!(log.borrow().iter().any(|e| e == "toggle:t1.4:true"), "{:?}", log.borrow());

    ui.set_light(true);
    hover(w, 640., 790.);
    draw();
    // Past the buttons' colour animation, so the picture is the light theme and not a blend.
    std::thread::sleep(std::time::Duration::from_millis(300));
    save(&draw(), &output.replace(".png", "-light.png"), width, height)?;
    ui.hide()?;

    // One agent in its own window: the same components, its own global.
    let window = AgentWindow::new()?;
    window.set_agent_title("Agent · pi · tidy the photos folder, dupes into Trash".into());
    fill(&window.global::<AgentsState>(), true);
    window.show()?;
    let (ww, wh) = (1000u32, 680u32);
    w.set_size(slint::PhysicalSize::new(ww, wh));
    slint::platform::update_timers_and_animations();
    let mut pixels = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(ww, wh);
    w.request_redraw();
    w.draw_if_needed(|r| { r.render(pixels.make_mut_slice(), ww as usize); });
    save(&pixels, &output.replace(".png", "-window.png"), ww, wh)?;
    println!("PASS: Agents list hover reported and cleared, row select, tab filter, Stop, a card opened from its line; screen and window rendered");
    Ok(())
}
