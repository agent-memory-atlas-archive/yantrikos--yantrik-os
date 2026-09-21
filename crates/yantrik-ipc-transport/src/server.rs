//! JSON-RPC server — Unix domain sockets (Linux) or TCP localhost (Windows dev).

use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::protocol::{RpcRequest, RpcResponse, RPC_METHOD_NOT_FOUND, RPC_PARSE_ERROR};

/// Directory holding this session's service sockets.
///
/// Picks the first candidate we can actually create and write, rather than
/// trusting one path. `$XDG_RUNTIME_DIR` is the correct answer on a normal
/// desktop session, but it is routinely unset — or set to a path that does not
/// exist — in containers, WSL without systemd, and bare `ssh` sessions. The
/// previous hardcoded `/run/yantrik` was unwritable for a user-run service, so
/// every service died on bind.
///
/// Order: `$XDG_RUNTIME_DIR/yantrik` → `/run/yantrik` (root/system service) →
/// `/tmp/yantrik-<uid>` (last resort, always writable).
#[cfg(unix)]
pub fn socket_dir() -> std::path::PathBuf {
    use std::path::PathBuf;

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        let dir = dir.trim();
        if !dir.is_empty() {
            candidates.push(PathBuf::from(dir).join("yantrik"));
        }
    }
    candidates.push(PathBuf::from("/run/yantrik"));
    // SAFETY: getuid() is always safe — it cannot fail and touches no memory.
    let uid = unsafe { libc::getuid() };
    let last_resort = PathBuf::from(format!("/tmp/yantrik-{uid}"));
    candidates.push(last_resort.clone());

    // Why each rejection is logged: this function silently falls through to the last candidate,
    // so a failure on the *first* one surfaces later as a bind error naming the *last* one.
    // perception-service died with "cannot create socket directory /tmp/yantrik-0" while holding
    // a perfectly good /run/user/1000/yantrik, and the message sent the diagnosis to the wrong
    // directory entirely. A fallback chain that does not say why it fell back is a chain that
    // lies about where the problem is.
    for dir in &candidates {
        // Only create it if it is not already there.
        //
        // `create_dir_all` looks idempotent and is not, under Landlock. On an existing directory
        // it still issues `mkdir`, and a ruleset without MAKE_DIR denies that with EACCES —
        // before the kernel ever reaches the EEXIST that `std` would have translated into "fine,
        // it exists". So a service that resolves this directory once, applies a ruleset over it,
        // and resolves it again gets a permission error on the directory it just made itself, and
        // falls through to candidates it can create even less. That is exactly how
        // perception-service died pointing at /tmp/yantrik-0.
        if !dir.is_dir() {
            if let Err(e) = std::fs::create_dir_all(dir) {
                tracing::debug!(dir = %dir.display(), error = %e, "socket dir candidate: cannot create");
                continue;
            }
        }
        if let Err(e) = harden(dir) {
            tracing::debug!(dir = %dir.display(), error = %e, "socket dir candidate: cannot harden");
            continue;
        }
        tracing::debug!(dir = %dir.display(), "socket dir chosen");
        return dir.clone();
    }
    tracing::warn!(
        candidates = ?candidates,
        "no socket directory could be prepared; falling back to the last candidate, which will \
         almost certainly fail to bind"
    );
    last_resort
}

/// Restrict a socket directory to its owner.
///
/// Matters most for the `/tmp` fallback: `/tmp` is world-writable, and these
/// sockets expose system-monitor, network and notification control. Without
/// this, any local user could drive them.
#[cfg(unix)]
fn harden(dir: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::metadata(dir)?.permissions();
    // Already private: say so and touch nothing.
    //
    // This early return is what makes `socket_dir` idempotent, and that matters more than it
    // looks. perception-service calls it once to learn where its socket goes, applies a Landlock
    // ruleset over that directory, and then the service SDK calls it again to bind. The second
    // call used to re-issue this chmod — which Landlock denies, because the ruleset grants writes
    // *inside* the directory and not the right to change the directory itself. So the second call
    // failed on a directory the first call had just successfully created, fell through to
    // candidates it could not create either, and reported the last one's error. The service died
    // with "cannot create socket directory /tmp/yantrik-0" while holding a perfectly good
    // /run/user/1000/yantrik it had made moments earlier.
    if perms.mode() & 0o777 == 0o700 {
        return Ok(());
    }
    let mut perms = perms;
    perms.set_mode(0o700);
    std::fs::set_permissions(dir, perms)
}

