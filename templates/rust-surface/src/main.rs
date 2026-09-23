//! `my-surface`: serve the to-do list on `app-my-surface.sock` until the process is stopped.
//!
//! ```text
//! cargo run -p my-surface
//! yos describe my-surface
//! yos act my-surface add title="Water the plants" priority=high
//! yos check my-surface
//! ```

fn main() {
    let surface = my_surface::surface();

    // Every declaration is one the dispatch can enforce; a mistake here is the author's, so it
    // stops the program rather than publishing an action nobody could call right.
    let problems = surface.registry().problems();
    if !problems.is_empty() {
        for problem in &problems {
            eprintln!("my-surface: {problem}");
        }
        std::process::exit(2);
    }

    eprintln!("my-surface: serving `{}` on {}", surface.app_id(), surface.address());
    if let Err(e) = surface.serve() {
        eprintln!("my-surface: could not serve: {e}");
        std::process::exit(1);
    }
}
