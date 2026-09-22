//! The headless verifier: does the built game actually play?
//!
//! Playwright's Python package does not install cleanly in this WSL image and its
//! browser download is exactly the kind of network dependency a self-contained kit
//! should not grow, so the verifier drives a Chromium directly over the DevTools
//! protocol instead: HTTP to discover the page target (`ureq`), one websocket for
//! everything else (`tungstenite`), both already workspace dependencies. Any
//! Chromium-class browser will do; the discovery list matches the one the desktop's
//! Browser route uses.
//!
//! The gates are the charter's, in the charter's order:
//!
//!  1. boots — the page loads, the engine starts, frames advance
//!  2. no_console_errors — across the whole session, CDP events and the engine's
//!     own window.onerror collection
//!  3. frame_renders — a real screenshot decodes and is not one flat colour
//!  4. input_moves_player — a dispatched arrow key moves the player's position
//!  5. bot_reaches_win — the greedy bot, steering through the ordinary input
//!     vector, collects every item
//!  6. bot_reaches_lose — the suicidal bot exhausts the lives
//!  7. frame_budget — average frame time under software rendering stays under a
//!     generous budget; a game that crawls in swiftshader is a broken game
//!
//! The session logic is split from Chrome discovery and launching
//! (`verify_session` takes a websocket URL), so the unit test runs the entire
//! gate sequence against a fake CDP server — no browser, no display, no luck.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tungstenite::protocol::Message;
use tungstenite::WebSocket;

/// Average frame-time ceiling in milliseconds, measured by the engine over its
/// last 90 frames. This is a floor for "not broken", not a quality bar: headless
/// Chromium in WSL renders through swiftshader (software), which is many times
/// slower than any GPU the game will actually run on. 250 ms means the software
/// renderer still managed 4 fps; a real machine clears that trivially, and a game
/// that misses it has a runaway loop or a pathological draw count.
pub const FRAME_BUDGET_MS: f64 = 250.0;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Gate {
    pub gate: String,
    pub passed: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Report {
    pub passed: bool,
    pub when: String,
    pub browser: String,
    pub frame_budget_ms: f64,
    pub gates: Vec<Gate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<String>,
}

impl Report {
    fn new(browser: String) -> Report {
        Report {
            passed: false,
            when: chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
            browser,
            frame_budget_ms: FRAME_BUDGET_MS,
            gates: Vec::new(),
            screenshot: None,
        }
    }

    fn gate(&mut self, name: &str, passed: bool, detail: impl Into<String>) -> bool {
        self.gates.push(Gate { gate: name.into(), passed, detail: detail.into() });
        passed
    }

    /// Mark every remaining gate as not run, so a report always shows the full
    /// charter list and an early failure explains what never happened.
    fn abandon(&mut self, names: &[&str], reason: &str) {
        for n in names {
            self.gate(n, false, format!("not run: {reason}"));
        }
    }

    fn finish(mut self) -> Report {
        self.passed = self.gates.iter().all(|g| g.passed);
        self
    }
}

const ALL_GATES: [&str; 7] = [
    "boots",
    "no_console_errors",
    "frame_renders",
    "input_moves_player",
    "bot_reaches_win",
    "bot_reaches_lose",
    "frame_budget",
];

// ── Chrome discovery ───────────────────────────────────────────────

/// The same candidate list the desktop's Browser route uses.
pub fn chrome_binary() -> Option<PathBuf> {
    ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser", "brave-browser", "microsoft-edge"]
        .iter()
        .map(PathBuf::from)
        .find(|b| {
            std::process::Command::new(b)
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok()
        })
}

// ── Headless launch ────────────────────────────────────────────────

pub struct Headless {
    child: Child,
    pub ws_url: String,
    pub browser: String,
    profile: PathBuf,
}

impl Drop for Headless {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.profile);
    }
}

