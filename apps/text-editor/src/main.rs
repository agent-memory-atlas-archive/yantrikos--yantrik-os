//! Native, bounded text workbench. All document I/O runs on one worker.
mod document;
use document::{Document, MAX_TABS};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
    sync::mpsc,
    time::Duration,
};
use yantrik_app_runtime::prelude::*;
slint::include_modules!();

enum Job {
    Open(PathBuf),
    Save(Document, PathBuf),
    Recovery(Vec<Document>, u64),
    Shutdown(Vec<Document>),
}
enum Event {
    Open(Result<Document, String>),
    Saved(Result<Document, String>),
    Recovery(Result<(), String>, u64),
}
struct Workbench {
    docs: Vec<Document>,
    active: usize,
    matches: Vec<(usize, usize)>,
    match_index: usize,
    pending_close: Option<usize>,
    quitting: bool,
    save_close: bool,
    jobs: mpsc::Sender<Job>,
    events: mpsc::Receiver<Event>,
    recovery_timer: slint::Timer,
    recovery_generation: u64,
    worker: Option<std::thread::JoinHandle<()>>,
}
type State = Rc<RefCell<Workbench>>;
fn expanded(value: &str) -> PathBuf {
    if let Some(rest) = value.strip_prefix("~/") {
        PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(rest)
    } else {
        PathBuf::from(value)
    }
}
fn main() {
    init_tracing("yantrik-text-editor");
    // A VM explicitly forcing software OpenGL has no GPU to accelerate femtovg.
    // Render directly on the CPU instead; retain any explicit renderer choice.
    if std::env::var("LIBGL_ALWAYS_SOFTWARE").as_deref() == Ok("1")
        && matches!(
            std::env::var("SLINT_BACKEND").as_deref(),
            Ok("winit") | Err(_)
        )
    {
        std::env::set_var("SLINT_BACKEND", "winit-software");
    }
    let path = std::env::args_os().nth(1).map(PathBuf::from);
    // flock closes the launch race and releases automatically after a crash.
    use std::os::{fd::AsRawFd, unix::fs::OpenOptionsExt};
    let lock_path =
        PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR is required"))
            .join("yantrik-editor.lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(lock_path)
        .expect("editor instance lock");
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let args = path
            .map(|p| serde_json::json!({"action":"open","args":{"path":p}}))
            .unwrap_or_else(|| serde_json::json!({"action":"show","args":{}}));
        let client = SyncRpcClient::for_service("app-editor").with_timeout(Duration::from_secs(3));
        for _ in 0..20 {
            if let Ok(reply) = client.call("app.act", args.clone()) {
                if reply["accepted"] == true {
                    focus();
                    return;
                } else {
                    eprintln!("Editor declined request: {reply}");
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        eprintln!("Text Editor is already starting; try opening the file again.");
        return;
    }
    let ui = TextEditorApp::new().unwrap();
    let prefs = theme::load();
    ui.global::<ThemeMode>().set_dark(prefs.dark);
    ui.global::<AccentPreset>().set_index(prefs.accent_index);
    let state = wire(&ui, document::recovery_path(), true);
    if let Some(path) = path {
        open(&ui, &state, path);
    }
    ui.run().unwrap();
    // Synchronous final checkpoint only after the window event loop has stopped.
    let mut b = state.borrow_mut();
    b.recovery_timer.stop();
    let _ = b.jobs.send(Job::Shutdown(b.docs.clone()));
    if let Some(worker) = b.worker.take() {
        let _ = worker.join();
    }
}
fn focus() {
    let _ = std::process::Command::new("wlrctl")
        .args(["toplevel", "focus", "title:Editor"])
        .status();
}
fn wire(ui: &TextEditorApp, recovery_path: PathBuf, publish: bool) -> State {
    let (jobs, work) = mpsc::channel();
    let (results, events) = mpsc::channel();
    let weak = ui.as_weak();
    let recovery = document::recover(&recovery_path);
    let recovery_ok = recovery.is_ok();
    let docs = match recovery {
        Ok(d) if !d.is_empty() => {
            ui.set_notice("Recovered unsaved drafts. Review them, then Save or Save As.".into());
            d
        }
        Ok(_) => vec![Document::blank()],
        Err(e) => {
            ui.set_notice(e.into());
            vec![Document::blank()]
        }
    };
    let worker = std::thread::spawn(move || {
        while let Ok(job) = work.recv() {
            let event = match job {
                Job::Open(p) => Event::Open(Document::open(&p)),
                Job::Save(d, p) => Event::Saved(d.save(&p)),
                Job::Recovery(d, g) => Event::Recovery(
                    if recovery_ok {
                        document::checkpoint(&recovery_path, &d)
                    } else {
                        Err("Existing recovery file is unreadable and has been preserved.".into())
                    },
                    g,
                ),
                Job::Shutdown(d) => {
                    if recovery_ok {
                        let _ = document::checkpoint(&recovery_path, &d);
                    }
                    break;
                }
            };
            if results.send(event).is_err() {
                break;
            }
            let _ = weak.upgrade_in_event_loop(|u| u.invoke_refresh());
        }
    });
    let state = Rc::new(RefCell::new(Workbench {
        docs,
        active: 0,
        matches: vec![],
        match_index: 0,
        pending_close: None,
        quitting: false,
        save_close: false,
        jobs,
        events,
        recovery_timer: slint::Timer::default(),
        recovery_generation: 0,
        worker: Some(worker),
    }));
    let weak = ui.as_weak();
    let s = state.clone();
    ui.on_refresh(move || {
        if let Some(u) = weak.upgrade() {
            receive(&u, &s);
        }
    });
    let weak = ui.as_weak();
    let s = state.clone();
    ui.on_action(move |id| {
        if let Some(u) = weak.upgrade() {
            action(&u, &s, &id);
        }
    });
    let weak = ui.as_weak();
    let s = state.clone();
    ui.on_select_tab(move |index| {
        if let Some(u) = weak.upgrade() {
            if u.get_busy() || u.get_dialog() != 0 {
                return;
            }
            let mut b = s.borrow_mut();
            if index >= 0 && (index as usize) < b.docs.len() {
                b.active = index as usize;
                paint(&u, &b, true);
                drop(b);
                search(&u, &s, false);
            }
        }
    });
    let weak = ui.as_weak();
    let s = state.clone();
    ui.on_close_tab(move |index| {
        if let Some(u) = weak.upgrade() {
            request_close(&u, &s, index as usize, false);
        }
    });
    let weak = ui.as_weak();
    let s = state.clone();
    ui.on_edited(move |text| {
        if let Some(u) = weak.upgrade() {
            edit(&u, &s, text.to_string());
        }
    });
    let weak = ui.as_weak();
    let s = state.clone();
    ui.on_search(move || {
        if let Some(u) = weak.upgrade() {
            search(&u, &s, true);
        }
    });
    let weak = ui.as_weak();
    ui.on_cursor(move |offset| {
        if let Some(u) = weak.upgrade() {
            let text = u.get_content();
            let end = (offset.max(0) as usize).min(text.len());
            if let Some(prefix) = text.get(..end) {
                u.set_cursor_line(prefix.bytes().filter(|b| *b == b'\n').count() as i32 + 1);
                u.set_cursor_column(
                    prefix.rsplit('\n').next().unwrap_or("").chars().count() as i32 + 1,
                );
            }
        }
    });
    let weak = ui.as_weak();
    let s = state.clone();
    ui.window().on_close_requested(move || {
        if let Some(u) = weak.upgrade() {
            if !u.get_busy() {
                request_close(&u, &s, 0, true);
            }
        }
        slint::CloseRequestResponse::KeepWindowShown
    });
    paint(ui, &state.borrow(), true);
    ui.invoke_focus_editor();
    if publish {
        publish_control(ui, state.clone());
    }
    state
}
fn paint(ui: &TextEditorApp, b: &Workbench, content: bool) {
    let d = &b.docs[b.active];
    ui.set_tabs(ModelRc::new(VecModel::from(
        b.docs
            .iter()
            .enumerate()
            .map(|(i, d)| DocumentTab {
                title: d.title().into(),
                active: i == b.active,
                modified: d.dirty(),
            })
            .collect::<Vec<_>>(),
    )));
    ui.set_document_title(d.title().into());
    ui.set_modified(d.dirty());
    ui.set_path_label(
        d.path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "Untitled · choose a home with Save As".into())
            .into(),
    );
    ui.set_language(document::language(d.path.as_deref()).into());
    if content {
        ui.set_content(d.text.clone().into());
        ui.invoke_reset_position();
        ui.invoke_focus_editor();
    }
    let lines = d.text.bytes().filter(|b| *b == b'\n').count() + 1;
    ui.set_line_count(lines as i32);
    ui.set_numbers(
        (1..=lines)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("\n")
            .into(),
    );
    ui.set_ending(
        if d.text.contains("\r\n") {
            "CRLF"
        } else {
            "LF"
        }
        .into(),
    );
    let (a, c, e) = highlight(&d.text, document::language(d.path.as_deref()));
    ui.set_has_highlights(!a.is_empty());
    ui.set_keywords(a.into());
    ui.set_strings(c.into());
    ui.set_comments(e.into());
}
/// Small lexical highlighter. No parser service, background polling or per-token UI nodes.
fn highlight(text: &str, language: &str) -> (String, String, String) {
    if text.len() > 65536 || language == "Plain text" {
        return Default::default();
    }
    let mut layers = [String::new(), String::new(), String::new()];
    let words = [
        "fn", "let", "mut", "pub", "use", "mod", "struct", "impl", "enum", "match", "if", "else",
        "return", "for", "in", "while", "loop", "true", "false", "None", "Some", "const", "def",
        "class", "import", "from", "as", "with", "try", "except", "async", "await", "function",
        "export", "default", "var", "null", "new", "self",
    ];
    for line in text.split_inclusive('\n') {
        let mut masks = [
            vec![b' '; line.len()],
            vec![b' '; line.len()],
            vec![b' '; line.len()],
        ];
        for (i, b) in line.bytes().enumerate() {
            if b == b'\n' || b == b'\r' {
                for m in &mut masks {
                    m[i] = b;
                }
            }
        }
        if line.is_ascii() && !line.contains('\t') {
            let bytes = line.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                if (bytes[i] == b'#'
                    && matches!(language, "Python" | "Shell" | "TOML" | "Markdown"))
                    || (bytes[i..].starts_with(b"//") && language != "JSON")
                {
                    masks[2][i..].copy_from_slice(&bytes[i..]);
                    break;
                }
                if matches!(bytes[i], b'\'' | b'"') {
                    let start = i;
                    let quote = bytes[i];
                    i += 1;
                    while i < bytes.len() {
                        if bytes[i] == b'\\' {
                            i = (i + 2).min(bytes.len());
                            continue;
                        }
                        if bytes[i] == quote {
                            i += 1;
                            break;
                        }
                        i += 1;
                    }
                    masks[1][start..i].copy_from_slice(&bytes[start..i]);
                    continue;
                }
                if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
                    let start = i;
                    i += 1;
                    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_')
                    {
                        i += 1
                    }
                    if words.contains(&&line[start..i]) {
                        masks[0][start..i].copy_from_slice(&bytes[start..i]);
                    }
                    continue;
                }
                i += 1;
            }
        }
        for (out, mask) in layers.iter_mut().zip(masks) {
            out.push_str(&String::from_utf8(mask).unwrap());
        }
    }
    let [a, b, c] = layers;
    (a, b, c)
}
fn checkpoint(ui: &TextEditorApp, state: &State) {
    let mut b = state.borrow_mut();
    b.recovery_generation += 1;
    let generation = b.recovery_generation;
    let docs: Vec<_> = b.docs.iter().map(Document::snapshot).collect();
    let jobs = b.jobs.clone();
    ui.set_recovery_status("Protecting draft…".into());
    b.recovery_timer.start(
        slint::TimerMode::SingleShot,
        Duration::from_millis(750),
        move || {
            let _ = jobs.send(Job::Recovery(docs.clone(), generation));
        },
    );
}
fn edit(ui: &TextEditorApp, s: &State, text: String) {
    if ui.get_busy() {
        return;
    }
    let mut b = s.borrow_mut();
    if let Err(e) = document::validate(&text) {
        ui.set_content(b.docs[b.active].text.clone().into());
        ui.set_notice(e.into());
        return;
    }
    let active = b.active;
    b.docs[active].edit(text);
    paint(ui, &b, false);
    drop(b);
    search(ui, s, false);
    checkpoint(ui, s);
}
fn search(ui: &TextEditorApp, s: &State, select: bool) {
    let mut b = s.borrow_mut();
    b.matches = document::matches(&b.docs[b.active].text, &ui.get_query(), ui.get_match_case());
    b.match_index = 0;
    ui.set_match_count(b.matches.len() as i32);
    ui.set_match_index(if b.matches.is_empty() { 0 } else { 1 });
    if select {
        if let Some(&(a, z)) = b.matches.first() {
            ui.invoke_select_range(a as i32, z as i32);
        }
    }
}
fn open(ui: &TextEditorApp, s: &State, path: PathBuf) {
    if ui.get_busy() {
        ui.set_notice("Finish the current file operation first.".into());
        return;
    }
    if ui.get_dialog() == 3 {
        ui.set_notice("Resolve the unsaved changes dialog first.".into());
        return;
    }
    ui.set_busy(true);
    ui.set_notice("Opening file…".into());
    let _ = s.borrow().jobs.send(Job::Open(path));
}
fn save(ui: &TextEditorApp, s: &State, path: Option<PathBuf>) {
    let b = s.borrow();
    let d = b.docs[b.active].snapshot();
    let Some(path) = path.or_else(|| d.path.clone()) else {
        drop(b);
        action(ui, s, "save-as");
        return;
    };
    ui.set_busy(true);
    ui.set_notice("Saving…".into());
    ui.set_dialog_error("".into());
    let _ = b.jobs.send(Job::Save(d, path));
}
fn receive(ui: &TextEditorApp, s: &State) {
    loop {
        let event = s.borrow().events.try_recv();
        let Ok(event) = event else { break };
        match event {
            Event::Recovery(result, g) => {
                if g == s.borrow().recovery_generation {
                    match result {
                        Ok(()) => ui.set_recovery_status("Draft recovery up to date".into()),
                        Err(e) => {
                            ui.set_recovery_status("Recovery unavailable".into());
                            ui.set_notice(format!("Draft recovery failed: {e}").into());
                        }
                    }
                }
            }
            Event::Open(result) => {
                ui.set_busy(false);
                match result {
                    Ok(d) => {
                        let mut b = s.borrow_mut();
                        if let Some(index) = b.docs.iter().position(|old| old.path == d.path) {
                            b.active = index;
                        } else if b.docs.len() == 1
                            && b.docs[0].path.is_none()
                            && !b.docs[0].dirty()
                        {
                            b.docs[0] = d;
                            b.active = 0;
                        } else if b.docs.len() < MAX_TABS {
                            b.docs.push(d);
                            b.active = b.docs.len() - 1;
                        } else {
                            ui.set_notice(
                                "Eight tabs are open. Close one before opening another file."
                                    .into(),
                            );
                            continue;
                        }
                        ui.set_dialog(0);
                        ui.set_notice("".into());
                        paint(ui, &b, true);
                        drop(b);
                        search(ui, s, false);
                    }
                    Err(e) => {
                        ui.set_notice(e.clone().into());
                        ui.set_dialog_error(e.into());
                    }
                }
            }
            Event::Saved(result) => {
                ui.set_busy(false);
                match result {
                    Ok(d) => {
                        let mut b = s.borrow_mut();
                        let active = b.active;
                        let mut d = d;
                        d.undo = std::mem::take(&mut b.docs[active].undo);
                        d.redo = std::mem::take(&mut b.docs[active].redo);
                        b.docs[active] = d;
                        let close = b.save_close;
                        b.save_close = false;
                        ui.set_dialog(0);
                        ui.set_notice("Saved".into());
                        paint(ui, &b, false);
                        drop(b);
                        checkpoint(ui, s);
                        if close {
                            finish_close(ui, s);
                        }
                    }
                    Err(e) => {
                        s.borrow_mut().save_close = false;
                        ui.set_notice(e.clone().into());
                        ui.set_dialog_error(e.into());
                    }
                }
            }
        }
    }
}
fn request_close(ui: &TextEditorApp, s: &State, index: usize, quitting: bool) {
    if ui.get_busy() || ui.get_dialog() != 0 {
        return;
    }
    let mut b = s.borrow_mut();
    b.quitting = quitting;
    let index = if quitting {
        b.docs.iter().position(Document::dirty).unwrap_or(0)
    } else {
        index
    };
    if index >= b.docs.len() {
        return;
    }
    b.pending_close = Some(index);
    if b.docs[index].dirty() {
        b.active = index;
        paint(ui, &b, true);
        ui.set_close_label(format!("{} has unsaved changes.", b.docs[index].title()).into());
        ui.set_dialog_error("".into());
        ui.set_dialog(3);
    } else {
        drop(b);
        finish_close(ui, s);
    }
}
fn finish_close(ui: &TextEditorApp, s: &State) {
    let mut b = s.borrow_mut();
    let quitting = b.quitting;
    if let Some(index) = b.pending_close.take() {
        if index < b.docs.len() {
            b.docs.remove(index);
            if index < b.active {
                b.active -= 1;
            }
        }
    }
    if b.docs.is_empty() {
        b.docs.push(Document::blank());
    }
    b.active = b.active.min(b.docs.len() - 1);
    ui.set_dialog(0);
    paint(ui, &b, true);
    drop(b);
    checkpoint(ui, s);
    if quitting {
        if s.borrow().docs.iter().any(Document::dirty) {
            request_close(ui, s, 0, true);
        } else {
            // Flush an empty recovery snapshot before a deliberate clean exit.
            let b = s.borrow();
            b.recovery_timer.stop();
            let _ = b
                .jobs
                .send(Job::Recovery(b.docs.clone(), b.recovery_generation));
            drop(b);
            let _ = ui.hide();
            let _ = slint::quit_event_loop();
        }
    }
}
fn action(ui: &TextEditorApp, s: &State, id: &str) {
    if ui.get_busy() {
        return;
    }
    if ui.get_dialog() != 0
        && !matches!(
            id,
            "cancel" | "confirm" | "discard" | "save-close" | "save-as"
        )
    {
        return;
    }
    match id {
        "undo" | "redo" => {
            let mut b = s.borrow_mut();
            let active = b.active;
            b.docs[active].undo(id == "redo");
            paint(ui, &b, true);
            drop(b);
            search(ui, s, false);
            checkpoint(ui, s);
        }
        "new" => {
            let mut b = s.borrow_mut();
            if b.docs.len() >= MAX_TABS {
                ui.set_notice("Eight tabs are open. Close a tab first.".into());
                return;
            }
            b.docs.push(Document::blank());
            b.active = b.docs.len() - 1;
            ui.set_notice("".into());
            paint(ui, &b, true);
        }
        "open" | "save-as" => {
            let b = s.borrow();
            ui.set_dialog_path(if id == "open" {
                "~/".into()
            } else {
                b.docs[b.active]
                    .path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "~/untitled.txt".into())
                    .into()
            });
            ui.set_dialog_error("".into());
            ui.set_dialog(if id == "open" { 1 } else { 2 });
        }
        "save" => save(ui, s, None),
        "save-close" => {
            s.borrow_mut().save_close = true;
            save(ui, s, None);
        }
        "goto" => {
            ui.set_dialog_path(ui.get_cursor_line().to_string().into());
            ui.set_dialog_error("".into());
            ui.set_dialog(4);
        }
        "confirm" => match ui.get_dialog() {
            1 => open(ui, s, expanded(&ui.get_dialog_path())),
            2 => save(ui, s, Some(expanded(&ui.get_dialog_path()))),
            4 => {
                if let Ok(line) = ui.get_dialog_path().parse::<usize>() {
                    let text = ui.get_content();
                    let offset = if line <= 1 {
                        0
                    } else {
                        text.match_indices('\n')
                            .nth(line - 2)
                            .map(|(i, _)| i + 1)
                            .unwrap_or(text.len())
                    };
                    ui.set_dialog(0);
                    ui.invoke_focus_editor();
                    ui.invoke_select_range(offset as i32, offset as i32);
                } else {
                    ui.set_dialog_error("Enter a whole line number.".into());
                }
            }
            _ => {}
        },
        "cancel" => {
            let mut b = s.borrow_mut();
            b.pending_close = None;
            b.quitting = false;
            b.save_close = false;
            ui.set_dialog(0);
            ui.invoke_focus_editor();
        }
        "discard" => finish_close(ui, s),
        "close" => {
            let i = s.borrow().active;
            request_close(ui, s, i, false);
        }
        "next-tab" | "previous-tab" => {
            let mut b = s.borrow_mut();
            b.active = (b.active
                + if id == "next-tab" {
                    1
                } else {
                    b.docs.len() - 1
                })
                % b.docs.len();
            paint(ui, &b, true);
            drop(b);
            search(ui, s, false);
        }
        "find-next" | "find-prev" => {
            let mut b = s.borrow_mut();
            let len = b.matches.len();
            if len == 0 {
                return;
            }
            b.match_index = (b.match_index + if id == "find-next" { 1 } else { len - 1 }) % len;
            let (a, z) = b.matches[b.match_index];
            ui.set_match_index(b.match_index as i32 + 1);
            ui.invoke_select_range(a as i32, z as i32);
        }
        "replace" | "replace-all" => {
            let b = s.borrow();
            let ranges = if id == "replace-all" {
                b.matches.as_slice()
            } else {
                b.matches
                    .get(b.match_index)
                    .map(std::slice::from_ref)
                    .unwrap_or(&[])
            };
            let result = document::replace(&b.docs[b.active].text, ranges, &ui.get_replacement());
            drop(b);
            match result {
                Ok(text) => {
                    ui.set_content(text.clone().into());
                    edit(ui, s, text);
                }
                Err(e) => ui.set_notice(e.into()),
            }
        }
        _ => {}
    }
}
fn publish_control(ui: &TextEditorApp, s: State) {
    use yantrik_app_runtime::control::{Action, App, Param, View};
    let weak = ui.as_weak();
    let state = s.clone();
    let mut app=App::new("editor").describe(move||{
        let b=state.borrow();let d=&b.docs[b.active];let ui=weak.upgrade();
        View::new(format!("Text Editor — {}",d.title())).with("path",serde_json::json!(d.path)).with("modified",d.dirty()).with("content",d.text.chars().take(4000).collect::<String>()).with("bytes",d.text.len()).with("tabs",b.docs.iter().map(|d|serde_json::json!({"name":d.title(),"path":d.path,"modified":d.dirty()})).collect::<Vec<_>>()).with("active_tab",b.active).with("busy",ui.as_ref().is_some_and(|u|u.get_busy())).with("notice",ui.as_ref().map(|u|u.get_notice().to_string())).with("dialog",ui.as_ref().map(|u|u.get_dialog())).with("recovery",ui.as_ref().map(|u|u.get_recovery_status().to_string())).with("cursor_line",ui.as_ref().map(|u|u.get_cursor_line())).with("cursor_column",ui.as_ref().map(|u|u.get_cursor_column())).with("find_count",ui.as_ref().map(|u|u.get_match_count()))
    });
    for name in [
        "new",
        "save",
        "show",
        "close",
        "cancel",
        "discard",
        "find-next",
        "find-prev",
        "replace",
        "replace-all",
    ] {
        let weak = ui.as_weak();
        let state = s.clone();
        app = app.action(Action::new(name, &format!("Editor: {name}")), move |_| {
            let ui = weak.upgrade().ok_or("Editor closed")?;
            if ui.get_busy() {
                return Err("Editor is busy".into());
            }
            if name == "show" {
                let _ = ui.show();
            } else {
                action(&ui, &state, name);
            }
            Ok(serde_json::json!({"accepted":true}))
        });
    }
    for name in [
        "open",
        "save_as",
        "set_content",
        "find",
        "replace_text",
        "select_tab",
    ] {
        let param = if name == "open" || name == "save_as" {
            "path"
        } else {
            "text"
        };
        let weak = ui.as_weak();
        let state = s.clone();
        app = app.action(
            Action::new(name, &format!("Editor: {name}")).arg(Param::text(param)),
            move |args| {
                let ui = weak.upgrade().ok_or("Editor closed")?;
                if ui.get_busy() {
                    return Err("Editor is busy".into());
                }
                if ui.get_dialog() == 3 {
                    return Err("Resolve unsaved changes first".into());
                }
                let text = args[param].as_str().ok_or("Missing parameter")?;
                match name {
                    "open" => open(&ui, &state, expanded(text)),
                    "save_as" => save(&ui, &state, Some(expanded(text))),
                    "set_content" => {
                        document::validate(text)?;
                        ui.set_content(text.into());
                        edit(&ui, &state, text.into());
                    }
                    "find" => {
                        ui.set_query(text.into());
                        ui.set_show_find(true);
                        search(&ui, &state, true);
                    }
                    "replace_text" => ui.set_replacement(text.into()),
                    "select_tab" => {
                        let i = text.parse::<i32>().map_err(|_| "Expected tab index")?;
                        ui.invoke_select_tab(i);
                    }
                    _ => {}
                }
                Ok(serde_json::json!({"accepted":true,"completed":!ui.get_busy()}))
            },
        );
    }
    app.serve();
}

#[cfg(test)]
mod tests;
