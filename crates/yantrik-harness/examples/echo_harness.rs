//! A complete harness, so the size of the job is not a matter of opinion.
//!
//! It echoes what you say, which is the least interesting thing a mind can do and exactly the
//! point: everything here that is not the echo is the entire cost of attaching to this OS.
//! Replace `answer()` with a call to your own model, agent loop, or whatever `yantrik-mind` and
//! hermes-agent already do, and change nothing else.
//!
//! Note what is absent. No endpoint, no model name, no API key, no registration, no install step
//! and nothing to restart — a harness brings its own of all that, and appears in the picker
//! because it is polling.
//!
//! ```text
//! cargo run --example echo-harness
//! ```

use std::time::Duration;

use yantrik_harness::protocol;
use yantrik_ipc_transport::SyncRpcClient;

fn main() {
    let client = SyncRpcClient::for_service("harness").with_timeout(Duration::from_secs(40));

    // 1. Say who you are. The OS learns everything it knows about you from this.
    let attached = client.call(
        protocol::ATTACH,
        serde_json::json!({
            "id": "echo",
            "name": "Echo",
            "detail": "repeats what you say",
        }),
    );
    let session = match attached {
        Ok(reply) => reply["session"].as_str().unwrap_or_default().to_string(),
        Err(e) => {
            eprintln!("could not attach: {} — is the Yantrik shell running?", e.message);
            std::process::exit(1);
        }
    };
    eprintln!("attached as `echo` (session {session}); ctrl-c to leave");

    // 2. Ask for work, forever.
    loop {
        let turn = match client.call(protocol::POLL, serde_json::json!({ "session": session })) {
            Ok(turn) => turn,
            Err(e) => {
                // The shell restarted, or this session aged out. Attaching again is the whole
                // recovery, which is why the protocol has no reconnect dance.
                eprintln!("poll failed ({}); re-attaching", e.message);
                std::thread::sleep(Duration::from_secs(2));
                match client.call(
                    protocol::ATTACH,
                    serde_json::json!({ "id": "echo", "name": "Echo" }),
                ) {
                    Ok(_) => continue,
                    Err(_) => continue,
                }
            }
        };

        let Some(turn_id) = turn["turn_id"].as_u64() else {
            // Nothing waiting. Ordinary: a person types far less often than this loop runs.
            std::thread::sleep(Duration::from_millis(300));
            continue;
        };
        let text = turn["text"].as_str().unwrap_or_default().to_string();
        eprintln!("turn {turn_id}: {text}");

        // 3. Answer in pieces, as they exist. A real harness sends each token here; the panel
        //    renders them as they land, which is why this is a stream and not a return value.
        for piece in answer(&text) {
            let _ = client.call(
                protocol::CHUNK,
                serde_json::json!({ "session": session, "turn_id": turn_id, "delta": piece }),
            );
            std::thread::sleep(Duration::from_millis(40));
        }

        // 4. Say you are done, or say why you could not be.
        let _ = client.call(
            protocol::COMPLETE,
            serde_json::json!({ "session": session, "turn_id": turn_id }),
        );
    }
}

/// The only part a real harness replaces.
fn answer(text: &str) -> Vec<String> {
    let mut pieces = vec!["You said: ".to_string()];
    pieces.extend(text.split_inclusive(' ').map(str::to_string));
    pieces
}
