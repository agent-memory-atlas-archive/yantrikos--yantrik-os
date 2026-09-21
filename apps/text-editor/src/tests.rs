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
            "Native Editor condition timed out"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    tick(queue, window);
}
/// Directory the UI screenshots are written to.
///
/// This used to be `CARGO_MANIFEST_DIR/../../target`, which names the build
/// directory only when the build directory is the default one inside the
/// checkout. CI builds this tree from a git worktree against a shared target
/// directory outside it, so that path pointed at a directory that does not
/// exist, and `File::create(..).unwrap()` panicked with ENOENT before the test
/// had asserted anything. It read like a missing display; it is not one. The
/// window below is a `MinimalSoftwareWindow` behind a substituted
/// `slint::platform::Platform` and never touches X11 or Wayland, so this test
/// runs headless — it just has to put its PNGs somewhere that exists.
///
/// The test binary lives in `<target>/<profile>/deps/`, so its own path names
/// the real build directory wherever cargo put it.
fn shot_dir() -> PathBuf {
    let dir = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.parent()?.join("ui-screenshots")))
        .unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn save(window: &MinimalSoftwareWindow, name: &str) {
    let size = window.size();
    let mut pixels = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(size.width, size.height);
    window.request_redraw();
    window.draw_if_needed(|r| {
        r.render(pixels.make_mut_slice(), size.width as usize);
    });
    let path = shot_dir().join(name);
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
fn atomic_save_conflicts_links_permissions_and_utf8() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let dir = fixture("atomic");
    let path = dir.join("original.txt");
    std::fs::write(&path, "hello 🦀\r\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
    let mut d = Document::open(&path).unwrap();
    d.text.push_str("second\r\n");
    let d = d.save(&path).unwrap();
    assert!(!d.dirty());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), d.text);
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o640
    );
    std::fs::write(&path, "external edit").unwrap();
    assert!(d.save(&path).unwrap_err().contains("changed on disk"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "external edit");
    assert!(Document::blank().save(&path).is_err());
    let link = dir.join("linked.txt");
    symlink(&path, &link).unwrap();
    assert!(Document::blank().save(&link).is_err());
    let d = Document::open(&path).unwrap();
    std::fs::hard_link(&path, dir.join("hard.txt")).unwrap();
    assert!(d.save(&path).is_err());
    let bad = dir.join("binary");
    std::fs::write(&bad, [0xff, 0xfe]).unwrap();
    assert!(Document::open(&bad).is_err());
    assert!(std::fs::read_dir(&dir).unwrap().all(|e| !e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .ends_with(".tmp")));
}
#[test]
fn bounded_inputs_and_unicode_search() {
    assert!(document::validate("binary\u{1}control").is_err());
    let text = "a".repeat(20000);
    let ranges = document::matches(&text, "a", true);
    assert_eq!(ranges.len(), 20000);
    assert_eq!(
        document::replace(&text, &ranges, "b").unwrap(),
        "b".repeat(20000)
    );
    assert!(document::replace(&text, &ranges, &"x".repeat(1024)).is_err());
    assert!(document::validate(&"a".repeat(document::MAX_BYTES + 1)).is_err());
    assert!(document::validate(&"\n".repeat(20001)).is_err());
    let text = "İ α Kelvin K kelvin 🦀";
    let ranges = document::matches(text, "k", false);
    assert_eq!(
        ranges.iter().map(|&(a, z)| &text[a..z]).collect::<Vec<_>>(),
        ["K", "K", "k"]
    );
    assert_eq!(document::matches("one ONE", "one", true), [(0, 3)]);
    let dir = fixture("bounded");
    let fifo = dir.join("pipe");
    let path = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
    unsafe {
        libc::mkfifo(path.as_ptr(), 0o600);
    }
    assert!(Document::open(&fifo).is_err());
    let huge = dir.join("large");
    let f = std::fs::File::create(&huge).unwrap();
    f.set_len(2 * 1024 * 1024 * 1024).unwrap();
    assert!(Document::open(&huge).is_err());
}
#[test]
fn recovery_preserves_drafts_without_touching_originals() {
    let dir = fixture("recovery");
    let original = dir.join("a.txt");
    std::fs::write(&original, "disk").unwrap();
    let mut d = Document::open(&original).unwrap();
    d.text = "draft 🦀".into();
    let path = dir.join("recovery/drafts.json");
    document::checkpoint(&path, &[d, Document::blank()]).unwrap();
    let recovered = document::recover(&path).unwrap();
    assert_eq!(recovered.len(), 1);
    assert!(recovered[0].dirty());
    assert_eq!(recovered[0].text, "draft 🦀");
    assert_eq!(std::fs::read_to_string(original).unwrap(), "disk");
    let mut escaped = Document::blank();
    escaped.baseline = "\"".repeat(document::MAX_BYTES);
    escaped.text = "\\".repeat(document::MAX_BYTES);
    document::checkpoint(&path, &vec![escaped; 8]).unwrap();
    assert_eq!(document::recover(&path).unwrap().len(), 8);
    document::checkpoint(&path, &[]).unwrap();
    assert!(document::recover(&path).unwrap().is_empty());
}
#[test]
fn real_editor_keyboard_tabs_search_save_close_and_recovery() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    let queue = Queue::default();
    slint::platform::set_platform(Box::new(NativeTest {
        window: window.clone(),
        queue: queue.clone(),
        clipboard: Arc::new(Mutex::new(String::new())),
    }))
    .unwrap();
    let ui = TextEditorApp::new().unwrap();
    ui.show().unwrap();
    window.set_size(slint::PhysicalSize::new(1100, 760));
    let dir = fixture("ui");
    let recovery = dir.join("state/drafts.json");
    let s = wire(&ui, recovery.clone(), false);
    tick(&queue, &window);
    ui.invoke_focus_editor();
    key(&window, "Hello 🦀\nKelvin K kelvin\n");
    assert!(s.borrow().docs[0].dirty());
    assert_eq!(ui.get_line_count(), 3);
    ui.invoke_action("new".into());
    key(&window, "second draft");
    ui.invoke_action("undo".into());
    assert_eq!(ui.get_content(), "");
    ui.invoke_action("redo".into());
    assert_eq!(ui.get_content(), "second draft");
    ui.invoke_select_tab(0);
    window.dispatch_event(WindowEvent::KeyPressed {
        text: slint::platform::Key::Control.into(),
    });
    key(&window, "z");
    window.dispatch_event(WindowEvent::KeyReleased {
        text: slint::platform::Key::Control.into(),
    });
    assert_eq!(ui.get_content(), "");
    ui.invoke_action("redo".into());
    assert!(ui.get_content().starts_with("Hello"));
    ui.set_query("k".into());
    ui.invoke_search();
    assert_eq!(ui.get_match_count(), 3);
    ui.set_replacement("X".into());
    ui.invoke_action("replace-all".into());
    assert!(ui.get_content().contains("Xelvin X Xelvin"));
    ui.invoke_close_tab(0);
    assert_eq!(ui.get_dialog(), 3);
    ui.invoke_action("cancel".into());
    assert_eq!(s.borrow().docs.len(), 2);
    ui.invoke_action("save-as".into());
    ui.set_dialog_path(dir.join("saved.txt").display().to_string().into());
    ui.invoke_action("confirm".into());
    wait(&queue, &window, || !ui.get_busy());
    assert!(!s.borrow().docs[0].dirty());
    key(&window, "new edit");
    std::fs::write(dir.join("saved.txt"), "external").unwrap();
    ui.invoke_action("save".into());
    wait(&queue, &window, || !ui.get_busy());
    assert!(ui.get_notice().contains("changed on disk"));
    assert!(s.borrow().docs[0].dirty());
    ui.invoke_action("save-as".into());
    ui.set_dialog_path(dir.join("missing/no.txt").display().to_string().into());
    ui.invoke_action("confirm".into());
    wait(&queue, &window, || !ui.get_busy());
    assert_eq!(ui.get_dialog(), 2);
    assert!(s.borrow().docs[0].dirty());
    ui.invoke_action("cancel".into());
    wait(&queue, &window, || {
        ui.get_recovery_status() == "Draft recovery up to date"
    });
    assert_eq!(document::recover(&recovery).unwrap().len(), 2);
    for _ in 0..6 {
        ui.invoke_action("new".into());
    }
    assert_eq!(s.borrow().docs.len(), 8);
    ui.invoke_action("new".into());
    assert_eq!(s.borrow().docs.len(), 8);
    ui.invoke_select_tab(0);
    assert!(ui.get_content().contains("Xelvin"));
    // Real pointer/keyboard view with a small source file, rendered in both themes.
    let demo = dir.join("hello.rs");
    std::fs::write(&demo,"// A small idea, ready to grow.\n\nfn main() {\n    let message = \"Hello, Yantrik\";\n    println!(\"{}\", message);\n}\n").unwrap();
    for _ in 0..6 {
        ui.invoke_close_tab(2);
    }
    open(&ui, &s, demo);
    wait(&queue, &window, || !ui.get_busy());
    tick(&queue, &window);
    save(&window, "editor-dark.png");
    ui.global::<ThemeMode>().set_dark(false);
    window.set_size(slint::PhysicalSize::new(800, 600));
    tick(&queue, &window);
    save(&window, "editor-light.png");
    // Defocus the native caret: idle workbench must not continually request frames.
    window.dispatch_event(WindowEvent::WindowActiveChanged(false));
    let settle = std::time::Instant::now();
    while settle.elapsed() < Duration::from_millis(1200) {
        tick(&queue, &window);
        std::thread::sleep(Duration::from_millis(20));
    }
    let before = std::time::Instant::now();
    let mut redraws = 0;
    while before.elapsed() < Duration::from_millis(1000) {
        redraws += tick(&queue, &window) as usize;
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        redraws, 0,
        "inactive editor should not keep requesting frames"
    );
    let mut b = s.borrow_mut();
    b.recovery_timer.stop();
    let _ = b.jobs.send(Job::Shutdown(b.docs.clone()));
    if let Some(w) = b.worker.take() {
        w.join().unwrap();
    }
}
