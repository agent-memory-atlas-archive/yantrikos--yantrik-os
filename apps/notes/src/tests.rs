//! Exercise the real window, callbacks and PTYs with Slint's native software
//! renderer. Only the window-system event queue and clipboard are substituted.
use super::*;
use slint::platform::{
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
    Clipboard, EventLoopProxy, Platform, WindowAdapter, WindowEvent,
};
use std::sync::Arc;
use std::{collections::VecDeque, sync::Mutex, time::Instant};
type Queue = Arc<Mutex<VecDeque<Box<dyn FnOnce() + Send>>>>;
struct Proxy(Queue);
impl EventLoopProxy for Proxy {
    fn quit_event_loop(&self) -> Result<(), slint::EventLoopError> {
        Ok(())
    }
    fn invoke_from_event_loop(
        &self,
        event: Box<dyn FnOnce() + Send>,
    ) -> Result<(), slint::EventLoopError> {
        self.0.lock().unwrap().push_back(event);
        Ok(())
    }
}
struct NativeTest {
    window: Rc<MinimalSoftwareWindow>,
    queue: Queue,
    clipboard: Arc<Mutex<String>>,
}
impl Platform for NativeTest {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }
    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        Some(Box::new(Proxy(self.queue.clone())))
    }
    fn set_clipboard_text(&self, text: &str, _: Clipboard) {
        *self.clipboard.lock().unwrap() = text.into();
    }
    fn clipboard_text(&self, _: Clipboard) -> Option<String> {
        Some(self.clipboard.lock().unwrap().clone())
    }
}
fn key(window: &MinimalSoftwareWindow, text: impl Into<slint::SharedString>) {
    let text = text.into();
    window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    window.dispatch_event(WindowEvent::KeyReleased { text });
}
fn tick(queue: &Queue, window: &MinimalSoftwareWindow) -> bool {
    // Drop the queue lock before callbacks enqueue further events.
    let events: Vec<_> = queue.lock().unwrap().drain(..).collect();
    for event in events {
        event();
    }
    slint::platform::update_timers_and_animations();
    let size = window.size();
    let mut buffer = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(size.width, size.height);
    window.draw_if_needed(|renderer| {
        renderer.render(buffer.make_mut_slice(), size.width as usize);
    })
}
fn wait(queue: &Queue, window: &MinimalSoftwareWindow, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(6);
    while !condition() {
        tick(queue, window);
        assert!(
            Instant::now() < deadline,
            "Native Notes condition timed out"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    tick(queue, window);
}
fn screenshot(window: &MinimalSoftwareWindow, name: &str) {
    let size = window.size();
    let mut pixels = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(size.width, size.height);
    window.request_redraw();
    window.draw_if_needed(|r| {
        r.render(pixels.make_mut_slice(), size.width as usize);
    });
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target")
        .join(name);
    let mut png = png::Encoder::new(
        std::fs::File::create(path).unwrap(),
        size.width,
        size.height,
    );
    png.set_color(png::ColorType::Rgb);
    png.set_depth(png::BitDepth::Eight);
    png.write_header()
        .unwrap()
        .write_image_data(pixels.as_bytes())
        .unwrap();
}

fn fixture(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "yantrik-editor-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

#[test]
fn vault_compatibility_atomic_conflicts_and_recovery() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let dir = fixture("store");
    store::initialize(&dir).unwrap();
    std::fs::write(dir.join("old.md"), "# Existing\n\nHello 🦀\n").unwrap();
    std::fs::write(dir.join("old.meta"), "pinned:true\ntags:work, ideas\n").unwrap();
    let (mut notes, _) = store::load(&dir).unwrap();
    let mut n = notes.remove(0);
    assert_eq!(n.title(), "Existing");
    assert_eq!(n.field("tags"), "work, ideas");
    assert!(!n.dirty());
    n.text.push_str("A new line\n");
    n.set_field("notebook", "Work");
    let mut n = store::save(&dir, &n).unwrap();
    assert!(!n.dirty());
    assert_eq!(std::fs::read_to_string(dir.join(&n.id)).unwrap(), n.text);
    std::fs::write(dir.join(&n.id), "# External edit\n").unwrap();
    n.text.push_str("Protected draft");
    assert!(store::save(&dir, &n)
        .unwrap_err()
        .contains("changed on disk"));
    let (notes, msg) = store::load(&dir).unwrap();
    assert!(msg.contains("Recovered"));
    assert_eq!(notes.len(), 2);
    assert!(notes.iter().any(|n| n.text.ends_with("Protected draft")));
    assert_eq!(
        std::fs::read_to_string(dir.join("old.md")).unwrap(),
        "# External edit\n"
    );
    let mut n = notes.iter().find(|n| n.id == "old.md").unwrap().clone();
    n.text.push_str("bad");
    std::fs::set_permissions(dir.join("old.md"), std::fs::Permissions::from_mode(0o400)).unwrap();
    assert!(store::save(&dir, &n).is_err());
    std::fs::set_permissions(dir.join("old.md"), std::fs::Permissions::from_mode(0o600)).unwrap();
    symlink(dir.join("old.md"), dir.join("link.md")).unwrap();
    assert!(store::read(&dir.join("link.md"), store::LIMIT).is_err());
    n.id = "../escape.md".into();
    assert!(store::save(&dir, &n).is_err());
}
#[test]
fn trash_restore_no_clobber_and_bounded_reads() {
    let dir = fixture("trash");
    store::initialize(&dir).unwrap();
    let mut n = Note::blank("Trash me");
    n.set_field("notebook", "Research");
    let n = store::save(&dir, &n).unwrap();
    store::trash(&dir, &n).unwrap();
    assert!(!dir.join(&n.id).exists());
    let (notes, _) = store::load(&dir).unwrap();
    assert_eq!(notes.len(), 1);
    assert!(notes[0].trash);
    std::fs::write(dir.join(&n.id), "collision").unwrap();
    assert!(store::restore(&dir, &notes[0]).is_err());
    assert_eq!(
        std::fs::read_to_string(dir.join(&n.id)).unwrap(),
        "collision"
    );
    std::fs::remove_file(dir.join(&n.id)).unwrap();
    let r = store::restore(&dir, &notes[0]).unwrap();
    assert_eq!(r.text, n.text);
    assert_eq!(r.meta, n.meta);
    assert!(store::validate(&"x".repeat(store::LIMIT + 1)).is_err());
    assert!(store::validate("\0").is_err());
    let huge = dir.join("large.md");
    std::fs::File::create(&huge)
        .unwrap()
        .set_len(2 * 1024 * 1024 * 1024)
        .unwrap();
    assert!(store::read(&huge, store::LIMIT).is_err());
    std::fs::remove_file(huge).unwrap();
    let fifo = dir.join("pipe.md");
    let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    assert!(store::read(&fifo, store::LIMIT).is_err());
}
#[test]
fn real_notes_autosave_navigation_conflict_trash_preview_and_idle() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    let queue = Queue::default();
    slint::platform::set_platform(Box::new(NativeTest {
        window: window.clone(),
        queue: queue.clone(),
        clipboard: Arc::new(Mutex::new(String::new())),
    }))
    .unwrap();
    let ui = NotesApp::new().unwrap();
    ui.show().unwrap();
    window.set_size(slint::PhysicalSize::new(1120, 760));
    let dir = fixture("ui");
    let s = wire(&ui, dir.clone(), false);
    wait(&queue, &window, || !ui.get_busy());
    window.dispatch_event(WindowEvent::KeyPressed {
        text: slint::platform::Key::Control.into(),
    });
    key(&window, "n");
    window.dispatch_event(WindowEvent::KeyReleased {
        text: slint::platform::Key::Control.into(),
    });
    wait(&queue, &window, || !ui.get_busy());
    assert!(ui.get_opened(), "Ctrl+N works in the empty window");
    window.dispatch_event(WindowEvent::KeyPressed {
        text: slint::platform::Key::Control.into(),
    });
    key(&window, "f");
    key(&window, "e");
    assert!(ui.get_focus_mode());
    key(&window, "e");
    assert!(!ui.get_focus_mode());
    window.dispatch_event(WindowEvent::KeyReleased {
        text: slint::platform::Key::Control.into(),
    });
    ui.invoke_action("new:Field notes".into());
    wait(&queue, &window, || !ui.get_busy());
    let first = s.borrow().current.as_ref().unwrap().id.clone();
    ui.invoke_focus_editor();
    key(&window, "A thoughtful morning 🦀\n");
    assert!(ui.get_modified());
    let before = ui.get_content();
    window.dispatch_event(WindowEvent::KeyPressed {
        text: slint::platform::Key::Control.into(),
    });
    key(&window, "z");
    window.dispatch_event(WindowEvent::KeyReleased {
        text: slint::platform::Key::Control.into(),
    });
    assert_ne!(ui.get_content(), before, "Native undo must work");
    window.dispatch_event(WindowEvent::KeyPressed {
        text: slint::platform::Key::Control.into(),
    });
    window.dispatch_event(WindowEvent::KeyPressed {
        text: slint::platform::Key::Shift.into(),
    });
    key(&window, "z");
    window.dispatch_event(WindowEvent::KeyReleased {
        text: slint::platform::Key::Shift.into(),
    });
    window.dispatch_event(WindowEvent::KeyReleased {
        text: slint::platform::Key::Control.into(),
    });
    assert_eq!(ui.get_content(), before);
    // Switching immediately must await the save rather than dropping the edit.
    ui.invoke_action("new:Second page".into());
    wait(&queue, &window, || {
        !ui.get_busy() && ui.get_note_title() == "Second page"
    });
    assert!(std::fs::read_to_string(dir.join(&first))
        .unwrap()
        .contains("thoughtful"));
    let second = ui.get_content();
    window.dispatch_event(WindowEvent::KeyPressed {
        text: slint::platform::Key::Control.into(),
    });
    key(&window, "z");
    window.dispatch_event(WindowEvent::KeyReleased {
        text: slint::platform::Key::Control.into(),
    });
    assert_eq!(ui.get_content(), second, "Undo must not cross notes");
    ui.invoke_choose(first.clone().into());
    ui.set_notebook("Journal".into());
    ui.invoke_metadata();
    wait(&queue, &window, || !ui.get_busy() && !ui.get_modified());
    assert!(
        std::fs::read_to_string(dir.join(&first).with_extension("meta"))
            .unwrap()
            .contains("Journal")
    );
    ui.set_query("THOUGHTFUL".into());
    ui.invoke_filter();
    {
        use slint::Model;
        assert_eq!(ui.get_notes().row_count(), 1);
    }
    ui.set_query("".into());
    ui.invoke_filter();
    let text="# Field notes\n\nA quiet place for ideas.\n\n## Today's intentions\n\n- [x] Make room to think\n- [ ] Follow the interesting question\n\n> Small steps, taken with care.\n\n```rust\nlet idea = \"keep going\";\n```\n";
    ui.set_content(text.into());
    ui.invoke_edited(text.into());
    wait(&queue, &window, || !ui.get_busy() && !ui.get_modified());
    window.dispatch_event(WindowEvent::KeyPressed {
        text: slint::platform::Key::Control.into(),
    });
    key(&window, "p");
    assert!(ui.get_preview());
    key(&window, "p");
    assert!(!ui.get_preview());
    key(&window, "p");
    assert!(ui.get_preview());
    window.dispatch_event(WindowEvent::KeyReleased {
        text: slint::platform::Key::Control.into(),
    });
    {
        use slint::Model;
        assert!(ui.get_blocks().row_count() > 4);
    }
    tick(&queue, &window);
    screenshot(&window, "notes-dark.png");
    ui.global::<ThemeMode>().set_dark(false);
    window.set_size(slint::PhysicalSize::new(800, 600));
    tick(&queue, &window);
    screenshot(&window, "notes-light.png");
    ui.invoke_action("preview".into());
    std::fs::write(dir.join(&first), "external edit").unwrap();
    ui.set_content("# Draft kept\n".into());
    ui.invoke_edited("# Draft kept\n".into());
    ui.invoke_action("save".into());
    wait(&queue, &window, || !ui.get_busy());
    assert!(ui.get_modified());
    assert!(ui.get_notice().contains("changed on disk"));
    ui.invoke_action("new".into());
    wait(&queue, &window, || !ui.get_busy());
    assert_eq!(ui.get_note_title(), "Draft kept");
    ui.invoke_action("copy".into());
    wait(&queue, &window, || !ui.get_busy());
    assert!(!ui.get_modified());
    assert_eq!(
        std::fs::read_to_string(dir.join(&first)).unwrap(),
        "external edit"
    );
    let copy = s.borrow().current.as_ref().unwrap().id.clone();
    ui.invoke_action("trash".into());
    wait(&queue, &window, || !ui.get_busy());
    assert_eq!(ui.get_trash_count(), 1);
    ui.invoke_choose(format!("trash:{copy}").into());
    ui.invoke_action("restore".into());
    wait(&queue, &window, || !ui.get_busy());
    assert_eq!(ui.get_trash_count(), 0);
    window.dispatch_event(WindowEvent::WindowActiveChanged(false));
    let until = Instant::now() + Duration::from_millis(1400);
    while Instant::now() < until {
        tick(&queue, &window);
        std::thread::sleep(Duration::from_millis(20));
    }
    let start = Instant::now();
    let mut redraws = 0;
    while start.elapsed() < Duration::from_secs(1) {
        redraws += tick(&queue, &window) as usize;
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(redraws, 0);
    let mut b = s.borrow_mut();
    b.timer.stop();
    b.jobs.send(Job::Stop).unwrap();
    b.worker.take().unwrap().join().unwrap();
}