/// Launch a headless Chromium showing the given file, and find its page target.
pub fn launch_headless(html: &Path) -> Result<Headless, String> {
    let binary = chrome_binary().ok_or_else(|| {
        "no Chromium-class browser found (looked for google-chrome, chromium, chromium-browser, brave-browser, microsoft-edge); install one to verify".to_string()
    })?;
    let abs = std::fs::canonicalize(html)
        .map_err(|e| format!("cannot read {}: {e}", html.display()))?;
    let url = format!("file://{}", abs.display());

    let profile = std::env::temp_dir().join(format!("arcade-verify-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&profile);
    std::fs::create_dir_all(&profile).map_err(|e| format!("cannot create a profile directory: {e}"))?;

    let child = Command::new(&binary)
        // Software rendering: WSL here has no GPU path for headless Chromium.
        .args([
            "--headless=new",
            "--use-gl=angle",
            "--use-angle=swiftshader",
            "--enable-unsafe-swiftshader",
            "--no-sandbox",
            "--disable-dev-shm-usage",
            "--mute-audio",
            "--hide-scrollbars",
            "--window-size=1280,720",
            "--remote-debugging-port=0",
        ])
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg(&url)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", binary.display()))?;

    let mut headless = Headless { child, ws_url: String::new(), browser: binary.display().to_string(), profile: profile.clone() };

    // The port lands in DevToolsActivePort once the debugger is up.
    let port_file = profile.join("DevToolsActivePort");
    let deadline = Instant::now() + Duration::from_secs(30);
    let port = loop {
        if Instant::now() > deadline {
            return Err("the browser did not open its debugging port within 30 s".into());
        }
        if let Ok(text) = std::fs::read_to_string(&port_file) {
            if let Some(first) = text.lines().next() {
                if let Ok(p) = first.trim().parse::<u16>() {
                    break p;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    };

    let base = format!("http://127.0.0.1:{port}");
    let ws_url = page_ws_url(&base)?;
    headless.ws_url = ws_url;
    Ok(headless)
}

/// GET /json/list and take the first page target's debugger URL.
pub fn page_ws_url(http_base: &str) -> Result<String, String> {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut last = "no targets yet".to_string();
    while Instant::now() < deadline {
        match ureq::get(&format!("{http_base}/json/list")).call() {
            Ok(resp) => match resp.into_json::<Value>() {
                Ok(list) => {
                    if let Some(targets) = list.as_array() {
                        for t in targets {
                            if t.get("type").and_then(|v| v.as_str()) == Some("page") {
                                if let Some(ws) = t.get("webSocketDebuggerUrl").and_then(|v| v.as_str()) {
                                    return Ok(ws.to_string());
                                }
                            }
                        }
                        last = "the browser listed targets but no page among them".into();
                    }
                }
                Err(e) => last = format!("/json/list answered something that is not JSON: {e}"),
            },
            Err(e) => last = format!("/json/list: {e}"),
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err(format!("no page target from the browser ({last})"))
}

// ── The CDP session ────────────────────────────────────────────────

pub struct Session {
    conn: WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
    next_id: AtomicU64,
    events: Vec<Value>,
}

impl Session {
    pub fn connect(ws_url: &str) -> Result<Session, String> {
        let (conn, _) = tungstenite::connect(ws_url).map_err(|e| format!("cannot reach the browser's websocket: {e}"))?;
        // Reads must not block forever: the gate loop polls between them. The
        // stream is plain ws:// (the browser listens on 127.0.0.1), and tungstenite
        // only hands out the TcpStream through the enum, not a getter.
        if let tungstenite::stream::MaybeTlsStream::Plain(tcp) = conn.get_ref() {
            let _ = tcp.set_read_timeout(Some(Duration::from_millis(150)));
        }
        let session = Session { conn, next_id: AtomicU64::new(1), events: Vec::new() };
        Ok(session)
    }

    /// Send a command and wait for its answer, parking events on the way.
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let msg = json!({ "id": id, "method": method, "params": params });
        self.conn
            .send(Message::Text(msg.to_string()))
            .map_err(|e| format!("the browser websocket refused a {method} command: {e}"))?;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if Instant::now() > deadline {
                return Err(format!("{method} did not answer within 30 s"));
            }
            match self.conn.read() {
                Ok(Message::Text(text)) => {
                    if let Ok(v) = serde_json::from_str::<Value>(&text) {
                        if v.get("id").and_then(|i| i.as_u64()) == Some(id) {
                            if let Some(err) = v.get("error") {
                                return Err(format!("{method}: {}", err.get("message").and_then(|m| m.as_str()).unwrap_or("unknown error")));
                            }
                            return Ok(v.get("result").cloned().unwrap_or(json!({})));
                        }
                        if v.get("method").is_some() {
                            self.events.push(v);
                        }
                    }
                }
                Ok(Message::Ping(data)) => {
                    let _ = self.conn.send(Message::Pong(data));
                }
                Ok(Message::Close(_)) => return Err("the browser closed the websocket".into()),
                Ok(_) => {}
                Err(tungstenite::Error::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(tungstenite::Error::Io(e)) if e.kind() == std::io::ErrorKind::TimedOut => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => return Err(format!("the browser websocket failed: {e}")),
            }
        }
    }

    /// Collect events that arrived since the last drain, without waiting.
    pub fn drain_events(&mut self) -> Vec<Value> {
        // One non-blocking sweep: read whatever is already buffered.
        loop {
            match self.conn.read() {
                Ok(Message::Text(text)) => {
                    if let Ok(v) = serde_json::from_str::<Value>(&text) {
                        if v.get("method").is_some() {
                            self.events.push(v);
                        }
                    }
                }
                Ok(Message::Ping(data)) => {
                    let _ = self.conn.send(Message::Pong(data));
                }
                Ok(_) => {}
                Err(tungstenite::Error::Io(e))
                    if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) =>
                {
                    break
                }
                Err(_) => break,
            }
        }
        std::mem::take(&mut self.events)
    }

    /// Runtime.evaluate with returnByValue: the engine's hooks all return plain data.
    pub fn evaluate(&mut self, expression: &str) -> Result<Value, String> {
        let result = self.call(
            "Runtime.evaluate",
            json!({ "expression": expression, "returnByValue": true }),
        )?;
        if let Some(exc) = result.get("exceptionDetails") {
            let text = exc
                .get("exception")
                .and_then(|e| e.get("description"))
                .and_then(|d| d.as_str())
                .unwrap_or("an exception with no description");
            return Err(format!("the page threw: {text}"));
        }
        Ok(result
            .get("result")
            .and_then(|r| r.get("value"))
            .cloned()
            .unwrap_or(Value::Null))
    }
}

/// Errors a page reports on its own: console.error calls, uncaught exceptions,
/// and browser log entries at error level.
fn event_errors(events: &[Value]) -> Vec<String> {
    let mut out = Vec::new();
    for e in events {
        match e.get("method").and_then(|m| m.as_str()) {
            Some("Runtime.consoleAPICalled") => {
                if e.pointer("/params/type").and_then(|t| t.as_str()) == Some("error") {
                    let text: Vec<String> = e
                        .pointer("/params/args")
                        .and_then(|a| a.as_array())
                        .map(|args| {
                            args.iter()
                                .map(|a| {
                                    a.get("value")
                                        .and_then(|v| v.as_str())
                                        .map(String::from)
                                        .unwrap_or_else(|| a.get("description").and_then(|d| d.as_str()).unwrap_or("?").to_string())
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    out.push(format!("console.error: {}", text.join(" ")));
                }
            }
            Some("Runtime.exceptionThrown") => {
                let text = e
                    .pointer("/params/exceptionDetails/exception/description")
                    .and_then(|d| d.as_str())
                    .or_else(|| e.pointer("/params/exceptionDetails/text").and_then(|d| d.as_str()))
                    .unwrap_or("an uncaught exception");
                out.push(format!("page exception: {text}"));
            }
            Some("Log.entryAdded") => {
                if e.pointer("/params/entry/level").and_then(|l| l.as_str()) == Some("error") {
                    let text = e.pointer("/params/entry/text").and_then(|t| t.as_str()).unwrap_or("?");
                    out.push(format!("browser log: {text}"));
                }
            }
            _ => {}
        }
    }
    out
}

// ── The gate sequence ──────────────────────────────────────────────

/// Run the full verification against an already-discovered page target. This is
/// the half the unit test exercises against a fake CDP server.
pub fn verify_session(ws_url: &str, browser: &str, screenshot_out: Option<&Path>) -> Result<Report, String> {
    let mut report = Report::new(browser.to_string());
    let mut session = Session::connect(ws_url)?;
    let mut page_events: Vec<Value> = Vec::new();

    session.call("Runtime.enable", json!({}))?;
    session.call("Log.enable", json!({}))?;
    session.call("Page.enable", json!({}))?;

    // Gate 1: boots. The engine flips status to "playing" only after its first
    // rendered frame, so frames > 0 and status playing means the whole stack —
    // Three.js, WebGL, spec parsing, arena construction — came up.
    let boot = wait_for_state(&mut session, Duration::from_secs(20), &|s| {
        s.get("status").and_then(|v| v.as_str()) == Some("playing")
            && s.get("frames").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0
    });
    let (boot_ok, boot_detail, target) = match &boot {
        Some(state) => {
            let frames = state.get("frames").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let target = state.get("target").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
            (true, format!("engine up, {frames:.0} frames rendered"), target)
        }
        None => {
            let state = session
                .evaluate("window.__arcade ? JSON.stringify(window.__arcade.state()) : 'no engine'")
                .unwrap_or(Value::Null);
            (false, format!("the engine never reported playing (last state: {state})"), 10usize)
        }
    };
    if !boot_ok {
        report.gate("boots", false, boot_detail.clone());
        report.abandon(&ALL_GATES[1..], "the game did not boot");
        return Ok(report.finish());
    }

    // Gate 3 runs here (the report keeps the charter's order regardless): a real
    // screenshot of a real frame.
    let shot = capture_screenshot(&mut session);
    let (shot_ok, shot_detail, png_bytes) = match shot {
        Ok(bytes) => match png_distinct_colours(&bytes) {
            Ok(n) if n >= 8 => (true, format!("frame has {n} distinct colours"), Some(bytes)),
            Ok(n) => (false, format!("the frame is nearly blank: only {n} distinct colours"), Some(bytes)),
            Err(e) => (false, format!("the screenshot is not a readable PNG: {e}"), None),
        },
        Err(e) => (false, format!("no screenshot from the browser: {e}"), None),
    };
    if let (Some(bytes), Some(out)) = (&png_bytes, screenshot_out) {
        if let Some(parent) = out.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(out, bytes).map_err(|e| format!("cannot write {}: {e}", out.display()))?;
        report.screenshot = out.file_name().and_then(|n| n.to_str()).map(String::from);
    }

    // Gate 4: input moves the player. A real dispatched key event, through the
    // engine's real handler, into the real movement code.
    let before = session.evaluate("JSON.stringify(window.__arcade.state())").ok();
    let key_result = dispatch_key(&mut session, "KeyW", "w", 87);
    std::thread::sleep(Duration::from_millis(1400));
    let after = key_result
        .and_then(|()| session.evaluate("JSON.stringify(window.__arcade.state())"))
        .ok();
    let (input_ok, input_detail) = match (pos_of(&before), pos_of(&after)) {
        (Some((x0, z0)), Some((x1, z1))) => {
            let d = ((x1 - x0).powi(2) + (z1 - z0).powi(2)).sqrt();
            if d > 0.5 {
                (true, format!("holding W moved the player {d:.2} m"))
            } else {
                (false, format!("holding W for 1.4 s moved the player only {d:.2} m"))
            }
        }
        _ => (false, "the engine stopped answering state queries".into()),
    };

    // Gate 5: the greedy bot reaches WIN through the ordinary input path.
    session.evaluate("window.__arcade.reset(); window.__arcade.setBot('win'); 0").ok();
    let win_budget = Duration::from_secs(25 + target as u64 * 5);
    let win = wait_for_state(&mut session, win_budget, &|s| {
        s.get("status").and_then(|v| v.as_str()) == Some("win")
    });
    let (win_ok, win_detail) = match win {
        Some(state) => {
            let c = state.get("collected").and_then(|v| v.as_u64()).unwrap_or(0);
            (true, format!("the win bot collected all {c} items"))
        }
        None => {
            let last = session.evaluate("JSON.stringify(window.__arcade.state())").unwrap_or(Value::Null);
            let c = state_str(&last, "collected").unwrap_or_default();
            (false, format!("the win bot never won (last state: collected {c} of {target})"))
        }
    };

    // Gate 6: the suicidal bot reaches LOSE.
    session.evaluate("window.__arcade.reset(); window.__arcade.setBot('lose'); 0").ok();
    let lose = wait_for_state(&mut session, Duration::from_secs(60), &|s| {
        s.get("status").and_then(|v| v.as_str()) == Some("lose")
    });
    let (lose_ok, lose_detail) = match lose {
        Some(state) => {
            let lives = state.get("lives").and_then(|v| v.as_i64()).unwrap_or(-1);
            (true, format!("the lose bot burned every life (lives now {lives})"))
        }
        None => {
            let last = session.evaluate("JSON.stringify(window.__arcade.state())").unwrap_or(Value::Null);
            (false, format!("the lose bot never lost (last state: {last})"))
        }
    };

    // Gate 7: frame budget, read after the bots have exercised everything.
    session.evaluate("window.__arcade.setBot(null); window.__arcade.reset(); 0").ok();
    std::thread::sleep(Duration::from_millis(2500));
    let final_state = session.evaluate("JSON.stringify(window.__arcade.state())").unwrap_or(Value::Null);
    let frame_ms = state_num(&final_state, "frameMs").unwrap_or(f64::MAX);
    let budget_ok = frame_ms <= FRAME_BUDGET_MS && frame_ms < f64::MAX;
    let budget_detail = if frame_ms == f64::MAX {
        "the engine stopped reporting frame times".to_string()
    } else {
        format!("average frame {frame_ms:.1} ms under software rendering, budget {FRAME_BUDGET_MS:.0} ms")
    };

    // Gate 2 is judged last so it covers the whole session: CDP events plus the
    // engine's own window.onerror collection.
    page_events.extend(session.drain_events());
    let mut errors = event_errors(&page_events);
    if let Some(arr) = state_num_or_array(&final_state, "errors") {
        errors.extend(arr);
    }
    let engine_errors = engine_error_list(&mut session);
    errors.extend(engine_errors);
    errors.dedup();
    let (console_ok, console_detail) = if errors.is_empty() {
        (true, "the console stayed clean for the whole session".to_string())
    } else {
        let shown: Vec<&str> = errors.iter().map(|s| s.as_str()).take(4).collect();
        (false, format!("{} error(s): {}", errors.len(), shown.join(" | ")))
    };

    // Assemble in the charter's order regardless of execution order.
    let executed: Vec<(&str, bool, String)> = vec![
        ("boots", boot_ok, boot_detail),
        ("no_console_errors", console_ok, console_detail),
        ("frame_renders", shot_ok, shot_detail),
        ("input_moves_player", input_ok, input_detail),
        ("bot_reaches_win", win_ok, win_detail),
        ("bot_reaches_lose", lose_ok, lose_detail),
        ("frame_budget", budget_ok, budget_detail),
    ];
    for (name, ok, detail) in executed {
        report.gate(name, ok, detail);
    }
    report.gates.sort_by_key(|g| ALL_GATES.iter().position(|n| *n == g.gate).unwrap_or(99));

    Ok(report.finish())
}

fn engine_error_list(session: &mut Session) -> Vec<String> {
    match session.evaluate("window.__arcade ? JSON.stringify(window.__arcade.state().errors) : '[]'") {
        Ok(v) => {
            if let Some(text) = v.as_str().map(String::from).or_else(|| Some(v.to_string())) {
                if let Ok(list) = serde_json::from_str::<Vec<String>>(&text) {
                    return list.into_iter().map(|e| format!("window.onerror: {e}")).collect();
                }
            }
            Vec::new()
        }
        Err(_) => Vec::new(),
    }
}

/// Poll the engine's state until the predicate holds. The hook returns an object;
/// asking for its JSON keeps the websocket traffic to plain strings.
fn wait_for_state(session: &mut Session, budget: Duration, pred: &dyn Fn(&Value) -> bool) -> Option<Value> {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if let Ok(v) = session.evaluate("window.__arcade ? JSON.stringify(window.__arcade.state()) : 'null'") {
            if let Some(text) = v.as_str() {
                if let Ok(state) = serde_json::from_str::<Value>(text) {
                    if pred(&state) {
                        return Some(state);
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    None
}

/// The evaluate results come back as JSON text; pull a field back out.
fn parse_state(v: &Option<Value>) -> Option<Value> {
    let text = v.as_ref()?.as_str()?;
    serde_json::from_str::<Value>(text).ok()
}

fn pos_of(v: &Option<Value>) -> Option<(f64, f64)> {
    let state = parse_state(v)?;
    Some((state.get("x")?.as_f64()?, state.get("z")?.as_f64()?))
}

fn state_num(v: &Value, field: &str) -> Option<f64> {
    let state = serde_json::from_str::<Value>(v.as_str()?).ok()?;
    state.get(field)?.as_f64()
}

fn state_str(v: &Value, field: &str) -> Option<String> {
    let state = serde_json::from_str::<Value>(v.as_str()?).ok()?;
    Some(state.get(field)?.to_string())
}

fn state_num_or_array(v: &Value, field: &str) -> Option<Vec<String>> {
    let state = serde_json::from_str::<Value>(v.as_str()?).ok()?;
    let arr = state.get(field)?.as_array()?;
    Some(arr.iter().map(|e| format!("window.onerror: {}", e.as_str().unwrap_or("?"))).collect())
}

/// A real key event: keyDown with the same code/key/virtual-key numbers a
/// keyboard would produce, a hold, then keyUp.
fn dispatch_key(session: &mut Session, code: &str, key: &str, vk: u32) -> Result<(), String> {
    session.call(
        "Input.dispatchKeyEvent",
        json!({ "type": "keyDown", "code": code, "key": key, "windowsVirtualKeyCode": vk, "nativeVirtualKeyCode": vk }),
    )?;
    std::thread::sleep(Duration::from_millis(120));
    session.call(
        "Input.dispatchKeyEvent",
        json!({ "type": "keyUp", "code": code, "key": key, "windowsVirtualKeyCode": vk, "nativeVirtualKeyCode": vk }),
    )?;
    // The engine listens on window, and a held key repeats: hold it by re-sending
    // keyDown a few times so one dispatch is not one 16 ms nudge.
    for _ in 0..8 {
        std::thread::sleep(Duration::from_millis(120));
        session.call(
            "Input.dispatchKeyEvent",
            json!({ "type": "keyDown", "code": code, "key": key, "windowsVirtualKeyCode": vk, "nativeVirtualKeyCode": vk }),
        )?;
    }
    session.call(
        "Input.dispatchKeyEvent",
        json!({ "type": "keyUp", "code": code, "key": key, "windowsVirtualKeyCode": vk, "nativeVirtualKeyCode": vk }),
    )?;
    Ok(())
}

fn capture_screenshot(session: &mut Session) -> Result<Vec<u8>, String> {
    let result = session.call("Page.captureScreenshot", json!({ "format": "png" }))?;
    let data = result
        .get("data")
        .and_then(|d| d.as_str())
        .ok_or_else(|| "the screenshot answer carried no data".to_string())?;
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|e| format!("the screenshot is not valid base64: {e}"))
}

// ── The non-blank check, pure ──────────────────────────────────────

/// Count distinct colours in a PNG. A WebGL context that failed, a canvas that
/// never drew, or a black-screen crash all come back as one or two colours; a
/// rendered arena comes back with sky, ground, walls, creature, items and shadow
/// at minimum. Eight is a floor anything real clears with room to spare.
pub fn png_distinct_colours(bytes: &[u8]) -> Result<usize, String> {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info().map_err(|e| format!("{e}"))?;
    // png 0.18 answers None when the frame size is not knowable up front (it is
    // for every screenshot we get); the fallback is a generous RGBA bound so
    // next_frame still has room and the count degrades to "unreadable", not panic.
    let upper_bound = {
        let info = reader.info();
        info.width as usize * info.height as usize * 4 + info.height as usize * 8
    };
    let mut buf = vec![0u8; reader.output_buffer_size().unwrap_or(upper_bound)];
    let info = reader.next_frame(&mut buf).map_err(|e| format!("{e}"))?;
    let channels = info.color_type.samples();
    let mut seen = std::collections::HashSet::new();
    // Sample every fourth pixel: a 1280x720 frame has nearly a million, and
    // distinct-colour counting does not need all of them.
    let mut px = 0;
    while px + channels <= buf.len() {
        seen.insert([buf[px], buf[px + 1], buf[px + 2]]);
        if seen.len() > 64 {
            break; // clearly not blank; stop counting
        }
        px += channels * 4;
    }
    Ok(seen.len())
}

// ── The whole job, start to finish ─────────────────────────────────

/// Verify a built game: find a browser, launch it headless on the file, run the
/// gates, and leave a screenshot beside the game if asked.
pub fn verify_game(html: &Path, screenshot_out: Option<&Path>) -> Result<Report, String> {
    let headless = launch_headless(html)?;
    verify_session(&headless.ws_url, &headless.browser, screenshot_out)
}

/// Just take the screenshot — the `screenshot` action and the CLI's one-file mode.
pub fn screenshot_game(html: &Path, png_out: &Path) -> Result<(), String> {
    let headless = launch_headless(html)?;
    let mut session = Session::connect(&headless.ws_url)?;
    session.call("Runtime.enable", json!({}))?;
    // Give the engine a moment to render something worth photographing.
    wait_for_state(&mut session, Duration::from_secs(15), &|s| {
        s.get("frames").and_then(|v| v.as_f64()).unwrap_or(0.0) > 5.0
    });
    let bytes = capture_screenshot(&mut session)?;
    if let Some(parent) = png_out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    std::fs::write(png_out, &bytes).map_err(|e| format!("cannot write {}: {e}", png_out.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    // ── Pure pieces ───────────────────────────────────────────────

    fn solid_png(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, w, h);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            // write_image_data wants the whole frame in one call, rows back to back.
            let data = rgb.repeat((w * h) as usize);
            writer.write_image_data(&data).unwrap();
        }
        out
    }

    /// An image where every pixel is its own colour — the stand-in for a rendered
    /// frame, which the distinct-colour gate must wave through.
    fn gradient_png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, w, h);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            let mut data = Vec::with_capacity((w * h * 3) as usize);
            for i in 0..(w * h) {
                data.push((i * 7 % 251) as u8);
                data.push((i * 13 % 251) as u8);
                data.push((i * 29 % 251) as u8);
            }
            writer.write_image_data(&data).unwrap();
        }
        out
    }

    #[test]
    fn a_flat_frame_is_blank() {
        let png_bytes = solid_png(64, 64, [12, 12, 12]);
        assert_eq!(png_distinct_colours(&png_bytes).unwrap(), 1);
    }

    #[test]
    fn a_rendered_frame_is_not_blank() {
        // The counter samples every fourth pixel: 32 pixels, 8 samples, 8 colours.
        let png_bytes = gradient_png(8, 4);
        assert_eq!(png_distinct_colours(&png_bytes).unwrap(), 8);
    }

    #[test]
    fn junk_is_refused_as_a_sentence() {
        let err = png_distinct_colours(b"not a png at all").unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn event_errors_pick_out_console_exceptions_and_logs() {
        let events = vec![
            json!({"method": "Runtime.consoleAPICalled", "params": {"type": "error", "args": [{"value": "THREE says no"}]}}),
            json!({"method": "Runtime.consoleAPICalled", "params": {"type": "log", "args": [{"value": "harmless"}]}}),
            json!({"method": "Runtime.exceptionThrown", "params": {"exceptionDetails": {"text": "Uncaught", "exception": {"description": "TypeError: x is not a function"}}}}),
            json!({"method": "Log.entryAdded", "params": {"entry": {"level": "error", "text": "GL failure"}}}),
            json!({"method": "Log.entryAdded", "params": {"entry": {"level": "info", "text": "fine"}}}),
        ];
        let errors = event_errors(&events);
        assert_eq!(errors.len(), 3);
        assert!(errors[0].contains("THREE says no"));
        assert!(errors[1].contains("TypeError"));
        assert!(errors[2].contains("GL failure"));
    }

    #[test]
    fn slug_of_a_report_lists_every_gate_in_order() {
        let mut report = Report::new("test".into());
        report.gate("boots", true, "up");
        report.abandon(&ALL_GATES[1..], "the game did not boot");
        let report = report.finish();
        assert!(!report.passed);
        let names: Vec<&str> = report.gates.iter().map(|g| g.gate.as_str()).collect();
        assert_eq!(names, ALL_GATES);
        assert!(report.gates[1].detail.contains("did not boot"));
    }

    // ── The fake CDP server ───────────────────────────────────────
    // A whole verification run, browser-free: the test server answers the same
    // methods Chrome would, and walks the engine state through boots → input
    // move → win → lose on a script. If the gate sequence, the polling, the
    // event collection or the report shape breaks, this fails.

    struct Fake {
        port: u16,
        stop: Arc<AtomicBool>,
    }

    fn state_json(status: &str, x: f64, z: f64, collected: u32, lives: u32, frames: u32, frame_ms: f64) -> Value {
        json!({
            "status": status, "collected": collected, "target": 5, "lives": lives,
            "x": x, "z": z, "frameMs": frame_ms, "frames": frames, "bot": null, "errors": []
        })
    }

    impl Fake {
        /// `plant_error` decides whether the fake pushes one console.error event
        /// mid-session, so both verdicts of the console gate are covered.
        fn start(plant_error: bool) -> Fake {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let stop = Arc::new(AtomicBool::new(false));
            let stop2 = stop.clone();
            std::thread::spawn(move || {
                let (stream, _) = match listener.accept() {
                    Ok(s) => s,
                    Err(_) => return,
                };
                stream.set_read_timeout(Some(Duration::from_millis(300))).ok();
                let mut ws = match tungstenite::accept(stream) {
                    Ok(ws) => ws,
                    Err(_) => return,
                };
                // Scripted engine: the fake advances one phase per interesting call.
                let mut phase = 0u32; // 0 boot, 1 input-before, 2 input-after, 3 win, 4 lose, 5+ final
                let mut planted = false;
                loop {
                    if stop2.load(Ordering::SeqCst) {
                        return;
                    }
                    let msg = match ws.read() {
                        Ok(m) => m,
                        Err(tungstenite::Error::Io(ref e))
                            if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) =>
                        {
                            continue
                        }
                        Err(_) => return,
                    };
                    let text = match msg {
                        Message::Text(t) => t,
                        Message::Close(_) => return,
                        _ => continue,
                    };
                    let req: Value = match serde_json::from_str(&text) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    let id = req.get("id").cloned().unwrap_or(Value::Null);
                    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
                    let result: Value = match method {
                        "Runtime.enable" | "Log.enable" | "Page.enable" => json!({}),
                        "Page.captureScreenshot" => {
                            let b64 = base64::engine::general_purpose::STANDARD.encode(gradient_png(16, 16));
                            json!({ "data": b64 })
                        }
                        "Input.dispatchKeyEvent" => {
                            if phase < 2 {
                                phase = 2; // a key arrived: the player is "moving"
                            }
                            json!({})
                        }
                        "Runtime.evaluate" => {
                            let expr = req.pointer("/params/expression").and_then(|e| e.as_str()).unwrap_or("");
                            if expr.contains("setBot('win')") {
                                phase = phase.max(3);
                                json!({ "result": { "type": "number", "value": 0 } })
                            } else if expr.contains("setBot('lose')") {
                                phase = phase.max(4);
                                json!({ "result": { "type": "number", "value": 0 } })
                            } else if expr.contains("setBot(null)") {
                                phase = phase.max(5);
                                json!({ "result": { "type": "number", "value": 0 } })
                            } else if expr.contains("__arcade.state()") {
                                // Advance: boot completes on the first poll; the
                                // input gate sees movement only after a real key
                                // dispatch moved the fake to phase 2.
                                if phase == 0 {
                                    phase = 1;
                                }
                                let s = match phase {
                                    1 => state_json("playing", 0.0, 0.0, 0, 3, 40, 16.0),
                                    2 => state_json("playing", 0.0, -1.8, 0, 3, 90, 16.0),
                                    3 => state_json("win", 2.0, -3.0, 5, 3, 400, 17.0),
                                    4 => state_json("lose", 1.0, 1.0, 2, 0, 700, 18.0),
                                    _ => state_json("playing", 0.0, 0.0, 0, 3, 900, 16.5),
                                };
                                json!({ "result": { "type": "string", "value": s.to_string() } })
                            } else {
                                json!({ "result": { "type": "string", "value": "null" } })
                            }
                        }
                        _ => json!({}),
                    };
                    let answer = json!({ "id": id, "result": result });
                    if ws.send(Message::Text(answer.to_string())).is_err() {
                        return;
                    }
                    // One console error in the middle of the session must sink the
                    // console gate: push it unsolicited once the run is under way.
                    if plant_error && !planted && phase >= 3 {
                        planted = true;
                        let event = json!({
                            "method": "Runtime.consoleAPICalled",
                            "params": { "type": "error", "args": [{ "value": "planted failure" }] }
                        });
                        if ws.send(Message::Text(event.to_string())).is_err() {
                            return;
                        }
                    }
                }
            });
            Fake { port, stop }
        }

        fn ws_url(&self) -> String {
            format!("ws://127.0.0.1:{}/devtools/page/FAKE", self.port)
        }
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn a_clean_session_passes_every_gate() {
        let fake = Fake::start(false);
        let shot_dir = std::env::temp_dir().join(format!("arcade-fake-shot-clean-{}", std::process::id()));
        let shot = shot_dir.join("screenshot.png");
        let report = verify_session(&fake.ws_url(), "fake-chrome", Some(&shot)).unwrap();

        let names: Vec<&str> = report.gates.iter().map(|g| g.gate.as_str()).collect();
        assert_eq!(names, ALL_GATES, "the report must list the charter's gates in the charter's order");
        for g in &report.gates {
            assert!(g.passed, "gate {} should pass against a clean fake: {}", g.gate, g.detail);
        }
        assert!(report.passed);
        assert_eq!(report.browser, "fake-chrome");

        // The screenshot was written where the caller asked, and it decodes.
        assert!(shot.exists());
        assert_eq!(report.screenshot.as_deref(), Some("screenshot.png"));
        assert!(png_distinct_colours(&std::fs::read(&shot).unwrap()).unwrap() >= 4);
        let _ = std::fs::remove_dir_all(&shot_dir);
    }

    #[test]
    fn a_planted_console_error_fails_exactly_the_console_gate() {
        let fake = Fake::start(true);
        let report = verify_session(&fake.ws_url(), "fake-chrome", None).unwrap();
        assert_eq!(report.screenshot, None, "no path asked for, no screenshot recorded");
        for g in &report.gates {
            if g.gate == "no_console_errors" {
                assert!(!g.passed, "the planted error must sink the console gate");
                assert!(g.detail.contains("planted failure"), "{}", g.detail);
            } else {
                assert!(g.passed, "gate {} should be untouched by the planted error: {}", g.gate, g.detail);
            }
        }
        assert!(!report.passed, "one failed gate fails the run");
    }
}
