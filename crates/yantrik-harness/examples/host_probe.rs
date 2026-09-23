//! Serve the harness socket and prove a real client can attach over it.
//!
//! The host is transport-free by design, which is what makes it testable — but "testable without
//! a socket" has to be paid for by checking, once, that it works *with* one. This is that check:
//! it serves `harness` on the real bus, waits for `echo-harness` to attach, puts a turn to it,
//! and prints what came back.
//!
//! ```text
//! cargo run --example host-probe      # then, in another shell:
//! cargo run --example echo-harness
//! ```

use std::sync::Arc;
use std::time::{Duration, Instant};

use yantrik_harness::{protocol, Chunk, Host, Turn};
use yantrik_ipc_contracts::email::ServiceError;
use yantrik_ipc_transport::server::{RpcServer, ServiceHandler};

struct HarnessService {
    host: Host,
}

impl ServiceHandler for HarnessService {
    fn service_id(&self) -> &str {
        "harness"
    }

    fn handle(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, ServiceError> {
        // -32000 is the JSON-RPC range for application errors; the message is the part a harness
        // author reads, and the host writes those to be read.
        self.host
            .handle(method, &params)
            .map_err(|message| ServiceError { code: -32000, message })
    }
}

fn main() {
    let host = Host::new(vec![]);

    {
        let host = host.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            runtime.block_on(async {
                let address = RpcServer::default_address("harness");
                eprintln!("serving the harness socket at {address}");
                if let Err(e) =
                    RpcServer::new(&address).serve(Arc::new(HarnessService { host })).await
                {
                    eprintln!("the harness socket stopped: {e}");
                }
            });
        });
    }

    eprintln!("waiting for a harness to attach — run `cargo run --example echo-harness`");
    let deadline = Instant::now() + Duration::from_secs(60);
    while host.list().is_empty() {
        if Instant::now() > deadline {
            eprintln!("nothing attached in 60s");
            std::process::exit(1);
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    for entry in host.list() {
        eprintln!(
            "attached: {} ({}){}",
            entry.name,
            entry.id,
            entry.detail.map(|d| format!(" — {d}")).unwrap_or_default()
        );
    }

    let id = host.list()[0].id.clone();
    host.set_active(&id).expect("select the harness that just attached");
    eprintln!("asking `{id}` a question over the socket…\n");

    let mut text = String::new();
    for chunk in host.send(Turn::new("hello from the OS")) {
        match chunk {
            Chunk::Text(part) => {
                text.push_str(&part);
                eprint!("{part}");
            }
            Chunk::Failed(why) => {
                eprintln!("\nfailed: {why}");
                std::process::exit(1);
            }
            // What the harness says it is doing; this probe only checks the answer.
            Chunk::Event(event) => eprintln!("\n[{}]", event.kind()),
        }
    }
    eprintln!("\n");

    // The answer has to have come from the harness, over the socket, or this proves nothing.
    assert!(text.contains("hello from the OS"), "the harness did not answer with the turn");
    eprintln!("PASS: a real client attached over the bus and answered a turn end to end");
    eprintln!("      ({} methods, none of them carrying an endpoint or a key)", protocol::METHODS.len());
}