/// Who opened this connection, as the kernel says it — not as the caller says it.
///
/// Every other fact a service has about its caller arrives inside the request, which means the
/// caller chose it. These three did not: `SO_PEERCRED` is filled in by the kernel at `connect`
/// time from the peer's own process, and nothing the peer writes on the socket can change them.
/// That is the whole reason this exists (issue #43) — an approval card that names whoever is
/// asking was naming a string the asker supplied.
///
/// Best-effort and deliberately optional: a TCP connection on the Windows dev path has no peer
/// process, and a peer that exits between `accept` and the `getsockopt` still leaves a pid that
/// no longer resolves. A service that cannot learn this must still work; none of them may
/// *refuse* on it, because policy belongs to the shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeerCred {
    pub pid: i32,
    pub uid: u32,
    pub gid: u32,
}

/// Trait for service method dispatch. Implement this in each service.
pub trait ServiceHandler: Send + Sync + 'static {
    /// Service identifier (e.g. "weather", "notes").
    fn service_id(&self) -> &str;

    /// Dispatch an RPC method call. Returns the result as JSON value.
    fn handle(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, yantrik_ipc_contracts::email::ServiceError>;

    /// The same dispatch, told who is on the other end of the socket.
    ///
    /// Defaulted so that every existing `ServiceHandler` — fourteen apps and every service —
    /// compiles and behaves exactly as before: the default throws the credentials away and calls
    /// [`ServiceHandler::handle`]. Only a handler that has something honest to do with the
    /// caller's identity overrides it.
    fn handle_from(
        &self,
        method: &str,
        params: serde_json::Value,
        peer: Option<PeerCred>,
    ) -> Result<serde_json::Value, yantrik_ipc_contracts::email::ServiceError> {
        let _ = peer;
        self.handle(method, params)
    }
}

/// JSON-RPC server.
pub struct RpcServer {
    address: String,
}

impl RpcServer {
    /// Create a new server. On Linux, `address` is a Unix socket path.
    /// On Windows, `address` is a TCP address (e.g. "127.0.0.1:9500").
    pub fn new(address: &str) -> Self {
        Self {
            address: address.to_string(),
        }
    }

    /// Default address for a service.
    ///
    /// Prefers the per-user runtime directory. `/run` is root-owned, so a
    /// desktop session running services as the logged-in user cannot create
    /// `/run/yantrik` — every service then failed to bind with a bare ENOENT.
    /// `$XDG_RUNTIME_DIR` is the standard location for exactly this, and is
    /// already per-user, tmpfs-backed, and cleaned up on logout.
    ///
    /// Falls back to `/run/yantrik` for the system-service case (running as
    /// root, no session, no XDG_RUNTIME_DIR).
    ///
    /// Client and server both call this, so they cannot disagree.
    #[cfg(unix)]
    pub fn default_address(service_id: &str) -> String {
        format!("{}/{}.sock", socket_dir().display(), service_id)
    }

    #[cfg(windows)]
    pub fn default_address(service_id: &str) -> String {
        // Map service names to dev ports
        let port = match service_id {
            "weather" => 9501,
            "system-monitor" => 9502,
            "network" => 9503,
            "music" => 9504,
            "email" => 9505,
            "notes" => 9506,
            "calendar" => 9507,
            "notifications" => 9508,
            "companion" => 9509,
            _ => 9500,
        };
        format!("127.0.0.1:{}", port)
    }

