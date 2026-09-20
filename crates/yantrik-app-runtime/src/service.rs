//! Reach a service that is only started when something needs it.
//!
//! The machine rail calls calendar, email and notes "on demand", and until now that was a label
//! on a stopped process rather than a mechanism: nothing started them, so every call an app made
//! to one failed at connect. The calendar could not save an appointment on a freshly booted
//! machine, and said it had.
//!
//! The shell owns service lifetimes through its ServiceManager, so this asks the shell to start
//! one rather than spawning a binary behind its back — otherwise the rail would report "on
//! demand" for a process that is running, which is the same class of lie in the other direction.

use std::time::{Duration, Instant};

use yantrik_ipc_transport::{RpcServer, SyncRpcClient};

/// How long to wait for a service to come up before giving up on it. A service is a local
/// process opening a socket; if it has not managed that in this long, something is wrong and
/// the caller should hear so rather than keep hanging.
const START_TIMEOUT: Duration = Duration::from_secs(5);

/// How often to look for the socket while waiting.
const POLL: Duration = Duration::from_millis(100);

/// True when the service's socket accepts a connection right now.
pub fn is_up(service_id: &str) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::net::UnixStream::connect(RpcServer::default_address(service_id)).is_ok()
    }
    #[cfg(not(unix))]
    {
        std::net::TcpStream::connect(RpcServer::default_address(service_id)).is_ok()
    }
}

/// Make sure `service_id` is running, asking the shell to start it if it is not.
///
/// Returns `Ok(())` when the service is reachable. Callers that hold the UI thread should treat
/// the error as the answer to show: it names what could not be reached.
pub fn ensure(service_id: &str) -> Result<(), String> {
    if is_up(service_id) {
        return Ok(());
    }

    SyncRpcClient::for_service("app-shell")
        .with_timeout(START_TIMEOUT)
        .call(
            "app.act",
            serde_json::json!({
                "action": "start_service",
                "args": { "name": service_id },
            }),
        )
        .map_err(|e| {
            format!("could not ask the desktop to start the {service_id} service: {}", e.message)
        })?;

    // The shell answers when it has spawned the process; the socket appears a moment later.
    let deadline = Instant::now() + START_TIMEOUT;
    while Instant::now() < deadline {
        if is_up(service_id) {
            // The address failed a moment ago, when the service was genuinely down, and the
            // breaker is holding that verdict for a few seconds more. Drop it: the socket just
            // answered, and the call this start was for is about to be made.
            SyncRpcClient::clear_breaker(&RpcServer::default_address(service_id));
            return Ok(());
        }
        std::thread::sleep(POLL);
    }
    Err(format!("the {service_id} service did not come up"))
}

/// A client for a service, started first if it was not running.
pub fn client(service_id: &str) -> Result<SyncRpcClient, String> {
    ensure(service_id)?;
    Ok(SyncRpcClient::for_service(service_id))
}
