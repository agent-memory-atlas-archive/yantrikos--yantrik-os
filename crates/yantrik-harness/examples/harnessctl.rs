//! `harnessctl` — see which minds this machine has, and put a question to one.
//!
//! The shell will grow a picker and a Settings screen, but a person setting up a harness needs to
//! know whether the YAML they just wrote works *before* any of that exists, and needs to see the
//! reason when it does not. This is that, and it is also how the config path gets exercised
//! without a compositor.
//!
//! ```text
//! harnessctl list                     what is declared, and is it reachable
//! harnessctl ask <id> "<question>"    put a turn to one and stream the answer
//! ```

use std::io::Write;
use std::sync::Arc;

use yantrik_harness::{Chunk, Registry};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dirs = Registry::default_dirs();
    // No built-ins here: the companion lives in the shell process and cannot be reached from a
    // standalone binary. This tool sees exactly what a config file can declare, which is the
    // point of it.
    let registry = Registry::new(Vec::<Arc<dyn yantrik_harness::Harness>>::new(), &dirs);

    match args.first().map(String::as_str) {
        Some("list") | None => {
            println!("harness directories:");
            for dir in &dirs {
                println!("  {}", dir.display());
            }
            println!();

            let rows = registry.list();
            if rows.is_empty() {
                println!("No harnesses declared. Drop a YAML file in one of the directories above:");
                println!();
                println!("  id: mind");
                println!("  name: Yantrik Mind");
                println!("  kind: openai-http");
                println!("  endpoint: http://192.168.4.66:8080/v1");
                println!("  model: qwen2.5");
            } else {
                let health: std::collections::HashMap<String, String> = registry
                    .health()
                    .into_iter()
                    .map(|(id, h)| (id, h.summary()))
                    .collect();
                for row in rows {
                    let name =
                        if row.enabled { row.name } else { format!("{} (disabled)", row.name) };
                    println!("  {:<14} {:<13} {}", row.id, row.kind, name);
                    // On its own line, not a column: a transport error is a sentence long and
                    // wrapping it into a fixed width either truncates the useful half or shunts
                    // every name out of alignment.
                    if let Some(state) = health.get(&row.id) {
                        println!("  {:<14} {:<13} {}", "", "", state);
                    }
                }
            }

            // A file that could not be read is the thing a person most needs told about.
            if !registry.broken().is_empty() {
                println!("\nnot usable:");
                for broken in registry.broken() {
                    println!("  {} — {}", broken.source, broken.reason);
                }
            }
        }

        Some("ask") => {
            let Some(id) = args.get(1) else {
                eprintln!("usage: harnessctl ask <id> \"<question>\"");
                std::process::exit(2);
            };
            let question = args[2..].join(" ");
            if question.trim().is_empty() {
                eprintln!("usage: harnessctl ask <id> \"<question>\"");
                std::process::exit(2);
            }

            let mut registry = registry;
            if let Err(e) = registry.set_active(id) {
                eprintln!("{e}");
                std::process::exit(1);
            }
            let Some(harness) = registry.active() else {
                eprintln!("`{id}` has no adapter");
                std::process::exit(1);
            };

            eprintln!("— {} ({}) —", harness.name(), harness.id());
            let mut failed = false;
            for chunk in harness.send(yantrik_harness::Turn::new(question)) {
                match chunk {
                    Chunk::Text(text) => {
                        print!("{text}");
                        let _ = std::io::stdout().flush();
                    }
                    Chunk::Failed(why) => {
                        eprintln!("\nfailed: {why}");
                        failed = true;
                    }
                }
            }
            println!();
            if failed {
                std::process::exit(1);
            }
        }

        Some(other) => {
            eprintln!("unknown command `{other}`; this tool offers: list, ask");
            std::process::exit(2);
        }
    }
}