    /// Run the server, dispatching requests to the handler.
    pub async fn serve(self, handler: Arc<dyn ServiceHandler>) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            self.serve_unix(handler).await
        }
        #[cfg(windows)]
        {
            self.serve_tcp(handler).await
        }
    }

    #[cfg(unix)]
    async fn serve_unix(self, handler: Arc<dyn ServiceHandler>) -> std::io::Result<()> {
        use tokio::net::UnixListener;
        use std::path::Path;

        let path = Path::new(&self.address);
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        // Do NOT swallow this. When it failed silently (`/run` is root-owned),
        // the real cause — a permission error — surfaced later as a bare
        // ENOENT from bind(), which reads like a missing binary and sent
        // debugging in the wrong direction entirely.
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return Err(std::io::Error::new(
                    e.kind(),
                    format!(
                        "cannot create socket directory {}: {e} \
                         (set XDG_RUNTIME_DIR to a writable per-user path)",
                        parent.display()
                    ),
                ));
            }
        }

        let listener = UnixListener::bind(&self.address).map_err(|e| {
            std::io::Error::new(e.kind(), format!("cannot bind {}: {e}", self.address))
        })?;
        tracing::info!(socket = %self.address, service = handler.service_id(), "RPC server listening (UDS)");

        loop {
            let (stream, _) = listener.accept().await?;
            // Read at accept, not when somebody asks. The peer of these sockets is routinely a
            // short-lived process — `yos` runs one JSON-RPC call and exits — so by the time a
            // handler wants to know who called, the pid may already be gone or, worse, reused.
            // Asking here narrows that window to the connection's own lifetime.
            let peer = stream
                .peer_cred()
                .ok()
                .map(|c| PeerCred { pid: c.pid().unwrap_or(0), uid: c.uid(), gid: c.gid() });
            let handler = handler.clone();
            tokio::spawn(async move {
                let (reader, writer) = stream.into_split();
                handle_connection(BufReader::new(reader), writer, &handler, peer).await;
            });
        }
    }

    #[cfg(windows)]
    async fn serve_tcp(self, handler: Arc<dyn ServiceHandler>) -> std::io::Result<()> {
        use tokio::net::TcpListener;

        let listener = TcpListener::bind(&self.address).await?;
        tracing::info!(addr = %self.address, service = handler.service_id(), "RPC server listening (TCP dev)");

        loop {
            let (stream, _) = listener.accept().await?;
            let handler = handler.clone();
            tokio::spawn(async move {
                let (reader, writer) = stream.into_split();
                // TCP has no peer process to ask about. This path is the Windows dev loop only.
                handle_connection(BufReader::new(reader), writer, &handler, None).await;
            });
        }
    }
}

async fn handle_connection<R, W>(
    reader: BufReader<R>,
    mut writer: W,
    handler: &Arc<dyn ServiceHandler>,
    peer: Option<PeerCred>,
) where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut lines = reader.lines();

    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        let response = match serde_json::from_str::<RpcRequest>(&line) {
            Ok(req) => {
                tracing::debug!(method = %req.method, peer = ?peer, "RPC request");
                dispatch(handler, req, peer)
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to parse RPC request");
                RpcResponse::error(
                    serde_json::Value::Null,
                    RPC_PARSE_ERROR,
                    format!("Parse error: {}", e),
                )
            }
        };

        let mut resp_json = serde_json::to_string(&response).unwrap_or_default();
        resp_json.push('\n');
        if writer.write_all(resp_json.as_bytes()).await.is_err() {
            break;
        }
    }
}

fn dispatch(
    handler: &Arc<dyn ServiceHandler>,
    req: RpcRequest,
    peer: Option<PeerCred>,
) -> RpcResponse {
    match req.method.as_str() {
        "rpc.ping" => {
            return RpcResponse::success(req.id, serde_json::json!("pong"));
        }
        "rpc.service_id" => {
            return RpcResponse::success(req.id, serde_json::json!(handler.service_id()));
        }
        _ => {}
    }

    match handler.handle_from(&req.method, req.params, peer) {
        Ok(result) => RpcResponse::success(req.id, result),
        Err(e) => {
            if e.code == -1 {
                RpcResponse::error(req.id, RPC_METHOD_NOT_FOUND, e.message)
            } else {
                RpcResponse::error(req.id, e.code, e.message)
            }
        }
    }
}

#[cfg(unix)]
impl Drop for RpcServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.address);
    }
}

#[cfg(all(test, unix))]
mod socket_dir_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn hardening_an_already_private_directory_changes_nothing() {
        // The property perception-service depends on: asking twice must be safe. The second ask
        // happens after a Landlock ruleset is in force, and a chmod at that point is denied — so
        // if this is not a no-op the service cannot bind the socket it already made room for.
        let dir = std::env::temp_dir().join(format!("yantrik-harden-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        harden(&dir).expect("first harden");
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);

        // Make it read-only so any *attempted* chmod would be visible as a change, then prove the
        // second call does not attempt one.
        harden(&dir).expect("second harden must be a no-op, not a second chmod");
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_world_readable_directory_is_still_tightened() {
        // The other half: the early return must not make `harden` stop hardening. /tmp is
        // world-writable and these sockets drive system-monitor, network and notifications.
        let dir = std::env::temp_dir().join(format!("yantrik-loose-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut perms = std::fs::metadata(&dir).unwrap().permissions();
        perms.set_mode(0o777);
        std::fs::set_permissions(&dir, perms).unwrap();

        harden(&dir).expect("harden");
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700,
            "a world-writable socket directory must be tightened, not waved through"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn asking_twice_returns_the_same_directory() {
        assert_eq!(socket_dir(), socket_dir());
    }
}
