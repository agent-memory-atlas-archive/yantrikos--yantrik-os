//! Playing a game: hand the built file to the desktop's Browser, do not become one.
//!
//! The charter says `play` reuses the browser route, and that is what this does. The
//! shell's Browser launch already carries `--remote-debugging-port=9222`, so Arcade
//! asks the shell to open the Browser like any other caller, waits for the debugging
//! port to answer, and opens the game in a new tab over plain DevTools HTTP. No
//! second browser stack, no window of its own, and the person can see the game in the
//! same Browser they already use — with its tab bar, its zoom, its everything.

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::Value;

const DEBUGGER: &str = "http://127.0.0.1:9222";

/// A `file://` URL for a local path, percent-encoded the way Chrome's DevTools
/// expects it in `/json/new?<url>`.
pub fn file_url(path: &Path) -> String {
    let text = path.display().to_string();
    let mut out = String::from("file://");
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Is a browser already listening on the debugging port?
fn debugger_alive(base: &str) -> bool {
    ureq::get(&format!("{base}/json/version"))
        .timeout(Duration::from_millis(700))
        .call()
        .is_ok()
}

/// Ask the browser to open a URL in a new tab. Newer Chromes insist this is a PUT.
pub fn open_tab(base: &str, url: &str) -> Result<String, String> {
    let response = ureq::put(&format!("{base}/json/new?{url}"))
        .timeout(Duration::from_secs(5))
        .call()
        .map_err(|e| format!("the Browser's debugging port refused the new tab: {e}"))?;
    let body: Value = response
        .into_json()
        .map_err(|e| format!("the Browser answered something that is not JSON: {e}"))?;
    let id = body.get("id").and_then(|v| v.as_str()).unwrap_or("");
    if id.is_empty() {
        return Err("the Browser accepted the tab but named no target for it".into());
    }
    Ok(id.to_string())
}

/// Ask the shell to launch (or focus) the Browser app through its own route.
fn ask_shell_for_browser() -> Result<(), String> {
    let client = yantrik_ipc_transport::SyncRpcClient::for_service(
        &yantrik_app_runtime::control::service_id_for("shell"),
    )
    .with_timeout(Duration::from_secs(3));
    let result = client
        .call(
            "app.act",
            serde_json::json!({ "action": "open_app", "args": { "name": "browser" } }),
        )
        .map_err(|e| {
            format!(
                "the desktop shell did not answer its control surface ({}); is the shell running?",
                e.message
            )
        })?;
    if let Some(summary) = result.get("summary").and_then(|s| s.as_str()) {
        tracing::debug!("shell answered open_app browser: {summary}");
    }
    Ok(())
}

/// Open a built game in the desktop Browser. The whole sequence has one honest
/// refusal per thing that can go wrong, because every one of them is something a
/// person needs to hear: the shell is not running, the browser did not come up,
/// or its debugging port is not the one the route promises.
pub fn play(html: &Path) -> Result<String, String> {
    let abs = std::fs::canonicalize(html)
        .map_err(|e| format!("cannot read {}: {e}", html.display()))?;
    if !debugger_alive(DEBUGGER) {
        ask_shell_for_browser()?;
    }
    // The Browser may already have been up (port alive immediately) or may be
    // starting now; either way, wait for the port to answer.
    let deadline = Instant::now() + Duration::from_secs(12);
    while !debugger_alive(DEBUGGER) {
        if Instant::now() > deadline {
            return Err(
                "the Browser did not open its debugging port (127.0.0.1:9222) within 12 s; \
                 the desktop's Browser route is supposed to carry --remote-debugging-port=9222"
                    .into(),
            );
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let url = file_url(&abs);
    open_tab(DEBUGGER, &url)?;
    Ok(format!(
        "opened {} in the desktop Browser",
        abs.file_name().and_then(|n| n.to_str()).unwrap_or("the game")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    #[test]
    fn file_urls_encode_what_a_url_cannot_carry() {
        let url = file_url(Path::new("/home/me/My Games/pip's run/index.html"));
        assert_eq!(url, "file:///home/me/My%20Games/pip%27s%20run/index.html");
    }

    #[test]
    fn file_urls_leave_safe_characters_alone() {
        let url = file_url(Path::new("/tmp/a-b_c.d~e/f/index.html"));
        assert_eq!(url, "file:///tmp/a-b_c.d~e/f/index.html");
    }

    /// One canned HTTP exchange: enough to prove open_tab sends a PUT to the
    /// right path with the URL in the query, and reads the target id back.
    #[test]
    fn open_tab_puts_a_new_tab_and_reads_the_target_id() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            // Consume the headers so the client sees a well-formed exchange.
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" || line == "\n" {
                    break;
                }
            }
            let body = r#"{"id":"TARGET42","type":"page","url":"file:///x"}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
            request_line
        });
        let id = open_tab(&format!("http://127.0.0.1:{port}"), "file:///tmp/game/index.html").unwrap();
        assert_eq!(id, "TARGET42");
        let request_line = server.join().unwrap();
        assert!(request_line.starts_with("PUT "), "newer Chromes require PUT: {request_line}");
        assert!(request_line.contains("/json/new?file:///tmp/game/index.html"), "{request_line}");
    }

    #[test]
    fn a_dead_debugger_port_is_not_alive() {
        // Port 1 on loopback: nothing listens there.
        assert!(!debugger_alive("http://127.0.0.1:1"));
    }
}
