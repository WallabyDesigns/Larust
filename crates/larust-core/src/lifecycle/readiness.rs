/// Writes the handoff-readiness marker to **stderr**, flushed immediately.
/// Called from `Application::serve()` right before it starts actually
/// accepting connections, but only when this process was started as a
/// handoff replacement (`lifecycle::listener::INHERIT_LISTENER_ENV` set)
/// — nothing reads this line on an ordinary `cargo run`/`xr dev` boot, and
/// printing it unconditionally would just be a confusing, meaningless
/// extra line in a developer's terminal. See `handoff.rs` for the
/// parent-side half of this same protocol.
///
/// Deliberately stderr, not stdout: the parent only pipes whichever stream
/// carries this marker, and drops its read end once found. Routine app
/// logging (`tracing_subscriber`'s default writer) goes to stdout — if
/// *that* were the piped stream, every log line emitted after the
/// handshake completes would hit a reader-less pipe (`EPIPE`, surfaced by
/// `tracing-subscriber` as "the pipe is being closed"). Stderr is only
/// ever used for this one handshake line in the ordinary case, so parking
/// the piping there instead leaves stdout free to inherit the real
/// console for the replacement's entire working life.
pub(crate) fn announce_ready() {
    use std::io::Write;
    eprintln!("{}", super::handoff::READY_MARKER);
    let _ = std::io::stderr().flush();
}
