//! The proxy: one connection, from its head to the tunnel.
//!
//! In order, each step able to end it with a sentence: the head read whole (bounded in size and
//! time); the caller's account read from the kernel and required to be the mind's; the target
//! parsed; the name resolved here — the mind resolves nothing itself — and every address it gave
//! classed, so a name that resolves to this machine goes nowhere; the policy's verdict, counted
//! either way; then the connection, and bytes copied both ways until either end closes.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;

use crate::ledger::Outcome;
use crate::policy::{place_of, Place, Verdict};
use crate::request::{self, Target};
use crate::state::State;

/// How long a caller has to send its head, and a name to resolve, and an upstream to answer.
const HEAD_TIME: Duration = Duration::from_secs(10);
const RESOLVE_TIME: Duration = Duration::from_secs(10);
const CONNECT_TIME: Duration = Duration::from_secs(15);
/// The most connections at once.
pub const MOST_OPEN: usize = 256;

pub struct Proxy {
    pub state: Arc<Mutex<State>>,
    /// The one account served: the mind's.
    pub serve_uid: u32,
    /// Where the proxy listens, for finding the caller's socket.
    pub local: SocketAddr,
}

pub async fn serve(listener: TcpListener, proxy: Arc<Proxy>) {
    let open = Arc::new(Semaphore::new(MOST_OPEN));
    loop {
        let Ok((mut stream, peer)) = listener.accept().await else { continue };
        let Ok(permit) = open.clone().try_acquire_owned() else {
            let _ = reply(&mut stream, 503, "the proxy is at its limit of open connections; try again").await;
            continue;
        };
        let proxy = proxy.clone();
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(why) = one(&proxy, &mut stream, peer).await {
                tracing::debug!(%peer, why, "connection ended");
            }
        });
    }
}

async fn reply(stream: &mut TcpStream, code: u16, why: &str) -> std::io::Result<()> {
    let reason = match code {
        400 => "Bad Request",
        403 => "Forbidden",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let body = format!("{why}\n");
    let head = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nX-Yantrik-Egress: refused\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

async fn one(proxy: &Proxy, stream: &mut TcpStream, peer: SocketAddr) -> Result<(), &'static str> {
    // Who, before anything is read: a caller that is not the mind is told so and nothing more.
    let local = stream.local_addr().unwrap_or(proxy.local);
    let uid = tokio::task::spawn_blocking(move || crate::peer::uid_of(peer, local)).await.ok().flatten();
    if uid != Some(proxy.serve_uid) {
        let _ = reply(stream, 403, "this proxy serves the mind account only").await;
        return Err("not the mind");
    }

    let mut buf = Vec::with_capacity(2048);
    let head_len = tokio::time::timeout(HEAD_TIME, async {
        let mut chunk = [0u8; 4096];
        loop {
            if let Some(end) = request::head_end(&buf) {
                return Ok(end);
            }
            if buf.len() >= request::MOST_HEAD {
                return Err("the head is too long");
            }
            let n = stream.read(&mut chunk).await.map_err(|_| "read failed")?;
            if n == 0 {
                return Err("closed before its head ended");
            }
            buf.extend_from_slice(&chunk[..n]);
        }
    })
    .await
    .map_err(|_| "the head took too long")
    .and_then(|r| r);
    let head_len = match head_len {
        Ok(n) => n,
        Err(why) => {
            let _ = reply(stream, 400, why).await;
            return Err(why);
        }
    };
    let target = match request::parse(&buf[..head_len]) {
        Ok(t) => t,
        Err(request::Bad(why)) => {
            let _ = reply(stream, 400, why).await;
            return Err(why);
        }
    };
    let rest = buf[head_len..].to_vec();
    let (host, port, http) = match &target {
        Target::Connect { host, port } => (host.clone(), *port, false),
        Target::Http { host, port, .. } => (host.clone(), *port, true),
    };

    // Resolved here, and every address classed. The ones that are never a destination are
    // dropped; if nothing is left, the name was this machine (or the like) and goes nowhere.
    let addrs: Vec<SocketAddr> = match tokio::time::timeout(RESOLVE_TIME, tokio::net::lookup_host((host.as_str(), port))).await {
        Ok(Ok(a)) => a.collect(),
        _ => Vec::new(),
    };
    if addrs.is_empty() {
        let _ = reply(stream, 502, &format!("{host} did not resolve")).await;
        return Err("did not resolve");
    }
    let usable: Vec<SocketAddr> = addrs.iter().copied().filter(|a| place_of(a.ip()) != Place::Forbidden).collect();
    let place = if usable.is_empty() {
        Place::Forbidden
    } else if usable.iter().all(|a| place_of(a.ip()) == Place::Lan) {
        Place::Lan
    } else {
        Place::Internet
    };
    // A name that gives both kinds is reached at its internet addresses only, unless the rule
    // allows the local network: the verdict is for the place it is actually reached at.
    let usable: Vec<SocketAddr> = match place {
        Place::Internet => usable.into_iter().filter(|a| place_of(a.ip()) == Place::Internet).collect(),
        _ => usable,
    };

    let verdict = {
        let Ok(mut s) = proxy.state.lock() else { return Err("state poisoned") };
        let v = s.policy.decide(&host, port, http, place, s.private);
        let (outcome, why) = match &v {
            Verdict::Allow { audit: true } => (Outcome::Audited, ""),
            Verdict::Allow { audit: false } => (Outcome::Allowed, ""),
            Verdict::Refuse(why) => (Outcome::Refused, why.as_str()),
        };
        s.ledger.record(&host, port, outcome, place == Place::Lan, http, why, now());
        v
    };
    if let Verdict::Refuse(why) = verdict {
        tracing::info!(host, port, why, "refused");
        let _ = reply(stream, 403, &why).await;
        return Err("refused");
    }

    let mut upstream = None;
    for addr in &usable {
        if let Ok(Ok(s)) = tokio::time::timeout(CONNECT_TIME, TcpStream::connect(addr)).await {
            upstream = Some(s);
            break;
        }
    }
    let Some(mut upstream) = upstream else {
        let _ = reply(stream, 502, &format!("{host}:{port} did not answer")).await;
        return Err("upstream did not answer");
    };

    match target {
        Target::Connect { .. } => {
            stream.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await.map_err(|_| "write failed")?;
        }
        Target::Http { head, .. } => {
            upstream.write_all(&head).await.map_err(|_| "write failed")?;
        }
    }
    if !rest.is_empty() {
        upstream.write_all(&rest).await.map_err(|_| "write failed")?;
    }
    let _ = tokio::io::copy_bidirectional(stream, &mut upstream).await;
    Ok(())
}
