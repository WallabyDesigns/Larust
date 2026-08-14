/// Writes the handoff-readiness marker to stdout, flushed immediately.
/// Called from `Application::serve()` right before it starts actually
/// accepting connections, but only when this process was started as a
/// handoff replacement (`lifecycle::listener::INHERIT_LISTENER_ENV` set)
/// — nothing reads this line on an ordinary `cargo run`/`xr dev` boot, and
/// printing it unconditionally would just be a confusing, meaningless
/// extra line in a developer's terminal. See `handoff.rs` for the
/// parent-side half of this same protocol.
pub(crate) fn announce_ready() {
    use std::io::Write;
    println!("{}", super::handoff::READY_MARKER);
    let _ = std::io::stdout().flush();
}
