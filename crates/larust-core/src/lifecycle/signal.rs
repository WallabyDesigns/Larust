/// Resolves once on Ctrl+C, (Unix only) SIGTERM, or (Windows only)
/// `CTRL_BREAK_EVENT` - the ways a running app is normally asked to stop.
///
/// The Windows case needs its own explanation, confirmed empirically (not
/// from documentation alone) via a throwaway spike: `tokio::signal::
/// ctrl_c()` on Windows only ever resolves on a real `CTRL_C_EVENT`. An
/// *external* controlling process (a supervisor, this crate's own future
/// restart-handoff orchestration, or a test harness) cannot reliably send
/// `CTRL_C_EVENT` to a specific target process - `GenerateConsoleCtrlEvent`
/// only accepts `dwProcessGroupId = 0` for `CTRL_C_EVENT`, which broadcasts
/// to *every* process sharing the sender's own console (including the
/// sender itself), not a single chosen target. `CTRL_BREAK_EVENT` is the
/// one console-control event that *can* target one specific process group
/// (the child must be spawned with `CREATE_NEW_PROCESS_GROUP` for this),
/// which is why an external, deliberate "ask this one process to stop" is
/// only reliably deliverable as a break event on this platform - but
/// `ctrl_c()` alone does not listen for it: a `CTRL_BREAK_EVENT` with no
/// application handler for it falls through to the OS's own default
/// handler, which just terminates the process outright
/// (`STATUS_CONTROL_C_EXIT`), skipping graceful shutdown entirely. Proven
/// by first reproducing that exact failure, then fixing it by explicitly
/// also listening on `tokio::signal::windows::ctrl_break()` here - see
/// `docs/GOTCHAS.md`.
pub(crate) async fn wait_for_termination() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    #[cfg(windows)]
    let ctrl_break = async {
        tokio::signal::windows::ctrl_break()
            .expect("failed to install Ctrl+Break handler")
            .recv()
            .await;
    };
    #[cfg(not(windows))]
    let ctrl_break = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
        _ = ctrl_break => {}
    }
}
