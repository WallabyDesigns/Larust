use super::{AdminOutcome, ACK_HANDOFF_FAILED, ACK_HANDOFF_STARTED, RESTART_COMMAND, STOP_COMMAND};
use crate::lifecycle::handoff;
use std::net::TcpListener as StdTcpListener;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::ServerOptions;

fn pipe_name(address: &str) -> String {
    format!(r"\\.\pipe\{address}")
}

/// Creates one pipe instance, retrying on failure — needed specifically
/// for the *first* instance a process creates: during a handoff, the
/// replacement's own admin-channel loop starts up while its predecessor's
/// own pipe instance may still briefly exist (the predecessor only
/// releases it once its own admin task actually returns, which races the
/// replacement's boot rather than strictly preceding it), and
/// `first_pipe_instance(true)` fails outright with `ERROR_ACCESS_DENIED`
/// if any other instance of the name currently exists — confirmed
/// empirically (not assumed from docs) by hitting exactly this race
/// while building this module; see `docs/GOTCHAS.md`. Once one instance
/// is successfully held, subsequent instances (after each connection
/// finishes) never hit this — only this process holds the name by then —
/// so the retry loop is there for the handoff-boundary race specifically,
/// not steady-state operation.
async fn create_pipe_instance(
    name: &str,
    first_instance: bool,
) -> tokio::net::windows::named_pipe::NamedPipeServer {
    let mut last_error = None;
    for attempt in 0..50 {
        match ServerOptions::new()
            .first_pipe_instance(first_instance)
            .create(name)
        {
            Ok(server) => return server,
            Err(source) => {
                last_error = Some(source);
                if attempt < 49 {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }
    panic!("failed to create admin channel pipe {name} after retrying: {last_error:?}");
}

pub(super) async fn run_until_command(
    address: &str,
    listener: &StdTcpListener,
    ready_timeout: Duration,
) -> AdminOutcome {
    let name = pipe_name(address);
    // Every instance after the first is created fresh, one at a time,
    // only once the previous connection has fully finished and been
    // dropped — creating a second instance *while* the first is still in
    // use also hits `ERROR_ACCESS_DENIED`, so this loop deliberately
    // never holds more than one instance open at a time itself (separate
    // from the cross-process race `create_pipe_instance` handles above).
    let mut first_instance = true;

    loop {
        let server = create_pipe_instance(&name, first_instance).await;
        first_instance = false;

        if server.connect().await.is_err() {
            continue;
        }

        let (reader, mut writer) = tokio::io::split(server);
        let mut lines = BufReader::new(reader).lines();
        let Ok(Some(line)) = lines.next_line().await else {
            continue;
        };
        let line = line.trim();

        if line == STOP_COMMAND {
            let _ = writer.write_all(ACK_HANDOFF_STARTED.as_bytes()).await;
            let _ = writer.write_all(b"\n").await;
            return AdminOutcome::Stop;
        }

        if line != RESTART_COMMAND {
            continue;
        }

        // Resolved fresh, right here, rather than once at process boot —
        // `storage/releases/current` may well have been updated *after*
        // this process started but *before* this particular `RESTART`
        // arrived (exactly the case a real deploy-then-restart, or `xr
        // dev`'s own build-then-restart loop, produces), and a
        // long-running process must always respawn whatever the pointer
        // currently says, not whatever it said when this process itself
        // booted. Confirmed as a real, previously-broken bug via a
        // regression test before this fix — see `docs/GOTCHAS.md`.
        let binary_path = match handoff::resolve_binary_path() {
            Ok(path) => path,
            Err(_) => {
                let _ = writer.write_all(ACK_HANDOFF_FAILED.as_bytes()).await;
                let _ = writer.write_all(b"\n").await;
                continue;
            }
        };

        match handoff::spawn_replacement_and_wait_for_ready(listener, &binary_path, ready_timeout)
            .await
        {
            Ok(Some(child)) => {
                let _ = writer.write_all(ACK_HANDOFF_STARTED.as_bytes()).await;
                let _ = writer.write_all(b"\n").await;
                return AdminOutcome::Handoff(Box::new(child));
            }
            _ => {
                let _ = writer.write_all(ACK_HANDOFF_FAILED.as_bytes()).await;
                let _ = writer.write_all(b"\n").await;
            }
        }
    }
}
