//! The head of a proxy request: where the caller wants to go.
//!
//! Two forms are understood. `CONNECT host:port HTTP/1.1` opens a tunnel, and is how every HTTPS
//! request arrives — the proxy sees the name and the port, never what goes through. An
//! absolute-form request (`GET http://host:port/path HTTP/1.1`) is plain HTTP, forwarded only
//! where a rule says so (a LAN model server). Anything else is refused.
//!
//! The head is read whole before anything is decided, with a size limit, so a caller cannot hold
//! a half-read request open or send one that is larger than a head.

/// The largest request head read.
pub const MOST_HEAD: usize = 16 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum Target {
    /// A tunnel to `host:port`.
    Connect { host: String, port: u16 },
    /// A plain HTTP request; `head` is the whole head rewritten for the origin: the path alone on
    /// the request line, the proxy's own headers dropped, and `Connection: close`, so one tunnel
    /// carries one request to the host that was decided on.
    Http { host: String, port: u16, head: Vec<u8> },
}

/// Why a head was not a request this proxy serves.
#[derive(Debug, PartialEq, Eq)]
pub struct Bad(pub &'static str);

/// Where the head ends (`\r\n\r\n`), if it has.
pub fn head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

/// Read what `head` asks for.
pub fn parse(head: &[u8]) -> Result<Target, Bad> {
    let text = std::str::from_utf8(head).map_err(|_| Bad("the request is not text"))?;
    let mut lines = text.split("\r\n");
    let first = lines.next().unwrap_or_default();
    let mut parts = first.split(' ');
    let (method, target, version) = (parts.next(), parts.next(), parts.next());
    let (Some(method), Some(target), Some(version)) = (method, target, version) else {
        return Err(Bad("the request line is not METHOD TARGET VERSION"));
    };
    if parts.next().is_some() || !version.starts_with("HTTP/1.") {
        return Err(Bad("the request line is not METHOD TARGET VERSION"));
    }
    if method == "CONNECT" {
        let (host, port) = host_port(target, None)?;
        return Ok(Target::Connect { host, port });
    }
    if !method.bytes().all(|b| b.is_ascii_uppercase()) || method.is_empty() {
        return Err(Bad("not a method"));
    }
    let rest = target.strip_prefix("http://").ok_or(Bad("only CONNECT, or plain http:// in absolute form"))?;
    let (authority, path) = match rest.find(['/', '?']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let path = if path.starts_with('?') { format!("/{path}") } else { path.to_string() };
    let (host, port) = host_port(authority, Some(80))?;
    let mut out = format!("{method} {path} {version}\r\n");
    for line in lines.take_while(|l| !l.is_empty()) {
        let name = line.split(':').next().unwrap_or_default().trim().to_ascii_lowercase();
        if name.starts_with("proxy-") || name == "connection" || name == "keep-alive" {
            continue;
        }
        out.push_str(line);
        out.push_str("\r\n");
    }
    out.push_str("Connection: close\r\n\r\n");
    Ok(Target::Http { host, port, head: out.into_bytes() })
}

/// `host:port`, `[v6]:port`, or `host` with a default port. The host is lowercased, and must be a
/// name or an address — no user part, no path, nothing a resolver would read as more.
fn host_port(s: &str, default: Option<u16>) -> Result<(String, u16), Bad> {
    if s.contains('@') || s.is_empty() {
        return Err(Bad("not host:port"));
    }
    let (host, port) = if let Some(rest) = s.strip_prefix('[') {
        let (h, after) = rest.split_once(']').ok_or(Bad("an unclosed [address]"))?;
        let port = match after.strip_prefix(':') {
            Some(p) => Some(p),
            None if after.is_empty() => None,
            None => return Err(Bad("not host:port")),
        };
        (h, port)
    } else {
        match s.rsplit_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (s, None),
        }
    };
    let port = match port {
        Some(p) => p.parse::<u16>().ok().filter(|p| *p != 0).ok_or(Bad("not a port"))?,
        None => default.ok_or(Bad("a tunnel needs a port"))?,
    };
    let host = host.to_ascii_lowercase();
    let ok = !host.is_empty()
        && host.len() <= 253
        && host.bytes().all(|b| b.is_ascii_alphanumeric() || b"-.:_".contains(&b))
        && !host.starts_with('.')
        && !host.starts_with('-');
    if !ok {
        return Err(Bad("not a host name or address"));
    }
    Ok((host.trim_end_matches('.').to_string(), port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tunnel_names_its_host_and_port() {
        let t = parse(b"CONNECT API.Example.com:443 HTTP/1.1\r\nHost: api.example.com:443\r\n\r\n").unwrap();
        assert_eq!(t, Target::Connect { host: "api.example.com".into(), port: 443 });
        let t = parse(b"CONNECT [2001:db8::1]:8443 HTTP/1.1\r\n\r\n").unwrap();
        assert_eq!(t, Target::Connect { host: "2001:db8::1".into(), port: 8443 });
        assert!(parse(b"CONNECT api.example.com HTTP/1.1\r\n\r\n").is_err(), "a port is required");
    }

    #[test]
    fn plain_http_is_rewritten_for_the_origin_one_request_per_tunnel() {
        let t = parse(
            b"POST http://192.168.4.20:11434/api/chat?x=1 HTTP/1.1\r\nHost: 192.168.4.20:11434\r\nProxy-Authorization: Basic xyz\r\nConnection: keep-alive\r\nContent-Length: 2\r\n\r\n",
        )
        .unwrap();
        let Target::Http { host, port, head } = t else { panic!() };
        assert_eq!((host.as_str(), port), ("192.168.4.20", 11434));
        let head = String::from_utf8(head).unwrap();
        assert!(head.starts_with("POST /api/chat?x=1 HTTP/1.1\r\n"), "{head}");
        assert!(!head.to_ascii_lowercase().contains("proxy-authorization"));
        assert!(!head.contains("keep-alive"));
        assert!(head.contains("Content-Length: 2\r\n"));
        assert!(head.ends_with("Connection: close\r\n\r\n"));
        let Target::Http { port, .. } = parse(b"GET http://example.com HTTP/1.1\r\n\r\n").unwrap() else { panic!() };
        assert_eq!(port, 80);
    }

    #[test]
    fn anything_else_is_refused() {
        for bad in [
            &b"GET /path HTTP/1.1\r\n\r\n"[..],
            b"GET https://example.com/ HTTP/1.1\r\n\r\n",
            b"CONNECT user@example.com:443 HTTP/1.1\r\n\r\n",
            b"CONNECT example.com:0 HTTP/1.1\r\n\r\n",
            b"CONNECT example.com:99999 HTTP/1.1\r\n\r\n",
            b"CONNECT exa mple.com:443 HTTP/1.1\r\n\r\n",
            b"CONNECT example.com/x:443 HTTP/1.1\r\n\r\n",
            b"CONNECT example.com:443 SPDY/3\r\n\r\n",
            b"\xff\xfe\r\n\r\n",
        ] {
            assert!(parse(bad).is_err(), "{:?}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn the_head_ends_at_the_blank_line() {
        assert_eq!(head_end(b"CONNECT a:1 HTTP/1.1\r\n\r\nrest"), Some(24));
        assert_eq!(head_end(b"CONNECT a:1 HTTP/1.1\r\n"), None);
    }
}
