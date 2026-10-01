//! Open a web page in the person's browser: one place, so every caller is held to the same rule.

use std::sync::OnceLock;
use std::time::Duration;

/// Open `url` with the desktop's opener, and bring the browser to the front. Only `https://` and
/// `http://` addresses: a `file:` or custom scheme handed to the opener could start anything
/// registered for it.
///
/// A browser that is already running takes the page as a new tab in its window, and does not
/// come forward itself: on a desktop where Settings fills the screen, the page opened behind it
/// and a press on "Start" looked like nothing (seen on VM 520, 1 Oct 2026). So once the opener has
/// had a moment, the browser's window is presented, and the opener is then waited for, so no
/// press leaves a zombie behind.
pub fn open(url: &str) -> Result<(), String> {
    let url = url.trim();
    if !(url.starts_with("https://") || url.starts_with("http://")) || url.contains(char::is_whitespace) {
        return Err("only a web address is opened".to_string());
    }
    let mut child = std::process::Command::new("xdg-open")
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("the browser could not be opened: {e}"))?;
    let _ = std::thread::Builder::new().name("open-url".into()).spawn(move || {
        // A browser that was not running starts with its own window in front; one that was takes
        // the page as a tab and stays where it was. Either way, ask for it to the front.
        std::thread::sleep(Duration::from_millis(900));
        let browser = default_browser();
        if !crate::windows::present_app(browser) {
            tracing::debug!(%browser, "the browser's window could not be brought forward");
        }
        // xdg-open may stay until a browser it started exits; waiting here costs a thread, not a
        // zombie.
        let _ = child.wait();
    });
    Ok(())
}

/// The app id of the person's default browser (`xdg-settings`), `chromium` when it cannot say.
fn default_browser() -> &'static str {
    static BROWSER: OnceLock<String> = OnceLock::new();
    BROWSER.get_or_init(|| {
        std::process::Command::new("xdg-settings")
            .args(["get", "default-web-browser"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().trim_end_matches(".desktop").to_string())
            .filter(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)))
            .unwrap_or_else(|| "chromium".to_string())
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_a_web_address_is_handed_to_the_opener() {
        for refused in ["file:///etc/passwd", "javascript:alert(1)", "ssh://host", "https://a b", "", "-https://x"] {
            assert!(super::open(refused).is_err(), "{refused}");
        }
    }
}
