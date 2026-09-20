//! Exercise the real window, callbacks and PTYs with Slint's native software
//! renderer. Only the window-system event queue and clipboard are substituted.
use super::*;
use slint::platform::{
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
    Clipboard, EventLoopProxy, Platform, WindowAdapter, WindowEvent,
};
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
            "Native Terminal condition timed out"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    tick(queue, window);
}
fn save(window: &MinimalSoftwareWindow, name: &str) {
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
#[test]
fn real_window_shell_tabs_search_clipboard_resize_and_idle() {
    std::env::set_var("SHELL", "/bin/sh");
    std::env::set_var("PS1", "yantrik $ ");
    let dir = std::env::temp_dir().join(format!("yantrik-terminal-ui-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("HOME", &dir);
    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(&dir).unwrap();
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    let queue = Queue::default();
    let clipboard = Arc::new(Mutex::new(String::new()));
    slint::platform::set_platform(Box::new(NativeTest {
        window: window.clone(),
        queue: queue.clone(),
        clipboard: clipboard.clone(),
    }))
    .unwrap();
    let ui = TerminalApp::new().unwrap();
    ui.show().unwrap();
    window.set_size(slint::PhysicalSize::new(1000, 680));
    let state = wire(&ui, false);
    ui.invoke_focus_terminal();
    wait(&queue, &window, || {
        ui.get_screen_text().contains("yantrik $")
    });
    key(
        &window,
        "export YANTRIK_UI_TEST=kept; printf 'SESSION:%s\\n' \"$YANTRIK_UI_TEST\"",
    );
    key(&window, "\n");
    wait(&queue, &window, || {
        ui.get_screen_text().contains("SESSION:kept")
    });
    assert_eq!(state.borrow().tabs.len(), 1);
    assert_eq!(state.borrow().session().unwrap().cwd(), dir);
    let folder = dir.join("folder with spaces; literal");
    std::fs::create_dir(&folder).unwrap();
    state.borrow_mut().new_tab_at(&ui, &folder).unwrap();
    assert_eq!(state.borrow().session().unwrap().cwd(), folder);
    assert_eq!(state.borrow().tabs[0].session.cwd(), dir);
    assert!(state
        .borrow_mut()
        .new_tab_at(&ui, &dir.join("missing"))
        .is_err());
    assert_eq!(state.borrow().tabs.len(), 2);
    state.borrow_mut().close(1, &ui, true);
    assert_eq!(state.borrow().active, 0);
    window.dispatch_event(WindowEvent::KeyPressed {
        text: slint::platform::Key::Control.into(),
    });
    window.dispatch_event(WindowEvent::KeyPressed {
        text: slint::platform::Key::Shift.into(),
    });
    key(&window, "t");
    window.dispatch_event(WindowEvent::KeyReleased {
        text: slint::platform::Key::Shift.into(),
    });
    window.dispatch_event(WindowEvent::KeyReleased {
        text: slint::platform::Key::Control.into(),
    });
    assert_eq!(state.borrow().tabs.len(), 2);
    assert_eq!(state.borrow().active, 1);
    wait(&queue, &window, || {
        ui.get_screen_text().contains("yantrik $")
    });
    assert!(!ui.get_screen_text().contains("SESSION:kept"));
    ui.invoke_select_tab(0);
    ui.invoke_focus_terminal();
    tick(&queue, &window);
    assert!(ui.get_screen_text().contains("SESSION:kept"));
    ui.invoke_copy_screen();
    assert!(clipboard.lock().unwrap().contains("SESSION:kept"));
    *clipboard.lock().unwrap() = "printf 'PASTE:%s\\n' worked\n".into();
    ui.invoke_paste_clipboard();
    wait(&queue, &window, || {
        ui.get_screen_text().contains("PASTE:worked")
    });
    ui.set_show_search(true);
    tick(&queue, &window);
    key(&window, "SESSION:kept");
    wait(&queue, &window, || ui.get_match_count() > 0);
    assert!(ui.get_match_row() >= 0);
    save(&window, "terminal-search.png");
    let count = ui.get_match_count();
    state
        .borrow()
        .session()
        .unwrap()
        .write(b"printf 'SESSION:%s\\n' kept\r")
        .unwrap();
    wait(&queue, &window, || ui.get_match_count() > count);
    key(&window, slint::platform::Key::Escape);
    assert!(!ui.get_show_search());
    key(&window, "sleep 30");
    key(&window, "\n");
    wait(&queue, &window, || {
        state.borrow().session().unwrap().has_children()
    });
    ui.invoke_close_tab(0);
    assert!(ui.get_confirm_close());
    assert_eq!(state.borrow().tabs.len(), 2);
    tick(&queue, &window);
    save(&window, "terminal-close-confirmation.png");
    key(&window, slint::platform::Key::Escape);
    assert!(!ui.get_confirm_close());
    key(&window, "\x03");
    wait(&queue, &window, || {
        !state.borrow().session().unwrap().has_children()
    });
    key(
        &window,
        "printf '\\033[36mYantrik Terminal\\033[0m\\nPersistent shells. Quiet when idle.\\n'",
    );
    key(&window, "\n");
    wait(&queue, &window, || {
        ui.get_screen_text()
            .contains("Persistent shells. Quiet when idle.")
    });
    save(&window, "terminal-native.png");
    window.set_size(slint::PhysicalSize::new(640, 480));
    let old = state.borrow().session().unwrap().snapshot().size;
    wait(&queue, &window, || {
        state.borrow().session().unwrap().snapshot().size != old
    });
    save(&window, "terminal-native-compact.png");
    ui.global::<ThemeMode>().set_dark(false);
    tick(&queue, &window);
    save(&window, "terminal-native-light.png");
    ui.set_show_assistant(true);
    tick(&queue, &window);
    save(&window, "terminal-assistant-compact.png");
    ui.set_show_assistant(false);
    ui.global::<ThemeMode>().set_dark(true);
    // Settle finite theme/focus feedback. Count only renderer-requested frames.
    for _ in 0..35 {
        tick(&queue, &window);
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut redraws = 0;
    for _ in 0..100 {
        if tick(&queue, &window) {
            redraws += 1;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        redraws, 0,
        "An idle real terminal session must not request redraws"
    );
    ui.invoke_action("new".into());
    for _ in 0..8 {
        ui.invoke_action("new".into());
    }
    assert_eq!(state.borrow().tabs.len(), 8);
    wait(&queue, &window, || {
        ui.get_screen_text().contains("yantrik $")
    });
    save(&window, "terminal-eight-tabs.png");
    key(&window, "exit 7\n");
    wait(&queue, &window, || !ui.get_alive());
    assert_eq!(state.borrow().session().unwrap().snapshot().exit, Some(7));
    ui.invoke_action("restart".into());
    wait(&queue, &window, || {
        ui.get_alive() && ui.get_screen_text().contains("yantrik $")
    });
    key(&window, "sleep 30\n");
    wait(&queue, &window, || {
        state.borrow().session().unwrap().has_children()
    });
    ui.invoke_close_tab(7);
    assert!(ui.get_confirm_close());
    ui.invoke_action("confirm-close".into());
    assert_eq!(state.borrow().tabs.len(), 7);
    assert!(!ui.get_confirm_close());
    for tab in state.borrow_mut().tabs.drain(..) {
        tab.session.shutdown();
    }
    drop(state);
    drop(ui);
    std::env::set_current_dir(original_dir).unwrap();
    let _ = std::fs::remove_dir(dir);
}

#[test]
fn keyboard_encoding_preserves_shell_controls_and_application_mode() {
    use slint::platform::Key;
    use slint::private_unstable_api::re_exports::KeyEvent;
    let mut e = KeyEvent::default();
    e.text = "c".into();
    e.modifiers.control = true;
    assert_eq!(encode_key(&e, false).unwrap(), b"\x03");
    e.modifiers.control = false;
    e.text = Key::UpArrow.into();
    assert_eq!(encode_key(&e, false).unwrap(), b"\x1b[A");
    assert_eq!(encode_key(&e, true).unwrap(), b"\x1bOA");
    e.modifiers.control = true;
    assert_eq!(encode_key(&e, false).unwrap(), b"\x1b[1;5A");
    e.modifiers.control = false;
    e.text = Key::Tab.into();
    assert_eq!(encode_key(&e, false).unwrap(), b"\t");
    e.text = "日本語".into();
    assert_eq!(encode_key(&e, false).unwrap(), "日本語".as_bytes());
    e.text = Key::Backspace.into();
    assert_eq!(encode_key(&e, false).unwrap(), b"\x7f");
}

#[test]
fn physical_modifiers_never_become_shell_control_bytes() {
    use slint::platform::Key;
    use slint::private_unstable_api::re_exports::KeyEvent;
    let mut event = KeyEvent::default();
    for key in [
        Key::Shift,
        Key::ShiftR,
        Key::Control,
        Key::ControlR,
        Key::Alt,
        Key::AltGr,
        Key::Meta,
        Key::MetaR,
        Key::CapsLock,
    ] {
        event.text = key.into();
        assert!(
            encode_key(&event, false).is_none(),
            "Modifier was forwarded: {key:?}"
        );
    }
    event.modifiers.control = true;
    event.text = "p".into();
    assert_eq!(encode_key(&event, false).unwrap(), b"\x10");
    event.text = "q".into();
    assert_eq!(encode_key(&event, false).unwrap(), b"\x11");
}
