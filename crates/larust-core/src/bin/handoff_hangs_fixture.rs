//! Deliberately broken replacement for `tests/handoff.rs`'s failure-path
//! test: starts up and stays alive, but never announces readiness - a
//! process that hung during its own startup (deadlocked, stuck on a slow
//! dependency) rather than crashing outright. Proves
//! `handoff::spawn_replacement_and_wait_for_ready` actually times out
//! instead of waiting forever.

use std::io::BufRead;

fn main() {
    // Drains the encoded-listener line the parent writes, same as a real
    // replacement would read it - doesn't functionally matter for this
    // fixture (well under any pipe buffer size either way), just mirrors
    // real behavior up to the point it deliberately stops short.
    let mut discard = String::new();
    let _ = std::io::stdin().lock().read_line(&mut discard);

    std::thread::sleep(std::time::Duration::from_secs(3600));
}
