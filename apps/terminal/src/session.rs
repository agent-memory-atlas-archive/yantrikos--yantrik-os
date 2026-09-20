//! A bounded PTY session. One worker sleeps on the PTY and a wake socket;
//! the UI never waits for a command, a write, or a periodic output poll.
use std::{
    collections::VecDeque,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, OwnedFd},
        unix::net::UnixStream,
    },
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{sync_channel, SyncSender},
        Arc, Mutex,
    },
};

pub const HISTORY_LINES: usize = 1000;
const INPUT_LIMIT: usize = 64 * 1024;
type Notify = Arc<dyn Fn() + Send + Sync>;
enum Input {
    Bytes(Vec<u8>),
    Resize(u16, u16),
}
struct State {
    parser: Mutex<vt100::Parser>,
    alive: AtomicBool,
    closing: AtomicBool,
    exit: Mutex<Option<i32>>,
    error: Mutex<Option<String>>,
    revision: AtomicU64,
}
pub struct Session {
    state: Arc<State>,
    input: SyncSender<Input>,
    wake: UnixStream,
    pid: u32,
    initial_dir: PathBuf,
    notify: Notify,
    worker: Option<std::thread::JoinHandle<()>>,
}
#[derive(Clone, Debug)]
pub struct Run {
    pub text: String,
    pub row: u16,
    pub col: u16,
    pub width: u16,
    pub fg: (u8, u8, u8),
    pub bg: (u8, u8, u8),
    pub bold: bool,
    pub underline: bool,
}
pub struct Snapshot {
    pub text: String,
    pub runs: Vec<Run>,
    pub cursor: (u16, u16),
    pub cursor_visible: bool,
    pub alive: bool,
    pub exit: Option<i32>,
    pub error: Option<String>,
    pub scrollback: usize,
    pub size: (u16, u16),
}

impl Session {
    pub fn spawn(dir: &Path, rows: u16, cols: u16, notify: Notify) -> anyhow::Result<Self> {
        let shell = std::env::var("SHELL")
            .ok()
            .filter(|s| Path::new(s).is_file())
            .unwrap_or_else(|| {
                if Path::new("/bin/bash").exists() {
                    "/bin/bash".into()
                } else {
                    "/bin/sh".into()
                }
            });
        Self::spawn_shell(&shell, &["-i"], dir, rows, cols, notify)
    }

