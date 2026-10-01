//! Open a web page in the person's browser: one place, so every caller is held to the same rule.

/// Open `url` with the desktop's opener. Only `https://` and `http://` addresses: a `file:` or
/// custom scheme handed to the opener could start anything registered for it.
pub fn open(url: &str) -> Result<(), String> {
    let url = url.trim();
    if !(url.starts_with("https://") || url.starts_with("http://")) || url.contains(char::is_whitespace) {
        return Err("only a web address is opened".to_string());
    }
    std::process::Command::new("xdg-open")
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("the browser could not be opened: {e}"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_a_web_address_is_handed_to_the_opener() {
        for refused in ["file:///etc/passwd", "javascript:alert(1)", "ssh://host", "https://a b", ""] {
            assert!(super::open(refused).is_err(), "{refused}");
        }
    }
}
