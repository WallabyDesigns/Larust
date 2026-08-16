use super::{
    AdminOutcome, ACK_HANDOFF_FAILED, ACK_HANDOFF_STARTED, RELOAD_ASSETS_COMMAND, RESTART_COMMAND,
    STOP_COMMAND,
};
use crate::lifecycle::handoff;
use std::net::TcpListener as StdTcpListener;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

fn socket_path(address: &str) -> PathBuf {
    std::env::temp_dir().join(format!("{address}.sock"))
}

pub(super) async fn run_until_command(
    address: &str,
    listener: &StdTcpListener,
    ready_timeout: Duration,
) -> AdminOutcome {
    let path = socket_path(address);
    // A stale socket file from a previous run that didn't exit cleanly
    // (killed rather than gracefully stopped) would otherwise make
    // `UnixListener::bind` fail with "address already in use" even though
    // nothing is actually listening on it anymore.
    let _ = std::fs::remove_file(&path);
    let admin_listener = UnixListener::bind(&path)
        .unwrap_or_else(|source| panic!("failed to bind admin channel at {path:?}: {source}"));

    loop {
        let Ok((stream, _)) = admin_listener.accept().await else {
            continue;
        };
        let (reader, mut writer) = tokio::io::split(stream);
        let mut lines = BufReader::new(reader).lines();
        let Ok(Some(line)) = lines.next_line().await else {
            continue;
        };
        let line = line.trim();

        if line == STOP_COMMAND {
            let _ = writer.write_all(ACK_HANDOFF_STARTED.as_bytes()).await;
            let _ = writer.write_all(b"\n").await;
            let _ = std::fs::remove_file(&path);
            return AdminOutcome::Stop;
        }

        if line == RELOAD_ASSETS_COMMAND {
            crate::dev_reload::broadcast_asset_reload();
            let _ = writer.write_all(ACK_HANDOFF_STARTED.as_bytes()).await;
            let _ = writer.write_all(b"\n").await;
            continue;
        }

        if line != RESTART_COMMAND {
            continue;
        }

        // Resolved fresh, right here — see the matching comment in
        // `windows.rs`'s own `run_until_command` for why.
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
                let _ = std::fs::remove_file(&path);
                return AdminOutcome::Handoff(Box::new(child));
            }
            _ => {
                let _ = writer.write_all(ACK_HANDOFF_FAILED.as_bytes()).await;
                let _ = writer.write_all(b"\n").await;
            }
        }
    }
}