    pub fn spawn_shell(
        shell: &str,
        args: &[&str],
        dir: &Path,
        rows: u16,
        cols: u16,
        notify: Notify,
    ) -> anyhow::Result<Self> {
        let (rows, cols) = dimensions(rows, cols);
        let (pty, pts) = pty_process::blocking::open()?;
        pty.resize(pty_process::Size::new(rows, cols))?;
        let (wake, mut wake_reader) = UnixStream::pair()?;
        wake.set_nonblocking(true)?;
        wake_reader.set_nonblocking(true)?;
        let fd: OwnedFd = pty.into();
        let mut master = std::fs::File::from(fd);
        let raw = master.as_raw_fd();
        unsafe {
            let flags = libc::fcntl(raw, libc::F_GETFL);
            if flags < 0 || libc::fcntl(raw, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        let mut child = pty_process::blocking::Command::new(shell)
            .args(args)
            .current_dir(dir)
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
            .env_remove("COLUMNS")
            .env_remove("LINES")
            .spawn(pts)?;
        let pid = child.id();
        let state = Arc::new(State {
            parser: Mutex::new(vt100::Parser::new(rows, cols, HISTORY_LINES)),
            alive: AtomicBool::new(true),
            closing: AtomicBool::new(false),
            exit: Mutex::new(None),
            error: Mutex::new(None),
            revision: AtomicU64::new(1),
        });
        let (input, receiver) = sync_channel::<Input>(32);
        let shared = state.clone();
        let changed = notify.clone();
        // Each session owns one worker, sleeping until PTY or input activity.
        let worker = std::thread::spawn(move || {
            let mut pending = VecDeque::<u8>::new();
            let mut bytes = [0u8; 8192];
            'worker: loop {
                if shared.closing.load(Ordering::Acquire) {
                    break;
                }
                let mut poll = [
                    libc::pollfd {
                        fd: raw,
                        events: libc::POLLIN | if pending.is_empty() { 0 } else { libc::POLLOUT },
                        revents: 0,
                    },
                    libc::pollfd {
                        fd: wake_reader.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    },
                ];
                if unsafe { libc::poll(poll.as_mut_ptr(), 2, -1) } < 0 {
                    if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    break;
                }
                if poll[1].revents != 0 {
                    while wake_reader.read(&mut bytes).is_ok_and(|n| n > 0) {}
                    if shared.closing.load(Ordering::Acquire) {
                        break;
                    }
                    while let Ok(message) = receiver.try_recv() {
                        match message {
                            Input::Bytes(data) => {
                                if pending.len() + data.len() <= INPUT_LIMIT {
                                    pending.extend(data);
                                } else {
                                    *shared.error.lock().unwrap() =
                                        Some("Input queue is full; paste a smaller block.".into());
                                    shared.revision.fetch_add(1, Ordering::Release);
                                    changed();
                                }
                            }
                            Input::Resize(rows, cols) => {
                                let size = libc::winsize {
                                    ws_row: rows,
                                    ws_col: cols,
                                    ws_xpixel: 0,
                                    ws_ypixel: 0,
                                };
                                if unsafe { libc::ioctl(raw, libc::TIOCSWINSZ, &size) } == 0 {
                                    shared
                                        .parser
                                        .lock()
                                        .unwrap()
                                        .screen_mut()
                                        .set_size(rows, cols);
                                    shared.revision.fetch_add(1, Ordering::Release);
                                    changed();
                                }
                            }
                        }
                    }
                }
                if !pending.is_empty() {
                    match master.write(pending.make_contiguous()) {
                        Ok(n) => {
                            pending.drain(..n);
                        }
                        Err(e)
                            if e.kind() == std::io::ErrorKind::WouldBlock
                                || e.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(_) => break,
                    }
                }
                if poll[0].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
                    // Bound each batch so a producer cannot starve input or resize.
                    for _ in 0..16 {
                        match master.read(&mut bytes) {
                            Ok(0) => break 'worker,
                            Ok(n) => {
                                shared.parser.lock().unwrap().process(&bytes[..n]);
                                shared.revision.fetch_add(1, Ordering::Release);
                                changed();
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                            Err(_) => break 'worker, // Linux returns EIO after the slave closes.
                        }
                    }
                }
            }
            // Closing a tab also closes its foreground job and reaps its shell.
            let foreground = unsafe { libc::tcgetpgrp(raw) };
            if shared.closing.load(Ordering::Acquire) {
                unsafe {
                    if foreground > 0 && foreground != libc::getpgrp() {
                        libc::kill(-foreground, libc::SIGHUP);
                    }
                    libc::kill(-(pid as i32), libc::SIGHUP);
                }
            }
            drop(master);
            let mut status = None;
            for _ in 0..25 {
                if let Ok(Some(done)) = child.try_wait() {
                    status = Some(done);
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            if status.is_none() {
                unsafe {
                    if foreground > 0 && libc::getsid(foreground) == pid as i32 {
                        libc::kill(-foreground, libc::SIGKILL);
                    }
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
                let _ = child.kill();
                status = child.wait().ok();
            }
            *shared.exit.lock().unwrap() = status.and_then(|s| s.code());
            shared.alive.store(false, Ordering::Release);
            shared.revision.fetch_add(1, Ordering::Release);
            changed();
        });
        Ok(Self {
            state,
            input,
            wake,
            pid,
            initial_dir: dir.to_owned(),
            notify,
            worker: Some(worker),
        })
    }
    fn send(&self, input: Input) -> Result<(), String> {
        if !self.alive() {
            return Err("This shell has exited. Start a new session.".into());
        }
        self.input
            .try_send(input)
            .map_err(|_| "Terminal input is busy; try again.".to_string())?;
        let _ = (&self.wake).write(&[1]);
        Ok(())
    }
    pub fn write(&self, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() > INPUT_LIMIT {
            return Err("Paste is limited to 64 KiB.".into());
        }
        self.send(Input::Bytes(bytes.to_vec()))
    }
    pub fn paste(&self, text: &str) -> Result<(), String> {
        // Never permit clipboard escape sequences to leave bracketed paste mode.
        let text: String = text
            .chars()
            .filter(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'))
            .collect();
        if self.state.parser.lock().unwrap().screen().bracketed_paste() {
            self.write(format!("\x1b[200~{text}\x1b[201~").as_bytes())
        } else {
            self.write(text.as_bytes())
        }
    }
    pub fn resize(&self, rows: u16, cols: u16) -> Result<(), String> {
        let size = dimensions(rows, cols);
        if self.state.parser.lock().unwrap().screen().size() == size {
            return Ok(());
        }
        self.send(Input::Resize(size.0, size.1))
    }
    pub fn alive(&self) -> bool {
        self.state.alive.load(Ordering::Acquire)
    }
    pub fn pid(&self) -> u32 {
        self.pid
    }
    pub fn revision(&self) -> u64 {
        self.state.revision.load(Ordering::Acquire)
    }
    pub fn clear_error(&self) {
        *self.state.error.lock().unwrap() = None;
    }
    pub fn shutdown(mut self) {
        self.state.closing.store(true, Ordering::Release);
        let _ = (&self.wake).write(&[1]);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
    pub fn has_children(&self) -> bool {
        self.alive()
            && std::fs::read_to_string(format!("/proc/{0}/task/{0}/children", self.pid))
                .is_ok_and(|s| !s.trim().is_empty())
    }
    pub fn cwd(&self) -> PathBuf {
        std::fs::read_link(format!("/proc/{}/cwd", self.pid))
            .unwrap_or_else(|_| self.initial_dir.clone())
    }
    pub fn application_cursor(&self) -> bool {
        self.state
            .parser
            .lock()
            .unwrap()
            .screen()
            .application_cursor()
    }
    pub fn set_scrollback(&self, offset: usize) {
        self.state
            .parser
            .lock()
            .unwrap()
            .screen_mut()
            .set_scrollback(offset);
        self.state.revision.fetch_add(1, Ordering::Release);
        (self.notify)();
    }
    pub fn scroll(&self, lines: i32) {
        let mut parser = self.state.parser.lock().unwrap();
        let offset = parser
            .screen()
            .scrollback()
            .saturating_add_signed(lines as isize);
        parser.screen_mut().set_scrollback(offset);
        drop(parser);
        self.state.revision.fetch_add(1, Ordering::Release);
        (self.notify)();
    }
    pub fn history(&self) -> Vec<String> {
        let mut parser = self.state.parser.lock().unwrap();
        let saved = parser.screen().scrollback();
        let (rows, cols) = parser.screen().size();
        parser.screen_mut().set_scrollback(usize::MAX);
        let mut remaining = parser.screen().scrollback();
        let mut lines = Vec::new();
        while remaining > 0 {
            let take = remaining.min(rows as usize);
            lines.extend(parser.screen().rows(0, cols).take(take));
            remaining -= take;
            parser.screen_mut().set_scrollback(remaining);
        }
        lines.extend(parser.screen().rows(0, cols));
        parser.screen_mut().set_scrollback(saved);
        lines
    }
    pub fn snapshot(&self) -> Snapshot {
        let parser = self.state.parser.lock().unwrap();
        let screen = parser.screen();
        let (rows, cols) = screen.size();
        let mut runs: Vec<Run> = Vec::new();
        for row in 0..rows {
            for col in 0..cols {
                let Some(cell) = screen.cell(row, col) else {
                    continue;
                };
                if cell.is_wide_continuation() {
                    continue;
                }
                let mut fg = color(cell.fgcolor(), (222, 230, 239));
                let mut bg = color(cell.bgcolor(), (16, 23, 30));
                if cell.inverse() {
                    std::mem::swap(&mut fg, &mut bg);
                }
                if cell.dim() {
                    fg = (fg.0 / 2, fg.1 / 2, fg.2 / 2);
                }
                let text = if cell.contents().is_empty() {
                    " "
                } else {
                    cell.contents()
                };
                let width = if cell.is_wide() { 2 } else { 1 };
                if let Some(last) = runs.last_mut().filter(|r| {
                    r.row == row
                        && r.col + r.width == col
                        && r.fg == fg
                        && r.bg == bg
                        && r.bold == cell.bold()
                        && r.underline == cell.underline()
                }) {
                    last.text.push_str(text);
                    last.width += width;
                } else {
                    runs.push(Run {
                        text: text.into(),
                        row,
                        col,
                        width,
                        fg,
                        bg,
                        bold: cell.bold(),
                        underline: cell.underline(),
                    });
                }
            }
        }
        Snapshot {
            text: screen.contents(),
            runs,
            cursor: screen.cursor_position(),
            cursor_visible: !screen.hide_cursor() && screen.scrollback() == 0 && self.alive(),
            alive: self.alive(),
            exit: *self.state.exit.lock().unwrap(),
            error: self.state.error.lock().unwrap().clone(),
            scrollback: screen.scrollback(),
            size: (rows, cols),
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.state.closing.store(true, Ordering::Release);
        let _ = (&self.wake).write(&[1]);
    }
}
fn dimensions(rows: u16, cols: u16) -> (u16, u16) {
    (rows.clamp(2, 160), cols.clamp(8, 400))
}
fn color(value: vt100::Color, default: (u8, u8, u8)) -> (u8, u8, u8) {
    const ANSI: [(u8, u8, u8); 16] = [
        (24, 31, 40),
        (240, 115, 125),
        (130, 207, 156),
        (238, 203, 129),
        (126, 176, 244),
        (195, 158, 237),
        (110, 211, 207),
        (222, 230, 239),
        (116, 131, 150),
        (255, 151, 157),
        (163, 229, 182),
        (250, 223, 164),
        (163, 199, 255),
        (219, 185, 255),
        (156, 234, 230),
        (247, 249, 252),
    ];
    match value {
        vt100::Color::Default => default,
        vt100::Color::Rgb(r, g, b) => (r, g, b),
        vt100::Color::Idx(i @ 0..=15) => ANSI[i as usize],
        vt100::Color::Idx(i @ 16..=231) => {
            let n = i - 16;
            let c = |v| if v == 0 { 0 } else { 55 + 40 * v };
            (c(n / 36), c((n / 6) % 6), c(n % 6))
        }
        vt100::Color::Idx(i) => {
            let v = 8 + 10 * (i - 232);
            (v, v, v)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    fn wait(mut condition: impl FnMut() -> bool) {
        let until = Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(Instant::now() < until, "PTY condition timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn session() -> Session {
        Session::spawn_shell(
            "/bin/sh",
            &["-i"],
            Path::new("/tmp"),
            24,
            80,
            Arc::new(|| {}),
        )
        .unwrap()
    }
    #[test]
    fn persistent_shell_and_interactive_input() {
        let s = session();
        s.write(b"export YANTRIK_TEST=retained; cd /; printf 'READY:%s\\n' \"$YANTRIK_TEST\"\n")
            .unwrap();
        wait(|| s.snapshot().text.contains("READY:retained"));
        s.write(b"printf 'STATE:%s:%s\\n' \"$PWD\" \"$YANTRIK_TEST\"\n")
            .unwrap();
        wait(|| s.snapshot().text.contains("STATE:/:retained"));
        s.write(b"read answer; printf 'ANSWER:%s\\n' \"$answer\"\n")
            .unwrap();
        s.write(b"interactive\n").unwrap();
        wait(|| s.snapshot().text.contains("ANSWER:interactive"));
        assert_eq!(s.cwd(), Path::new("/"));
    }
    #[test]
    fn interrupt_resize_ansi_and_exit() {
        let s = session();
        s.resize(30, 100).unwrap();
        wait(|| s.snapshot().size == (30, 100));
        s.write(b"stty size; printf '\\033[31mRED_OK\\033[0m\\n'; sleep 30\n")
            .unwrap();
        wait(|| {
            s.snapshot().text.contains("30 100")
                && s.snapshot().runs.iter().any(|r| {
                    r.text.contains("RED_OK") && r.fg == color(vt100::Color::Idx(1), (0, 0, 0))
                })
        });
        wait(|| s.has_children());
        s.write(b"\x03").unwrap();
        wait(|| !s.has_children());
        s.write(b"printf 'INTERRUPTED_OK\\n'\n").unwrap();
        wait(|| {
            s.snapshot()
                .text
                .lines()
                .any(|l| l.trim() == "INTERRUPTED_OK")
        });
        s.write(b"exit 7\n").unwrap();
        wait(|| !s.alive());
        assert_eq!(s.snapshot().exit, Some(7));
        assert!(s.write(b"x").is_err());
    }
    #[test]
    fn bounded_history_independent_tabs_and_cleanup() {
        let s = session();
        let other = session();
        s.write(
            b"i=0; while [ $i -lt 1200 ]; do echo line-$i; i=$((i+1)); done; echo HISTORY_DONE\n",
        )
        .unwrap();
        wait(|| s.snapshot().text.lines().any(|line| line == "HISTORY_DONE"));
        let history = s.history();
        assert!(history.len() <= HISTORY_LINES + 24);
        assert!(history.iter().any(|l| l == "line-1199"));
        assert!(!other.snapshot().text.contains("line-1199"));
        s.set_scrollback(100);
        assert_eq!(s.snapshot().scrollback, 100);
        assert!(!s.snapshot().cursor_visible);
        let pid = s.pid();
        drop(s);
        wait(|| !Path::new(&format!("/proc/{pid}")).exists());
        assert!(other.alive());
    }
}
