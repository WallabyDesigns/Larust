//! Deliberately broken replacement for `tests/handoff.rs`'s failure-path
//! test: exits immediately with an error, before ever reading the
//! handed-off listener or announcing readiness — simulates a replacement
//! binary that's simply broken (a bad build, a startup panic).

fn main() {
    std::process::exit(1);
}
