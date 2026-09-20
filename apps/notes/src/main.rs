//! Native Notes workbench. Filesystem work is serialized on one sleeping worker.
mod store;
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{cell::RefCell, path::PathBuf, rc::Rc, sync::mpsc, time::Duration};
use store::Note;
use yantrik_app_runtime::prelude::*;
slint::include_modules!();
enum Job {
    Load,
    Save(Note),
    Trash(Note),
    Restore(Note),
    Import(PathBuf, usize),
    Export(PathBuf, String),
    Stop,
}
enum Event {
    Loaded(Result<(Vec<Note>, String), String>),
    Saved(Result<Note, String>),
    Trashed(String, Result<(), String>),
    Restored(Result<Note, String>),
    Imported(Result<Note, String>),
    Exported(Result<(), String>),
}
struct Workbench {
    notes: Vec<Note>,
    current: Option<Note>,
    jobs: mpsc::Sender<Job>,
    events: mpsc::Receiver<Event>,
    timer: slint::Timer,
    worker: Option<std::thread::JoinHandle<()>>,
    pending: Option<String>,
    quitting: bool,
    ready: bool,
    failed: bool,
    undo: Vec<String>,
    redo: Vec<String>,
}
type State = Rc<RefCell<Workbench>>;
fn main() {
    init_tracing("yantrik-notes");
    if std::env::var("LIBGL_ALWAYS_SOFTWARE").as_deref() == Ok("1")
        && matches!(
            std::env::var("SLINT_BACKEND").as_deref(),
            Ok("winit") | Err(_)
        )
    {
        std::env::set_var("SLINT_BACKEND", "winit-software");
    }
    use std::os::{fd::AsRawFd, unix::fs::OpenOptionsExt};
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(
            PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR"))
                .join("yantrik-notes.lock"),
        )
        .expect("Notes instance lock");
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let _ = std::process::Command::new("wlrctl")
            .args(["toplevel", "focus", "title:Notes"])
            .status();
        return;
    }
    let ui = NotesApp::new().unwrap();
    let prefs = theme::load();
    ui.global::<ThemeMode>().set_dark(prefs.dark);
    ui.global::<AccentPreset>().set_index(prefs.accent_index);
    let dir = std::env::var_os("YANTRIK_NOTES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                .join(".local/share/yantrik/notes")
        });
    let s = wire(&ui, dir, true);
    ui.run().unwrap();
    let mut b = s.borrow_mut();
    b.timer.stop();
    let _ = b.jobs.send(Job::Stop);
    if let Some(w) = b.worker.take() {
        let _ = w.join();
    }
}
fn wire(ui: &NotesApp, dir: PathBuf, publish: bool) -> State {
    let (jobs, work) = mpsc::channel();
    let (results, events) = mpsc::channel();
    let weak = ui.as_weak();
    let worker = std::thread::spawn(move || {
        while let Ok(job) = work.recv() {
            let e = match job {
                Job::Load => Event::Loaded(store::load(&dir)),
                Job::Save(n) => Event::Saved(store::save(&dir, &n)),
                Job::Trash(n) => Event::Trashed(n.id.clone(), store::trash(&dir, &n)),
                Job::Restore(n) => Event::Restored(store::restore(&dir, &n)),
                Job::Import(p, remaining) => Event::Imported((|| {
                    let text = store::read(&p, store::LIMIT)?.ok_or("File not found")?;
                    store::validate(&text)?;
                    if text.len() > remaining {
                        return Err("Library reached its 32 MiB text limit.".into());
                    }
                    let mut n = Note::blank("Imported note");
                    n.text = text;
                    store::save(&dir, &n)
                })()),
                Job::Export(p, t) => Event::Exported(store::atomic(&p, &t, false)),
                Job::Stop => break,
            };
            if results.send(e).is_err() {
                break;
            }
            let _ = weak.upgrade_in_event_loop(|u| u.invoke_refresh());
        }
    });
    let s = Rc::new(RefCell::new(Workbench {
        notes: vec![],
        current: None,
        jobs,
        events,
        timer: slint::Timer::default(),
        worker: Some(worker),
        pending: None,
        quitting: false,
        ready: false,
        failed: false,
        undo: vec![],
        redo: vec![],
    }));
    let w = ui.as_weak();
    let b = s.clone();
    ui.on_refresh(move || {
        if let Some(u) = w.upgrade() {
            receive(&u, &b)
        }
    });
    let w = ui.as_weak();
    let b = s.clone();
    ui.on_action(move |a| {
        if let Some(u) = w.upgrade() {
            action(&u, &b, a.as_str())
        }
    });
    let w = ui.as_weak();
    let b = s.clone();
    ui.on_choose(move |id| {
        if let Some(u) = w.upgrade() {
            action(&u, &b, &format!("open:{id}"))
        }
    });
    let w = ui.as_weak();
    let b = s.clone();
    ui.on_filter(move || {
        if let Some(u) = w.upgrade() {
            list(&u, &b)
        }
    });
    let w = ui.as_weak();
    let b = s.clone();
    ui.on_edited(move |text| {
        if let Some(u) = w.upgrade() {
            edit(&u, &b, text.to_string())
        }
    });
    let w = ui.as_weak();
    let b = s.clone();
    ui.on_metadata(move || {
        if let Some(u) = w.upgrade() {
            {
                let mut st = b.borrow_mut();
                if let Some(n) = st.current.as_mut() {
                    if n.trash {
                        return;
                    }
                    n.set_field(
                        "notebook",
                        &u.get_notebook().chars().take(80).collect::<String>(),
                    );
                    n.set_field("tags", &u.get_tags().chars().take(512).collect::<String>());
                }
            }
            changed(&u, &b);
        }
    });
    let w = ui.as_weak();
    let b = s.clone();
    ui.window().on_close_requested(move || {
        let Some(u) = w.upgrade() else {
            return slint::CloseRequestResponse::HideWindow;
        };
        if u.get_busy() {
            u.set_notice("Please wait for the current operation before closing.".into());
            return slint::CloseRequestResponse::KeepWindowShown;
        }
        if b.borrow().current.as_ref().is_some_and(Note::dirty) {
            b.borrow_mut().quitting = true;
            save(&u, &b);
            slint::CloseRequestResponse::KeepWindowShown
        } else {
            slint::CloseRequestResponse::HideWindow
        }
    });
    send(ui, &s, Job::Load);
    if publish {
        control(ui, &s)
    }
    s
}
fn send(ui: &NotesApp, s: &State, job: Job) {
    ui.set_busy(true);
    ui.set_status("Working…".into());
    if s.borrow().jobs.send(job).is_err() {
        ui.set_busy(false);
        ui.set_notice(
            "Storage worker stopped. Keep this window open to preserve your draft.".into(),
        );
    }
}
fn changed(ui: &NotesApp, s: &State) {
    let b = s.borrow();
    let Some(n) = &b.current else { return };
    ui.set_modified(n.dirty());
    ui.set_note_title(n.title().into());
    ui.set_words(n.text.split_whitespace().count() as i32);
    ui.set_status(
        if n.dirty() {
            "Unsaved · autosave pending"
        } else {
            "All changes saved"
        }
        .into(),
    );
    let w = ui.as_weak();
    let state = Rc::downgrade(s);
    b.timer.start(
        slint::TimerMode::SingleShot,
        Duration::from_millis(750),
        move || {
            if let (Some(u), Some(s)) = (w.upgrade(), state.upgrade()) {
                save(&u, &s)
            }
        },
    );
}
fn edit(ui: &NotesApp, s: &State, text: String) {
    if s.borrow()
        .notes
        .iter()
        .filter(|n| {
            s.borrow()
                .current
                .as_ref()
                .is_none_or(|c| c.id != n.id || c.trash != n.trash)
        })
        .map(|n| n.text.len())
        .sum::<usize>()
        + text.len()
        > store::VAULT_LIMIT
    {
        ui.set_notice("Library reached its 32 MiB text limit.".into());
        if let Some(n) = &s.borrow().current {
            ui.set_content(n.text.clone().into());
        }
        return;
    }
    if let Err(e) = store::validate(&text) {
        ui.set_notice(e.into());
        if let Some(n) = &s.borrow().current {
            ui.set_content(n.text.clone().into());
        }
        return;
    }
    {
        let mut b = s.borrow_mut();
        let Some(n) = b.current.as_mut() else { return };
        if n.trash {
            return;
        }
        if n.text == text {
            return;
        }
        let previous = std::mem::replace(&mut n.text, text);
        remember(&mut b.undo, previous);
        b.redo.clear();
    }
    changed(ui, s);
}
fn save(ui: &NotesApp, s: &State) {
    if ui.get_busy() {
        return;
    }
    let n = {
        let b = s.borrow();
        b.timer.stop();
        if !b.ready {
            return;
        }
        b.current.clone()
    };
    if let Some(n) = n {
        if n.dirty() && !n.trash {
            send(ui, s, Job::Save(n));
        }
    }
}
fn remember(history: &mut Vec<String>, text: String) {
    history.push(text);
    while history.len() > 64 || history.iter().map(String::len).sum::<usize>() > 2 * 1024 * 1024 {
        history.remove(0);
    }
}
fn undo(ui: &NotesApp, s: &State, redo: bool) {
    let mut b = s.borrow_mut();
    if b.current.as_ref().is_none_or(|n| n.trash) {
        return;
    }
    let text = if redo { b.redo.pop() } else { b.undo.pop() };
    let Some(text) = text else { return };
    let n = b.current.as_mut().unwrap();
    let previous = std::mem::replace(&mut n.text, text.clone());
    if redo {
        remember(&mut b.undo, previous)
    } else {
        remember(&mut b.redo, previous)
    }
    drop(b);
    ui.set_content(text.clone().into());
    ui.invoke_caret(text.len() as i32);
    changed(ui, s);
}
fn show(ui: &NotesApp, s: &State) {
    {
        let mut b = s.borrow_mut();
        b.undo.clear();
        b.redo.clear();
    }
    let b = s.borrow();
    let n = b.current.as_ref();
    ui.set_opened(n.is_some());
    ui.set_content(n.map(|n| n.text.clone()).unwrap_or_default().into());
    ui.set_note_title(n.map(Note::title).unwrap_or_default().into());
    ui.set_notebook(n.map(|n| n.field("notebook")).unwrap_or_default().into());
    ui.set_tags(n.map(|n| n.field("tags")).unwrap_or_default().into());
    ui.set_trashed(n.is_some_and(|n| n.trash));
    ui.set_pinned(n.is_some_and(|n| n.field("pinned") == "true"));
    ui.set_modified(n.is_some_and(Note::dirty));
    ui.set_words(n.map(|n| n.text.split_whitespace().count()).unwrap_or(0) as i32);
    ui.set_status(
        if n.is_some_and(|n| n.trash) {
            "Recoverable deletion"
        } else if n.is_some_and(Note::dirty) {
            "Unsaved · autosave pending"
        } else {
            "All changes saved"
        }
        .into(),
    );
    drop(b);
    list(ui, s);
    if ui.get_preview() {
        preview(ui, s)
    }
    ui.invoke_reset_position();
    ui.invoke_focus_content();
}
fn list(ui: &NotesApp, s: &State) {
    let b = s.borrow();
    let q = ui.get_query().trim().to_lowercase();
    let folder = ui.get_folder();
    let current = b.current.as_ref();
    let all: Vec<_> = b
        .notes
        .iter()
        .map(|n| {
            current
                .filter(|c| c.id == n.id && c.trash == n.trash)
                .unwrap_or(n)
        })
        .collect();
    ui.set_all_count(all.iter().filter(|n| !n.trash).count() as i32);
    ui.set_pin_count(
        all.iter()
            .filter(|n| !n.trash && n.field("pinned") == "true")
            .count() as i32,
    );
    ui.set_trash_count(all.iter().filter(|n| n.trash).count() as i32);
    let mut books = std::collections::BTreeMap::<String, i32>::new();
    for n in &all {
        let name = n.field("notebook");
        if !n.trash && !name.is_empty() {
            *books.entry(name).or_default() += 1;
        }
    }
    ui.set_books(ModelRc::new(VecModel::from(
        books
            .into_iter()
            .map(|(name, count)| BookRow {
                selected: folder == format!("book:{name}"),
                name: name.into(),
                count,
            })
            .collect::<Vec<_>>(),
    )));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut visible: Vec<_> = all
        .into_iter()
        .filter(|n| {
            let scope = match folder.as_str() {
                "trash" => n.trash,
                "favorites" => !n.trash && n.field("pinned") == "true",
                "recent" => !n.trash && now.saturating_sub(n.modified) < 7 * 86400,
                f if f.starts_with("book:") => !n.trash && n.field("notebook") == f[5..],
                _ => !n.trash,
            };
            scope
                && (q.is_empty()
                    || n.text.to_lowercase().contains(&q)
                    || n.meta.to_lowercase().contains(&q))
        })
        .collect();
    visible.sort_by(|a, b| {
        b.field("pinned")
            .cmp(&a.field("pinned"))
            .then(b.modified.cmp(&a.modified))
            .then(a.id.cmp(&b.id))
    });
    ui.set_notes(ModelRc::new(VecModel::from(
        visible
            .into_iter()
            .map(|n| NoteRow {
                id: format!("{}{}", if n.trash { "trash:" } else { "" }, n.id).into(),
                title: n.title().into(),
                preview: n
                    .text
                    .lines()
                    .find(|l| !l.trim().is_empty() && !l.starts_with('#'))
                    .unwrap_or("A fresh page")
                    .chars()
                    .take(90)
                    .collect::<String>()
                    .into(),
                date: chrono::DateTime::from_timestamp(n.modified as i64, 0)
                    .map(|d| {
                        d.with_timezone(&chrono::Local)
                            .format("%b %-d · %H:%M")
                            .to_string()
                    })
                    .unwrap_or_default()
                    .into(),
                pinned: n.field("pinned") == "true",
                selected: current.is_some_and(|c| c.id == n.id && c.trash == n.trash),
            })
            .collect::<Vec<_>>(),
    )));
}
fn preview(ui: &NotesApp, s: &State) {
    use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
    let b = s.borrow();
    let Some(n) = &b.current else { return };
    let mut blocks = vec![];
    let mut text = String::new();
    let mut kind = 0;
    let flush = |blocks: &mut Vec<PreviewBlock>, text: &mut String, kind| {
        if !text.trim().is_empty() {
            blocks.push(PreviewBlock {
                text: std::mem::take(text).into(),
                kind,
            })
        }
    };
    for event in Parser::new_ext(
        &n.text,
        Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH,
    ) {
        match event {
            Event::Start(Tag::Heading { .. }) => {
                flush(&mut blocks, &mut text, kind);
                kind = 1;
            }
            Event::Start(Tag::CodeBlock(_)) => {
                flush(&mut blocks, &mut text, kind);
                kind = 2;
            }
            Event::Start(Tag::BlockQuote(_)) => {
                flush(&mut blocks, &mut text, kind);
                kind = 3;
            }
            Event::Start(Tag::Item) => {
                flush(&mut blocks, &mut text, kind);
                text.push_str("• ");
            }
            Event::End(
                TagEnd::Heading(_)
                | TagEnd::Paragraph
                | TagEnd::CodeBlock
                | TagEnd::Item
                | TagEnd::BlockQuote(_),
            ) => {
                flush(&mut blocks, &mut text, kind);
                kind = 0;
            }
            Event::Text(t) | Event::Code(t) => text.push_str(&t),
            Event::SoftBreak | Event::HardBreak => text.push('\n'),
            Event::TaskListMarker(v) => {
                if text == "• " {
                    text.clear()
                }
                text.push_str(if v { "☑ " } else { "☐ " })
            }
            Event::Rule => {
                flush(&mut blocks, &mut text, kind);
                text.push_str("────────────");
                flush(&mut blocks, &mut text, 0);
            }
            _ => {}
        }
    }
    flush(&mut blocks, &mut text, kind);
    ui.set_blocks(ModelRc::new(VecModel::from(blocks)));
}
fn receive(ui: &NotesApp, s: &State) {
    loop {
        let e = { s.borrow().events.try_recv() };
        let Ok(e) = e else { break };
        ui.set_busy(false);
        let result: Result<(), String> = match e {
            Event::Loaded(r) => r.map(|(notes, notice)| {
                let mut b = s.borrow_mut();
                let id = b.current.as_ref().map(|n| (n.id.clone(), n.trash));
                b.notes = notes;
                b.current = id.and_then(|(id, t)| {
                    b.notes.iter().find(|n| n.id == id && n.trash == t).cloned()
                });
                b.ready = true;
                ui.set_notice(notice.into());
                drop(b);
                show(ui, s);
            }),
            Event::Saved(r) => r.map(|saved| {
                let mut b = s.borrow_mut();
                if let Some(n) = b.current.as_mut() {
                    if n.id == saved.id {
                        n.baseline = saved.baseline.clone();
                        n.baseline_meta = saved.baseline_meta.clone();
                        n.modified = saved.modified;
                    }
                }
                if let Some(n) = b.notes.iter_mut().find(|n| n.id == saved.id && !n.trash) {
                    *n = saved;
                } else {
                    b.notes.push(saved);
                }
                b.failed = false;
                drop(b);
                ui.set_notice("".into());
                changed(ui, s);
                list(ui, s);
            }),
            Event::Trashed(id, r) => r.map(|_| {
                let mut b = s.borrow_mut();
                if let Some(n) = b.notes.iter_mut().find(|n| n.id == id && !n.trash) {
                    n.trash = true;
                }
                b.current = None;
                drop(b);
                show(ui, s);
                ui.set_notice("Moved to Trash. You can restore it from the library.".into());
            }),
            Event::Restored(r) | Event::Imported(r) => r.map(|n| {
                let mut b = s.borrow_mut();
                b.notes.retain(|a| a.id != n.id);
                b.notes.push(n.clone());
                b.current = Some(n);
                drop(b);
                ui.set_folder("all".into());
                ui.set_query("".into());
                ui.set_notice("".into());
                ui.set_dialog(0);
                show(ui, s);
            }),
            Event::Exported(r) => r.map(|_| {
                ui.set_dialog(0);
                ui.set_notice("Exported to the requested file.".into());
                ui.set_status("All changes saved".into());
            }),
        };
        if let Err(e) = result {
            let mut b = s.borrow_mut();
            b.pending = None;
            b.quitting = false;
            b.failed = true;
            ui.set_notice(e.into());
            ui.set_status("Needs attention · draft kept".into());
            continue;
        }
        let dirty = s.borrow().current.as_ref().is_some_and(Note::dirty);
        if !dirty {
            let pending = s.borrow_mut().pending.take();
            if let Some(a) = pending {
                action(ui, s, &a)
            }
            if s.borrow().quitting {
                let _ = ui.hide();
            }
        } else if s.borrow().quitting || s.borrow().pending.is_some() {
            save(ui, s)
        }
    }
}
fn expanded(s: &str) -> PathBuf {
    if let Some(p) = s.strip_prefix("~/") {
        PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(p)
    } else {
        PathBuf::from(s)
    }
}
fn action(ui: &NotesApp, s: &State, id: &str) {
    if id == "focus" {
        ui.set_focus_mode(!ui.get_focus_mode());
        ui.invoke_focus_content();
        return;
    }
    if id == "preview" {
        ui.set_preview(!ui.get_preview());
        if ui.get_preview() {
            preview(ui, s);
            ui.invoke_focus_shortcuts();
        } else {
            ui.invoke_focus_editor()
        }
        return;
    }
    if matches!(id, "undo" | "redo") {
        undo(ui, s, id == "redo");
        return;
    }
    if ui.get_busy() {
        ui.set_notice("Please wait for the current operation.".into());
        return;
    }
    if !s.borrow().ready {
        ui.set_notice(
            "The library is unavailable. Resolve the storage error, then Refresh.".into(),
        );
        if id == "reload" {
            send(ui, s, Job::Load)
        }
        return;
    }
    let navigation = id == "new"
        || id.starts_with("new:")
        || id.starts_with("open:")
        || matches!(id, "reload" | "trash" | "import");
    if navigation && s.borrow().current.as_ref().is_some_and(Note::dirty) {
        s.borrow_mut().pending = Some(id.into());
        save(ui, s);
        return;
    }
    ui.set_notice("".into());
    match id {
        "new" => new(ui, s, "Untitled"),
        a if a.starts_with("new:") => new(ui, s, &a[4..]),
        a if a.starts_with("open:") => {
            let key = &a[5..];
            let (trash, id) = key
                .strip_prefix("trash:")
                .map(|p| (true, p))
                .unwrap_or((false, key));
            let n = s
                .borrow()
                .notes
                .iter()
                .find(|n| n.id == id && n.trash == trash)
                .cloned();
            if n.is_some() {
                s.borrow_mut().current = n;
                s.borrow_mut().failed = false;
                show(ui, s);
            }
        }
        "save" => save(ui, s),
        "reload" => send(ui, s, Job::Load),
        "pin" => {
            if let Some(n) = s.borrow_mut().current.as_mut() {
                if n.trash {
                    return;
                }
                let v = n.field("pinned") != "true";
                n.set_field("pinned", if v { "true" } else { "false" });
                ui.set_pinned(v);
            }
            changed(ui, s);
            save(ui, s);
        }
        "trash" => {
            let n = s.borrow().current.clone();
            if let Some(n) = n {
                if !n.trash {
                    send(ui, s, Job::Trash(n))
                }
            }
        }
        "restore" => {
            let n = s.borrow().current.clone();
            if let Some(n) = n {
                if n.trash {
                    send(ui, s, Job::Restore(n))
                }
            }
        }
        "copy" => {
            let n = s.borrow().current.clone();
            if let Some(mut n) = n {
                let b = s.borrow();
                if b.notes.len() >= 2000
                    || b.notes.iter().map(|n| n.text.len()).sum::<usize>() + n.text.len()
                        > store::VAULT_LIMIT
                {
                    ui.set_notice(
                        "Library reached its note or text limit (2,000 notes / 32 MiB).".into(),
                    );
                    return;
                }
                drop(b);
                n.id = format!("copy-{}.md", uuid7::uuid7());
                n.baseline = None;
                n.baseline_meta = None;
                n.trash = false;
                let mut b = s.borrow_mut();
                b.current = Some(n.clone());
                b.failed = false;
                b.notes.push(n);
                drop(b);
                show(ui, s);
                save(ui, s);
            }
        }
        "import" => {
            ui.set_path("".into());
            ui.set_dialog(1);
        }
        "export" => {
            if ui.get_opened() {
                ui.set_path("".into());
                ui.set_dialog(2);
            }
        }
        "confirm" => {
            let p = expanded(ui.get_path().trim());
            if !p.is_absolute() {
                ui.set_notice("Enter an absolute file path.".into());
                return;
            }
            if ui.get_dialog() == 1 {
                {
                    let b = s.borrow();
                    let remaining = store::VAULT_LIMIT
                        .saturating_sub(b.notes.iter().map(|n| n.text.len()).sum::<usize>());
                    if b.notes.len() >= 2000 {
                        ui.set_notice("The library supports up to 2,000 notes.".into());
                        return;
                    }
                    drop(b);
                    send(ui, s, Job::Import(p, remaining))
                }
            } else if ui.get_dialog() == 2 {
                send(ui, s, Job::Export(p, ui.get_content().to_string()))
            }
        }
        _ => {}
    }
}
fn new(ui: &NotesApp, s: &State, title: &str) {
    if s.borrow().notes.len() >= 2000
        || s.borrow().notes.iter().map(|n| n.text.len()).sum::<usize>() + 1024 > store::VAULT_LIMIT
    {
        ui.set_notice("The library supports up to 2,000 notes.".into());
        return;
    }
    let n = Note::blank(&title.chars().take(160).collect::<String>());
    let mut b = s.borrow_mut();
    b.notes.push(n.clone());
    b.current = Some(n);
    b.failed = false;
    drop(b);
    ui.set_folder("all".into());
    ui.set_query("".into());
    ui.set_preview(false);
    show(ui, s);
    ui.invoke_caret(ui.get_content().len() as i32);
    save(ui, s);
}
fn control(ui: &NotesApp, s: &State) {
    use yantrik_app_runtime::control::{Action, App, Param, View};
    let w = ui.as_weak();
    let b = s.clone();
    let mut app=App::new("notes").describe(move||{
  let Some(u)=w.upgrade()else{return View::new("Notes closed")};let b=b.borrow();let n=b.current.as_ref();
  View::new("Notes").with("open_note",n.map(|n|n.id.clone())).with("title",n.map(Note::title)).with("unsaved",n.is_some_and(Note::dirty)).with("content",n.map(|n|n.text.chars().take(4000).collect::<String>())).with("busy",u.get_busy()).with("notice",u.get_notice().to_string()).with("status",u.get_status().to_string()).with("folder",u.get_folder().to_string()).with("preview",u.get_preview()).with("focus",u.get_focus_mode()).with("note_count",u.get_all_count()).with("trash_count",u.get_trash_count()).with("search_query",u.get_query().to_string()).with("matches",{use slint::Model;u.get_notes().row_count()}).with("notes",b.notes.iter().take(100).map(|n|serde_json::json!({"filename":n.id,"title":n.title(),"trash":n.trash,"pinned":n.field("pinned")=="true","notebook":n.field("notebook")})).collect::<Vec<_>>())
 });
    for name in [
        "new_note",
        "open_note",
        "set_title",
        "append",
        "set_content",
        "search",
        "set_folder",
        "notebook",
        "tags",
        "save",
        "trash",
        "restore",
        "copy",
        "reload",
        "preview",
        "focus",
        "import",
        "export",
    ] {
        let param = match name {
            "new_note" | "open_note" | "set_title" => "title",
            "search" => "query",
            "set_folder" => "folder",
            "import" | "export" => "path",
            _ => "text",
        };
        let mut a = Action::new(name, &format!("Notes: {name}"));
        if !matches!(
            name,
            "save" | "trash" | "restore" | "copy" | "reload" | "preview" | "focus"
        ) {
            a = a.arg(Param::text(param).optional());
        }
        let w = ui.as_weak();
        let b = s.clone();
        app = app.action(a, move |args| {
            let u = w.upgrade().ok_or("Notes closed")?;
            if u.get_busy() {
                return Err("Notes is busy".into());
            }
            let value = args[param].as_str().unwrap_or_default();
            match name {
                "new_note" => action(
                    &u,
                    &b,
                    &format!("new:{}", if value.is_empty() { "Untitled" } else { value }),
                ),
                "open_note" => {
                    let key = {
                        let b = b.borrow();
                        let n = b
                            .notes
                            .iter()
                            .find(|n| n.id == value || n.title() == value)
                            .ok_or("Note not found")?;
                        format!("open:{}{}", if n.trash { "trash:" } else { "" }, n.id)
                    };
                    action(&u, &b, &key)
                }
                "set_title" | "append" | "set_content" => {
                    let mut text = b
                        .borrow()
                        .current
                        .as_ref()
                        .filter(|n| !n.trash)
                        .ok_or("Open a note first")?
                        .text
                        .clone();
                    if name == "append" {
                        text.push_str(value)
                    } else if name == "set_content" {
                        text = value.into()
                    } else {
                        let title = value.replace(['\r', '\n'], " ");
                        let mut replaced = false;
                        text = text
                            .lines()
                            .map(|l| {
                                if !replaced && l.starts_with("# ") {
                                    replaced = true;
                                    format!("# {title}")
                                } else {
                                    l.into()
                                }
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        if !replaced {
                            text = format!("# {title}\n\n{text}")
                        }
                    }
                    store::validate(&text)?;
                    u.set_content(text.clone().into());
                    edit(&u, &b, text);
                }
                "search" => {
                    u.set_query(value.into());
                    list(&u, &b)
                }
                "set_folder" => {
                    u.set_folder(value.into());
                    list(&u, &b)
                }
                "notebook" | "tags" => {
                    if name == "notebook" {
                        u.set_notebook(value.into())
                    } else {
                        u.set_tags(value.into())
                    }
                    u.invoke_metadata()
                }
                "import" | "export" => {
                    if b.borrow().current.as_ref().is_some_and(Note::dirty) {
                        return Err("Save the current draft first".into());
                    }
                    u.set_dialog(if name == "import" { 1 } else { 2 });
                    u.set_path(value.into());
                    action(&u, &b, "confirm")
                }
                _ => action(&u, &b, name),
            }
            Ok(serde_json::json!({"accepted":true,"completed":!u.get_busy()}))
        });
    }
    app.serve();
}
#[cfg(test)]
mod tests;
